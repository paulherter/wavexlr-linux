//! The resident daemon: one loop that ticks the controller, serves the Unix socket, runs the tray and owns the
//! window process. All state lives on the loop's thread; the other threads only post events to it.

use crate::autostart::{self, Change};
use crate::controller::Controller;
use crate::ipc::{self, Command, DeviceInfoDto, LevelsDto, SettingsDto, Snapshot};
use crate::protocol::Levels;
use crate::settings;
use crate::transport::Hardware;
use crate::tray::{Actions, ThemeWatch, Tray, TrayAction, TrayView};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

const TICK_FAST: f64 = 0.25;
/// While the gain lock is on: a dial turn is put back within about this long (vendor reads only).
const TICK_LOCK: f64 = 0.05;
/// Ready, restore guard over, nobody watching: one look per second is enough.
const TICK_IDLE: f64 = 1.0;
const RESUME_GAP_SECONDS: f64 = 3.0;
const THEME_CHECK_SECONDS: f64 = 4.0;
const TRAY_RETRY_SECONDS: f64 = 10.0;
const MAX_LINE: u64 = 64 * 1024;

pub struct Options {
    pub tray: bool,
    /// Open the window right after start (a plain `wavexlr` launch, not the autostart one).
    pub show_window: bool,
}

enum Event {
    Connected(u64, UnixStream),
    Gone(u64),
    Command(Command),
    Tray(TrayAction),
}

// --- waking the loop ---------------------------------------------------------------------------

static WAKE_WRITE: AtomicI32 = AtomicI32::new(-1);
static STOP: AtomicBool = AtomicBool::new(false);

/// Pokes the loop out of its sleep. Only calls write(2), so it is fine inside a signal handler.
fn wake() {
    let fd = WAKE_WRITE.load(Ordering::Relaxed);
    if fd >= 0 {
        // SAFETY: a valid pipe fd and a one byte buffer; a full pipe (EAGAIN) means the loop wakes anyway.
        unsafe { libc::write(fd, [1u8].as_ptr().cast(), 1) };
    }
}

extern "C" fn on_signal(_: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
    wake();
}

/// Returns the read end of the wake-up pipe and installs the SIGTERM/SIGINT handlers.
fn setup_signals() -> std::io::Result<i32> {
    let mut fds = [0i32; 2];
    // SAFETY: `fds` has room for the two descriptors pipe2 writes.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_NONBLOCK | libc::O_CLOEXEC) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    WAKE_WRITE.store(fds[1], Ordering::Relaxed);
    for signal in [libc::SIGTERM, libc::SIGINT] {
        // SAFETY: the handler only touches an atomic and calls write(2).
        unsafe { libc::signal(signal, on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t) };
    }
    Ok(fds[0])
}

/// Sleeps until the wake-up pipe is poked or `timeout` passes, then empties the pipe.
fn sleep_until_woken(read_fd: i32, timeout: Duration) {
    let mut poll = libc::pollfd { fd: read_fd, events: libc::POLLIN, revents: 0 };
    let millis = timeout.as_millis().min(i32::MAX as u128) as i32;
    // SAFETY: one valid pollfd.
    unsafe { libc::poll(&mut poll, 1, millis) };
    let mut buffer = [0u8; 64];
    // SAFETY: reads into a local buffer from a non-blocking fd.
    while unsafe { libc::read(read_fd, buffer.as_mut_ptr().cast(), buffer.len()) } > 0 {}
}

// --- resume from sleep -------------------------------------------------------------------------

fn clock(id: libc::clockid_t) -> f64 {
    let mut spec = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: valid out pointer, and both clocks exist on Linux.
    unsafe { libc::clock_gettime(id, &mut spec) };
    spec.tv_sec as f64 + spec.tv_nsec as f64 / 1e9
}

/// Detects a wake-up from sleep: CLOCK_BOOTTIME keeps running during suspend, CLOCK_MONOTONIC does not.
struct ResumeWatch {
    offset: f64,
}

impl ResumeWatch {
    fn read() -> f64 {
        clock(libc::CLOCK_BOOTTIME) - clock(libc::CLOCK_MONOTONIC)
    }

    fn new() -> Self {
        ResumeWatch { offset: Self::read() }
    }

    fn resumed(&mut self) -> bool {
        let offset = Self::read();
        let jumped = offset - self.offset > RESUME_GAP_SECONDS;
        self.offset = offset;
        jumped
    }
}

// --- single instance and the socket ------------------------------------------------------------

fn send_line(stream: &mut UnixStream, command: &Command) -> std::io::Result<()> {
    let mut line = serde_json::to_string(command).map_err(std::io::Error::other)?;
    line.push('\n');
    stream.write_all(line.as_bytes())
}

/// Asks the daemon that is already running to open its window. Waits a moment for its socket to appear.
fn ask_running_daemon(path: &Path, command: Option<&Command>) -> bool {
    for _ in 0..10 {
        if let Ok(mut stream) = UnixStream::connect(path) {
            if let Some(command) = command {
                let _ = send_line(&mut stream, command);
            }
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

/// Takes the lock and binds the socket, or tells the running daemon to show its window and returns the exit code.
fn claim_instance(show_second_launch: bool) -> Result<(UnixListener, std::fs::File), i32> {
    let path = ipc::socket_path();
    let lock_path = path.with_file_name("wavexlr-daemon.lock"); // not wavexlr.lock: the Python app uses that name
    let lock = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(&lock_path).map_err(|error| {
        eprintln!("wavexlr: cannot open {}: {error}", lock_path.display());
        1
    })?;
    // SAFETY: valid fd. The lock lives as long as the file is open, so it is released when the process ends.
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let command = show_second_launch.then_some(Command::ShowWindow);
        if ask_running_daemon(&path, command.as_ref()) {
            return Err(0);
        }
        eprintln!("wavexlr: another copy holds the lock but does not answer on {}", path.display());
        return Err(1);
    }
    let _ = std::fs::remove_file(&path); // left over from a crashed run: we hold the lock, so nobody serves it
    let listener = UnixListener::bind(&path).map_err(|error| {
        eprintln!("wavexlr: cannot bind {}: {error}", path.display());
        1
    })?;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    Ok((listener, lock))
}

fn accept_clients(listener: UnixListener, events: Sender<Event>) {
    let mut next_id = 0u64;
    for stream in listener.incoming().flatten() {
        next_id += 1;
        let (id, events) = (next_id, events.clone());
        let _ = std::thread::Builder::new().name("client".into()).stack_size(128 * 1024).spawn(move || serve_client(id, stream, events));
    }
}

fn serve_client(id: u64, stream: UnixStream, events: Sender<Event>) {
    let Ok(writer) = stream.try_clone() else { return };
    let _ = writer.set_write_timeout(Some(Duration::from_millis(200)));
    if events.send(Event::Connected(id, writer)).is_err() {
        return;
    }
    wake();
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.by_ref().take(MAX_LINE).read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) if !line.ends_with('\n') => break, // longer than allowed, or the peer closed mid-line
            Ok(_) => {
                if let Ok(command) = serde_json::from_str::<Command>(&line) {
                    if events.send(Event::Command(command)).is_err() {
                        break;
                    }
                    wake();
                }
            }
        }
    }
    let _ = events.send(Event::Gone(id));
    wake();
}

// --- the window process ------------------------------------------------------------------------

/// After an update the running binary's path reads "... (deleted)"; the new file sits at the plain path.
fn without_deleted_suffix(path: PathBuf) -> PathBuf {
    match path.to_str().and_then(|p| p.strip_suffix(" (deleted)")) {
        Some(plain) => PathBuf::from(plain),
        None => path,
    }
}

fn own_executable() -> PathBuf {
    without_deleted_suffix(std::env::current_exe().unwrap_or_else(|_| PathBuf::from("wavexlr")))
}

struct Window {
    child: Option<Child>,
}

impl Window {
    fn running(&mut self) -> bool {
        match self.child.as_mut().map(Child::try_wait) {
            Some(Ok(None)) => true,
            Some(_) => {
                self.child = None;
                false
            }
            None => false,
        }
    }

    fn open(&mut self) {
        if self.running() {
            return;
        }
        // Own process group, so closing the window also ends helpers it started (the meter's parecord).
        match std::process::Command::new(own_executable()).arg("window").process_group(0).spawn() {
            Ok(child) => self.child = Some(child),
            Err(error) => eprintln!("wavexlr: cannot start the window: {error}"),
        }
    }

    fn close(&mut self) {
        let Some(mut child) = self.child.take() else { return };
        let group = -(child.id() as i32);
        // SAFETY: signalling the process group we created for the child.
        unsafe { libc::kill(group, libc::SIGTERM) };
        for _ in 0..20 {
            if !matches!(child.try_wait(), Ok(None)) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        // SAFETY: as above.
        unsafe { libc::kill(group, libc::SIGKILL) };
        let _ = child.wait();
    }

    fn toggle(&mut self) {
        if self.running() {
            self.close();
        } else {
            self.open();
        }
    }
}

// --- the loop ----------------------------------------------------------------------------------

impl From<Levels> for LevelsDto {
    fn from(l: Levels) -> Self {
        LevelsDto {
            gain_db: l.gain_db,
            headphone_db: l.headphone_db,
            muted: l.muted,
            phantom: l.phantom,
            low_z: l.low_z,
            clipguard: l.clipguard,
            gain_lock: l.gain_lock,
            mic_mix_percent: l.mic_mix_percent,
        }
    }
}

struct Daemon {
    controller: Controller<Hardware>,
    clients: Vec<(u64, UnixStream)>,
    window: Window,
    tray: Option<Tray>,
    tray_wanted: bool,
    tray_retry_at: f64,
    actions: Actions,
    theme: ThemeWatch,
    theme_check_at: f64,
    shown: Option<(TrayView, bool)>,
    published_revision: u64,
    /// "Start at login" and "Start minimised", as the autostart entry says. Read on connect and after a change.
    settings: SettingsDto,
    /// A setting changed since the last snapshot went out.
    settings_changed: bool,
    log: bool,
}

impl Daemon {
    fn snapshot(&self) -> Snapshot {
        let c = &self.controller;
        let device = c.device_info().map(|i| DeviceInfoDto { name: i.name, firmware: i.firmware, serial: i.serial });
        Snapshot { state: c.state(), error: c.error().to_string(), levels: c.levels().map(Into::into), device, settings: self.settings }
    }

    fn view(&self) -> TrayView {
        let c = &self.controller;
        TrayView { state: c.state(), levels: c.levels().map(|l| (l.gain_db, l.headphone_db, l.muted)) }
    }

    fn handle(&mut self, event: Event, now: f64) {
        match event {
            Event::Connected(id, mut stream) => {
                self.settings = autostart::read();
                if send_snapshot(&mut stream, &self.snapshot()) {
                    self.clients.push((id, stream));
                }
            }
            Event::Gone(id) => self.clients.retain(|(client, _)| *client != id),
            Event::Command(command) => self.command(command, now),
            Event::Tray(TrayAction::ToggleWindow) => self.window.toggle(),
            Event::Tray(TrayAction::OpenWindow) => self.window.open(),
            Event::Tray(TrayAction::SetMuted(on)) => self.controller.set_muted(on),
            Event::Tray(TrayAction::Quit) => {
                STOP.store(true, Ordering::Relaxed);
            }
        }
    }

    /// Commands act only in the Ready state, like the controller does.
    fn command(&mut self, command: Command, now: f64) {
        let c = &mut self.controller;
        match command {
            Command::SetGain { value } => c.set_gain(value),
            Command::SetHeadphone { value } => c.set_headphone(value),
            Command::SetMuted { value } => c.set_muted(value),
            Command::SetPhantom { value } => c.set_phantom(value),
            Command::SetLowZ { value } => c.set_low_z(value),
            Command::SetClipguard { value } => c.set_clipguard(value),
            Command::SetGainLock { value } => c.set_gain_lock(value),
            Command::SetMicMix { value } => c.set_mic_mix(value),
            Command::SetStartAtLogin { value } => self.change_setting(Change::StartAtLogin(value)),
            Command::SetStartMinimised { value } => self.change_setting(Change::StartMinimised(value)),
            Command::Retry => c.restore_again(now),
            Command::ShowWindow => self.window.open(),
        }
    }

    /// Writes the autostart entry, reads it back and lets the window know (also after a failure, so it shows the truth).
    fn change_setting(&mut self, change: Change) {
        if let Err(error) = autostart::change(change, &own_executable()) {
            eprintln!("wavexlr: cannot write the autostart entry: {error}");
        }
        self.settings = autostart::read();
        self.settings_changed = true;
    }

    fn tick(&mut self, now: f64, resume: &mut ResumeWatch) {
        if resume.resumed() {
            self.controller.restore_again(now);
        }
        self.controller.tick(now);
        self.window.running(); // reap a closed window
        if self.tray_wanted {
            self.check_tray(now);
        }
    }

    fn check_tray(&mut self, now: f64) {
        if self.tray.is_none() && now >= self.tray_retry_at {
            self.tray_retry_at = now + TRAY_RETRY_SECONDS;
            let dark = self.theme.dark_desktop();
            match Tray::spawn(self.view(), dark, self.actions.clone()) {
                Ok(tray) => {
                    self.shown = Some((self.view(), dark));
                    self.tray = Some(tray);
                }
                Err(error) => eprintln!("wavexlr: no tray icon yet: {error}"),
            }
        }
        if self.tray.is_some() && now >= self.theme_check_at {
            self.theme_check_at = now + THEME_CHECK_SECONDS;
            self.update_tray();
        }
    }

    fn update_tray(&mut self) {
        let Some(tray) = &self.tray else { return };
        let wanted = (self.view(), self.theme.dark_desktop());
        if self.shown != Some(wanted) {
            tray.update(wanted.0, wanted.1);
            self.shown = Some(wanted);
        }
    }

    /// After anything that can change what viewers see: tell the window and the tray.
    fn publish(&mut self) {
        let revision = self.controller.revision();
        if revision == self.published_revision && !self.settings_changed {
            return;
        }
        self.published_revision = revision;
        self.settings_changed = false;
        let snapshot = self.snapshot();
        if self.log {
            eprintln!("wavexlr: {}", serde_json::to_string(&snapshot).unwrap_or_default());
        }
        self.clients.retain_mut(|(_, stream)| send_snapshot(stream, &snapshot));
        self.update_tray();
    }

    fn interval(&self, now: f64) -> f64 {
        if self.controller.wants_fast_poll() {
            TICK_LOCK
        } else if self.clients.is_empty() && self.controller.settled(now) {
            TICK_IDLE
        } else {
            TICK_FAST
        }
    }
}

/// One JSON line. False if the client is gone or stuck.
fn send_snapshot(stream: &mut UnixStream, snapshot: &Snapshot) -> bool {
    let Ok(mut line) = serde_json::to_string(snapshot) else { return false };
    line.push('\n');
    stream.write_all(line.as_bytes()).is_ok()
}

pub fn run(options: Options) -> i32 {
    let (listener, _lock) = match claim_instance(options.show_window) {
        Ok(claimed) => claimed,
        Err(code) => return code,
    };
    let wake_fd = match setup_signals() {
        Ok(fd) => fd,
        Err(error) => {
            eprintln!("wavexlr: cannot set up signals: {error}");
            return 1;
        }
    };
    let (sender, receiver): (Sender<Event>, Receiver<Event>) = mpsc::channel();
    {
        let sender = sender.clone();
        if let Err(error) = std::thread::Builder::new().name("accept".into()).stack_size(128 * 1024).spawn(move || accept_clients(listener, sender)) {
            eprintln!("wavexlr: cannot start the socket thread: {error}");
            return 1;
        }
    }
    let actions: Actions = Arc::new(move |action| {
        let _ = sender.send(Event::Tray(action));
        wake();
    });
    let store = Box::new(|saved: &settings::Saved| {
        if let Err(error) = settings::save(saved) {
            eprintln!("wavexlr: cannot save the levels: {error}");
        }
    });
    let mut daemon = Daemon {
        controller: Controller::new(Hardware::new(), settings::load(), store),
        clients: Vec::new(),
        window: Window { child: None },
        tray: None,
        tray_wanted: options.tray,
        tray_retry_at: 0.0,
        actions,
        theme: ThemeWatch::new(),
        theme_check_at: 0.0,
        shown: None,
        published_revision: 0,
        settings: autostart::read(),
        settings_changed: false,
        log: std::env::var_os("WAVEXLR_LOG").is_some(),
    };
    let start = Instant::now();
    let mut resume = ResumeWatch::new();
    let mut next_tick = 0.0;
    if options.show_window {
        daemon.window.open();
    }

    while !STOP.load(Ordering::Relaxed) {
        let now = start.elapsed().as_secs_f64();
        sleep_until_woken(wake_fd, Duration::from_secs_f64((next_tick - now).max(0.0)));
        if STOP.load(Ordering::Relaxed) {
            break;
        }
        let now = start.elapsed().as_secs_f64();
        let before = daemon.clients.len();
        while let Ok(event) = receiver.try_recv() {
            daemon.handle(event, now);
        }
        if now >= next_tick || (before == 0 && !daemon.clients.is_empty()) {
            daemon.tick(now, &mut resume);
            next_tick = now + daemon.interval(now);
        }
        daemon.publish();
    }

    daemon.window.close();
    if let Some(tray) = daemon.tray.take() {
        tray.shutdown();
    }
    let _ = std::fs::remove_file(ipc::socket_path());
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_line_is_what_the_window_expects() {
        let snapshot = Snapshot {
            state: crate::ipc::StateName::Ready,
            error: String::new(),
            levels: Some(LevelsDto {
                gain_db: 36.75,
                headphone_db: -28.0,
                muted: false,
                phantom: true,
                low_z: true,
                clipguard: true,
                gain_lock: true,
                mic_mix_percent: 42.0,
            }),
            device: Some(DeviceInfoDto { name: "Wave XLR".into(), firmware: "1.3.5".into(), serial: "ABC".into() }),
            settings: crate::ipc::SettingsDto::default(),
        };
        let line = serde_json::to_string(&snapshot).unwrap();
        assert_eq!(serde_json::from_str::<Snapshot>(&line).unwrap(), snapshot);
        assert!(line.contains("\"state\":\"ready\""));
    }

    #[test]
    fn commands_parse_from_json_lines() {
        let command: Command = serde_json::from_str("{\"cmd\":\"set_gain\",\"value\":40.5}").unwrap();
        assert_eq!(command, Command::SetGain { value: 40.5 });
        let command: Command = serde_json::from_str("{\"cmd\":\"show_window\"}").unwrap();
        assert_eq!(command, Command::ShowWindow);
        let command: Command = serde_json::from_str("{\"cmd\":\"set_mic_mix\",\"value\":35.0}").unwrap();
        assert_eq!(command, Command::SetMicMix { value: 35.0 });
        let command: Command = serde_json::from_str("{\"cmd\":\"set_clipguard\",\"value\":false}").unwrap();
        assert_eq!(command, Command::SetClipguard { value: false });
        let command: Command = serde_json::from_str("{\"cmd\":\"set_gain_lock\",\"value\":true}").unwrap();
        assert_eq!(command, Command::SetGainLock { value: true });
    }

    #[test]
    fn an_old_snapshot_without_the_new_fields_still_parses() {
        let old = r#"{"state":"ready","error":"","levels":{"gain_db":38.5,"headphone_db":-23.5,"muted":false,"phantom":false,"low_z":false}}"#;
        let snapshot: Snapshot = serde_json::from_str(old).unwrap();
        let levels = snapshot.levels.unwrap();
        assert!(!levels.clipguard && !levels.gain_lock && levels.mic_mix_percent == 0.0);
        assert!(snapshot.device.is_none());
    }

    #[test]
    fn resume_watch_stays_quiet_while_awake() {
        let mut watch = ResumeWatch::new();
        assert!(!watch.resumed());
    }

    #[test]
    fn deleted_binary_suffix_is_stripped() {
        assert_eq!(without_deleted_suffix(PathBuf::from("/usr/bin/wavexlr (deleted)")), PathBuf::from("/usr/bin/wavexlr"));
        assert_eq!(without_deleted_suffix(PathBuf::from("/usr/bin/wavexlr")), PathBuf::from("/usr/bin/wavexlr"));
    }
}
