//! Live input level: `parecord` reads the microphone and a reader thread reports the peak of each 50 ms chunk.
//! It runs only while the window shows the Ready state. The daemon never listens to audio.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex};

use super::model::FLOOR_DB;

pub const RATE: u32 = 16_000;
/// 50 ms of 16 bit mono.
pub const CHUNK_BYTES: usize = (RATE as usize / 20) * 2;
const NONE: i32 = i32::MIN;

/// The WirePlumber fix feeds the microphone into a null sink; only its monitor delivers samples to a second
/// listener. Otherwise any source with "wave_xlr" in its name that is not a monitor.
pub fn pick_source(list: &str) -> Option<String> {
    let names: Vec<&str> = list.lines().filter_map(|line| line.split('\t').nth(1)).collect();
    if let Some(name) = names.iter().find(|n| **n == "null-sink-for-wavexlr-source.monitor") {
        return Some((*name).to_string());
    }
    names
        .iter()
        .find(|n| n.to_lowercase().contains("wave_xlr") && !n.ends_with(".monitor"))
        .map(|n| (*n).to_string())
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
    child: Mutex<Option<Child>>,
}

pub struct LiveMeter {
    shared: Arc<Shared>,
}

impl LiveMeter {
    /// Start listening. `wake` is called when the level changed enough to show (about one segment).
    pub fn start(wake: impl Fn() + Send + 'static) -> LiveMeter {
        let shared = Arc::new(Shared { level: AtomicI32::new(NONE), stop: AtomicBool::new(false), child: Mutex::new(None) });
        let thread_shared = Arc::clone(&shared);
        std::thread::spawn(move || run(&thread_shared, wake));
        LiveMeter { shared }
    }

    /// The loudest level since the last call, if any chunk arrived.
    pub fn take(&self) -> Option<f64> {
        let raw = self.shared.level.swap(NONE, Ordering::AcqRel);
        (raw != NONE).then(|| f64::from(raw) / 100.0)
    }

    pub fn stop(&self) {
        self.shared.stop.store(true, Ordering::Release);
        if let Some(mut child) = self.shared.child.lock().unwrap().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for LiveMeter {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run(shared: &Shared, wake: impl Fn()) {
    let Some(source) = find_source() else { return };
    if shared.stop.load(Ordering::Acquire) {
        return;
    }
    let spawned = Command::new("parecord")
        .arg(format!("--device={source}"))
        .arg(format!("--rate={RATE}"))
        .args(["--channels=1", "--format=s16le", "--raw", "--latency-msec=50"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = spawned else { return };
    let Some(mut out) = child.stdout.take() else { return };
    {
        let mut slot = shared.child.lock().unwrap();
        if shared.stop.load(Ordering::Acquire) {
            let _ = child.kill();
            let _ = child.wait();
            return;
        }
        *slot = Some(child);
    }
    let mut chunk = [0u8; CHUNK_BYTES];
    let mut shown = i32::MIN;
    while out.read_exact(&mut chunk).is_ok() {
        let fixed = (dbfs(&chunk) * 100.0).round() as i32;
        shared.level.fetch_max(fixed, Ordering::AcqRel);
        // Wake the UI only when the level moved by about a segment, so a quiet room costs no frames.
        if (fixed - shown).abs() >= 100 {
            shown = fixed;
            wake();
        }
    }
    // parecord ended (killed by `stop`, or it died). Show silence and reap it.
    shared.level.fetch_max((FLOOR_DB * 100.0) as i32, Ordering::AcqRel);
    wake();
    if let Some(mut child) = shared.child.lock().unwrap().take() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn find_source() -> Option<String> {
    let out = Command::new("pactl").args(["list", "short", "sources"]).stderr(Stdio::null()).output().ok()?;
    if !out.status.success() {
        return None;
    }
    pick_source(&String::from_utf8_lossy(&out.stdout))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pcm(values: &[i16]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_le_bytes()).collect()
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
    }

    #[test]
    fn picks_the_null_sink_monitor_first() {
        let list = "1\talsa_input.usb-Elgato_Wave_XLR-00.mono-fallback\tPipeWire\ts16le 1ch 48000Hz\tSUSPENDED\n\
                    2\tnull-sink-for-wavexlr-source.monitor\tPipeWire\ts16le 2ch 48000Hz\tRUNNING\n";
        assert_eq!(pick_source(list).as_deref(), Some("null-sink-for-wavexlr-source.monitor"));
    }

    #[test]
    fn falls_back_to_a_wave_xlr_source_that_is_not_a_monitor() {
        let list = "1\talsa_output.pci.analog-stereo.monitor\tPipeWire\ts16le 2ch\tIDLE\n\
                    2\talsa_input.usb-Elgato_Systems_Wave_XLR-00.mono-fallback\tPipeWire\ts16le 1ch\tIDLE\n\
                    3\talsa_output.usb-Elgato_Systems_Wave_XLR-00.analog-stereo.monitor\tPipeWire\ts16le 2ch\tIDLE\n";
        assert_eq!(
            pick_source(list).as_deref(),
            Some("alsa_input.usb-Elgato_Systems_Wave_XLR-00.mono-fallback")
        );
    }

    #[test]
    fn no_source_means_no_meter() {
        assert_eq!(pick_source("1\talsa_input.pci.analog-stereo\tPipeWire\ts16le 2ch\tIDLE\n"), None);
        assert_eq!(pick_source(""), None);
    }
}
