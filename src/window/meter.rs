//! Live input level: libpulse-simple is loaded at runtime (dlopen), so the binary starts without libpulse and the
//! meter just stays empty. A reader thread reports the peak of each 50 ms chunk. It runs only while the window
//! shows the Ready state. The daemon never listens to audio.

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::model::FLOOR_DB;

pub const RATE: u32 = 16_000;
/// 50 ms of 16 bit mono.
pub const CHUNK_BYTES: usize = (RATE as usize / 20) * 2;
const NONE: i32 = i32::MIN;
/// How long `Drop` waits for the reader thread to release the stream.
const STOP_WAIT: Duration = Duration::from_millis(200);

/// A source of PCM chunks. Dropping it releases the stream. Tests use a fake.
pub trait Capture {
    /// Fill `chunk` completely, blocking until the data is there. False when the stream ended or failed.
    fn read(&mut self, chunk: &mut [u8]) -> bool;
}

/// Source names to try, best first. Never the default source: it may be another microphone.
pub fn candidates(serial: &str) -> Vec<String> {
    let mut names = vec!["null-sink-for-wavexlr-source.monitor".to_string(), "wavexlr-source".to_string()];
    if !serial.is_empty() {
        let base = format!("alsa_input.usb-Elgato_Systems_Elgato_Wave_XLR_{serial}-00");
        names.extend(["mono-fallback", "analog-stereo", "iec958-stereo"].map(|profile| format!("{base}.{profile}")));
    }
    names
}

/// The first candidate that opens, with its name.
pub fn open_first(
    names: &[String],
    stop: &AtomicBool,
    mut open: impl FnMut(&str) -> Option<Box<dyn Capture>>,
) -> Option<(String, Box<dyn Capture>)> {
    for name in names {
        if stop.load(Ordering::Acquire) {
            return None;
        }
        if let Some(capture) = open(name) {
            return Some((name.clone(), capture));
        }
    }
    None
}

/// Peak of 16 bit little endian samples in dBFS, `FLOOR_DB` when silent.
pub fn dbfs(samples: &[u8]) -> f64 {
    let peak = samples
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| i32::from(i16::from_le_bytes(*b)).abs())
        .max()
        .unwrap_or(0);
    if peak == 0 {
        return FLOOR_DB;
    }
    (20.0 * (f64::from(peak) / 32768.0).log10()).max(FLOOR_DB)
}

struct Shared {
    /// Loudest chunk since the UI last looked, in 1/100 dB.
    level: AtomicI32,
    stop: AtomicBool,
    /// Set when the reader thread has released the stream and is about to end.
    done: AtomicBool,
}

pub struct LiveMeter {
    shared: Arc<Shared>,
}

impl LiveMeter {
    /// Start listening. `wake` is called when the level changed enough to show (about one segment).
    pub fn start(wake: impl Fn() + Send + 'static) -> LiveMeter {
        let shared = Arc::new(Shared {
            level: AtomicI32::new(NONE),
            stop: AtomicBool::new(false),
            done: AtomicBool::new(false),
        });
        let thread_shared = Arc::clone(&shared);
        std::thread::spawn(move || {
            let serial = crate::transport::find_usb().map(|usb| usb.serial).unwrap_or_default();
            run(&thread_shared, &wake, &candidates(&serial), ffi::open);
        });
        LiveMeter { shared }
    }

    /// The loudest level since the last call, if any chunk arrived.
    pub fn take(&self) -> Option<f64> {
        let raw = self.shared.level.swap(NONE, Ordering::AcqRel);
        (raw != NONE).then(|| f64::from(raw) / 100.0)
    }

    /// Ask the reader thread to end. It notices after the current 50 ms chunk and frees the stream itself.
    pub fn stop(&self) {
        self.shared.stop.store(true, Ordering::Release);
    }
}

impl Drop for LiveMeter {
    fn drop(&mut self) {
        self.stop();
        let start = Instant::now();
        while !self.shared.done.load(Ordering::Acquire) && start.elapsed() < STOP_WAIT {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

/// Sets `done` when the reader thread ends, however it ends.
struct DoneGuard<'a>(&'a AtomicBool);

impl Drop for DoneGuard<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

fn run(shared: &Shared, wake: &impl Fn(), names: &[String], open: impl FnMut(&str) -> Option<Box<dyn Capture>>) {
    let _done = DoneGuard(&shared.done);
    let Some((_, capture)) = open_first(names, &shared.stop, open) else { return };
    pump(shared, wake, capture);
}

/// Read chunks until the stream ends or `stop` is set; then free the stream, show silence and wake the UI.
fn pump(shared: &Shared, wake: &impl Fn(), mut capture: Box<dyn Capture>) {
    let mut chunk = [0u8; CHUNK_BYTES];
    let mut shown = i32::MIN;
    while !shared.stop.load(Ordering::Acquire) && capture.read(&mut chunk) {
        if shared.stop.load(Ordering::Acquire) {
            break;
        }
        let fixed = (dbfs(&chunk) * 100.0).round() as i32;
        shared.level.fetch_max(fixed, Ordering::AcqRel);
        // Wake the UI only when the level moved by about a segment, so a quiet room costs no frames.
        if (fixed - shown).abs() >= 100 {
            shown = fixed;
            wake();
        }
    }
    drop(capture);
    shared.level.fetch_max((FLOOR_DB * 100.0) as i32, Ordering::AcqRel);
    wake();
}

/// `wavexlr window --meter-probe`: print the dBFS of 20 real chunks, then exit. Returns the exit code.
pub fn probe() -> i32 {
    let serial = crate::transport::find_usb().map(|usb| usb.serial).unwrap_or_default();
    let stop = AtomicBool::new(false);
    let Some((name, mut capture)) = open_first(&candidates(&serial), &stop, ffi::open) else {
        eprintln!("meter probe: no source could be opened (libpulse-simple missing or no Wave XLR source)");
        return 1;
    };
    println!("source: {name}");
    let mut chunk = [0u8; CHUNK_BYTES];
    for n in 1..=20 {
        if !capture.read(&mut chunk) {
            eprintln!("meter probe: stream ended after {} chunks", n - 1);
            return 1;
        }
        println!("chunk {n:2}: {:6.1} dBFS", dbfs(&chunk));
    }
    0
}

/// libpulse-simple, loaded at runtime. Every `unsafe` block says why it is sound.
mod ffi {
    use super::{Capture, CHUNK_BYTES, RATE};
    use std::ffi::{c_char, c_int, c_void, CStr, CString};
    use std::sync::OnceLock;

    const PA_STREAM_RECORD: c_int = 2;
    const PA_SAMPLE_S16LE: c_int = 3;
    const UNSET: u32 = u32::MAX;

    #[repr(C)]
    pub struct SampleSpec {
        pub format: c_int,
        pub rate: u32,
        pub channels: u8,
    }

    #[repr(C)]
    pub struct BufferAttr {
        pub maxlength: u32,
        pub tlength: u32,
        pub prebuf: u32,
        pub minreq: u32,
        pub fragsize: u32,
    }

    /// Mono s16le at 16 kHz.
    pub fn sample_spec() -> SampleSpec {
        SampleSpec { format: PA_SAMPLE_S16LE, rate: RATE, channels: 1 }
    }

    /// Everything default except the fragment size, which is one 50 ms chunk for low latency.
    pub fn buffer_attr() -> BufferAttr {
        BufferAttr { maxlength: UNSET, tlength: UNSET, prebuf: UNSET, minreq: UNSET, fragsize: CHUNK_BYTES as u32 }
    }

    type NewFn = unsafe extern "C" fn(
        *const c_char,
        *const c_char,
        c_int,
        *const c_char,
        *const c_char,
        *const SampleSpec,
        *const c_void,
        *const BufferAttr,
        *mut c_int,
    ) -> *mut c_void;
    type ReadFn = unsafe extern "C" fn(*mut c_void, *mut c_void, usize, *mut c_int) -> c_int;
    type FreeFn = unsafe extern "C" fn(*mut c_void);

    #[derive(Clone, Copy)]
    struct Api {
        new: NewFn,
        read: ReadFn,
        free: FreeFn,
    }

    fn load() -> Option<Api> {
        let lib = c"libpulse-simple.so.0";
        // SAFETY: dlopen with a valid NUL-terminated name; a null result is handled. The library is never closed.
        let handle = unsafe { libc::dlopen(lib.as_ptr(), libc::RTLD_NOW) };
        if handle.is_null() {
            return None;
        }
        let symbol = |name: &CStr| {
            // SAFETY: `handle` is a live dlopen handle and `name` is NUL-terminated; a null result is handled.
            let ptr = unsafe { libc::dlsym(handle, name.as_ptr()) };
            (!ptr.is_null()).then_some(ptr)
        };
        let (new, read, free) = (symbol(c"pa_simple_new")?, symbol(c"pa_simple_read")?, symbol(c"pa_simple_free")?);
        // SAFETY: these three symbols have exactly these C signatures in libpulse-simple (pulse/simple.h).
        unsafe {
            Some(Api {
                new: std::mem::transmute::<*mut c_void, NewFn>(new),
                read: std::mem::transmute::<*mut c_void, ReadFn>(read),
                free: std::mem::transmute::<*mut c_void, FreeFn>(free),
            })
        }
    }

    struct Pulse {
        api: Api,
        stream: *mut c_void,
    }

    impl Capture for Pulse {
        fn read(&mut self, chunk: &mut [u8]) -> bool {
            let mut error: c_int = 0;
            // SAFETY: `stream` is a live pa_simple (freed only in Drop) and `chunk` is writable for its length.
            unsafe { (self.api.read)(self.stream, chunk.as_mut_ptr().cast(), chunk.len(), &mut error) >= 0 }
        }
    }

    impl Drop for Pulse {
        fn drop(&mut self) {
            // SAFETY: `stream` came from pa_simple_new and is freed exactly once, here.
            unsafe { (self.api.free)(self.stream) };
        }
    }

    /// Open a mono 16 kHz recording stream on the named source. None when libpulse is missing or the source
    /// does not exist.
    pub fn open(source: &str) -> Option<Box<dyn Capture>> {
        static API: OnceLock<Option<Api>> = OnceLock::new();
        let api = (*API.get_or_init(load))?;
        let device = CString::new(source).ok()?;
        let (spec, attr) = (sample_spec(), buffer_attr());
        let mut error: c_int = 0;
        // SAFETY: all pointers are valid for the call (the C strings, `spec`, `attr`, `error`); null server and
        // channel map select the defaults; the result is checked for null.
        let stream = unsafe {
            (api.new)(
                std::ptr::null(),
                c"Wave XLR".as_ptr(),
                PA_STREAM_RECORD,
                device.as_ptr(),
                c"Level meter".as_ptr(),
                &spec,
                std::ptr::null(),
                &attr,
                &mut error,
            )
        };
        (!stream.is_null()).then(|| Box::new(Pulse { api, stream }) as Box<dyn Capture>)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;
    use std::rc::Rc;

    fn pcm(values: &[i16]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    fn shared() -> Shared {
        Shared { level: AtomicI32::new(NONE), stop: AtomicBool::new(false), done: AtomicBool::new(false) }
    }

    /// Yields the given chunks, then ends. Sets `freed` when dropped.
    struct Fake {
        chunks: VecDeque<Vec<u8>>,
        freed: Rc<Cell<bool>>,
        /// Set the shared stop flag during this read (1-based).
        stop_at: Option<(usize, Rc<Shared>)>,
        reads: usize,
    }

    impl Fake {
        fn new(chunks: impl IntoIterator<Item = Vec<u8>>) -> Fake {
            Fake { chunks: chunks.into_iter().collect(), freed: Rc::default(), stop_at: None, reads: 0 }
        }
    }

    impl Capture for Fake {
        fn read(&mut self, chunk: &mut [u8]) -> bool {
            self.reads += 1;
            if let Some((n, shared)) = &self.stop_at {
                if self.reads == *n {
                    shared.stop.store(true, Ordering::Release);
                }
            }
            match self.chunks.pop_front() {
                Some(data) => {
                    chunk.copy_from_slice(&data);
                    true
                }
                None => false,
            }
        }
    }

    impl Drop for Fake {
        fn drop(&mut self) {
            self.freed.set(true);
        }
    }

    fn chunk_of(value: i16) -> Vec<u8> {
        pcm(&vec![value; CHUNK_BYTES / 2])
    }

    #[test]
    fn dbfs_is_the_peak_in_decibels() {
        assert_eq!(dbfs(&pcm(&[0, 0, 0])), FLOOR_DB);
        assert!((dbfs(&pcm(&[0, -16384, 100])) - (-6.0206)).abs() < 1e-3);
        assert!((dbfs(&pcm(&[i16::MIN])) - 0.0).abs() < 1e-9);
        assert_eq!(dbfs(&pcm(&[1])), FLOOR_DB); // -90 dB is below the floor
        assert_eq!(dbfs(&[]), FLOOR_DB);
    }

    #[test]
    fn chunk_is_fifty_milliseconds() {
        assert_eq!(CHUNK_BYTES, 1600);
        assert_eq!(ffi::buffer_attr().fragsize, 1600);
        assert_eq!(ffi::buffer_attr().maxlength, u32::MAX);
        let spec = ffi::sample_spec();
        assert_eq!((spec.format, spec.rate, spec.channels), (3, 16_000, 1));
    }

    #[test]
    fn candidates_with_a_serial() {
        assert_eq!(
            candidates("BCA123"),
            [
                "null-sink-for-wavexlr-source.monitor",
                "wavexlr-source",
                "alsa_input.usb-Elgato_Systems_Elgato_Wave_XLR_BCA123-00.mono-fallback",
                "alsa_input.usb-Elgato_Systems_Elgato_Wave_XLR_BCA123-00.analog-stereo",
                "alsa_input.usb-Elgato_Systems_Elgato_Wave_XLR_BCA123-00.iec958-stereo",
            ]
        );
    }

    #[test]
    fn candidates_without_a_serial_skip_the_usb_names() {
        assert_eq!(candidates(""), ["null-sink-for-wavexlr-source.monitor", "wavexlr-source"]);
    }

    #[test]
    fn open_first_takes_the_first_name_that_opens_and_stops_there() {
        let tried = RefCell::new(Vec::new());
        let names = candidates("S1");
        let stop = AtomicBool::new(false);
        let (name, _) = open_first(&names, &stop, |n| {
            tried.borrow_mut().push(n.to_string());
            (n == "wavexlr-source").then(|| Box::new(Fake::new([])) as Box<dyn Capture>)
        })
        .unwrap();
        assert_eq!(name, "wavexlr-source");
        assert_eq!(*tried.borrow(), ["null-sink-for-wavexlr-source.monitor", "wavexlr-source"]);
    }

    #[test]
    fn open_first_gives_up_when_nothing_opens_or_stop_is_set() {
        let names = candidates("S1");
        let stop = AtomicBool::new(false);
        assert!(open_first(&names, &stop, |_| None).is_none());
        stop.store(true, Ordering::Release);
        let tried = Cell::new(0);
        let result = open_first(&names, &stop, |_| {
            tried.set(tried.get() + 1);
            None
        });
        assert!(result.is_none());
        assert_eq!(tried.get(), 0);
    }

    #[test]
    fn pump_keeps_the_peak_wakes_on_big_moves_and_ends_with_silence() {
        let shared = shared();
        let fake = Fake::new([chunk_of(100), chunk_of(110), chunk_of(16384), chunk_of(50)]);
        let freed = fake.freed.clone();
        let wakes = Cell::new(0);
        pump(&shared, &|| wakes.set(wakes.get() + 1), Box::new(fake));
        assert!(freed.get());
        // Peak hold: the loudest chunk (-6 dBFS) is still there after the stream ended with silence.
        let level = f64::from(shared.level.load(Ordering::Acquire)) / 100.0;
        assert!((level - (-6.02)).abs() < 0.01, "{level}");
        // Wakes: first chunk, the jump to -6 dB, the drop back, and the final silence.
        assert_eq!(wakes.get(), 4);
    }

    #[test]
    fn pump_ends_with_floor_when_nothing_was_loud() {
        let shared = shared();
        pump(&shared, &|| {}, Box::new(Fake::new([])));
        assert_eq!(shared.level.load(Ordering::Acquire), (FLOOR_DB * 100.0) as i32);
    }

    #[test]
    fn pump_stops_after_the_chunk_in_flight_and_frees_the_stream() {
        let shared = Rc::new(shared());
        let mut fake = Fake::new((0..100).map(|_| chunk_of(1000)));
        fake.stop_at = Some((3, shared.clone()));
        let freed = fake.freed.clone();
        pump(&shared, &|| {}, Box::new(fake));
        assert!(freed.get());
    }

    #[test]
    fn run_marks_done_even_when_no_source_opens() {
        let shared = shared();
        run(&shared, &|| {}, &candidates(""), |_| None);
        assert!(shared.done.load(Ordering::Acquire));
        assert_eq!(shared.level.load(Ordering::Acquire), NONE);
    }

    #[test]
    fn drop_does_not_block_on_a_stuck_read() {
        // A reader that never returns in time, like a source that delivers nothing.
        struct Stuck;
        impl Capture for Stuck {
            fn read(&mut self, _: &mut [u8]) -> bool {
                std::thread::sleep(Duration::from_secs(2));
                false
            }
        }
        let shared = Arc::new(shared());
        let thread_shared = Arc::clone(&shared);
        std::thread::spawn(move || {
            run(&thread_shared, &|| {}, &["x".to_string()], |_| Some(Box::new(Stuck) as Box<dyn Capture>));
        });
        let start = Instant::now();
        drop(LiveMeter { shared });
        assert!(start.elapsed() < Duration::from_millis(500));
    }
}
