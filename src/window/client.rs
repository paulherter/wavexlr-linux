//! The window's side of the daemon socket: one reader thread that connects (and reconnects), turns lines into
//! events, and a `send` that writes a command line. No async runtime; the thread blocks on the socket.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::ipc::{Command, Snapshot};

pub enum Event {
    Snapshot(Snapshot),
    /// No daemon on the socket, or it went away. Sent once per outage.
    Disconnected,
}

pub fn parse_snapshot(line: &str) -> Result<Snapshot, serde_json::Error> {
    serde_json::from_str(line.trim())
}

pub fn encode_command(command: &Command) -> String {
    let mut line = serde_json::to_string(command).unwrap_or_default();
    line.push('\n');
    line
}

pub const RETRY: Duration = Duration::from_millis(400);

pub struct Client {
    writer: Arc<Mutex<Option<UnixStream>>>,
}

impl Client {
    /// Start the reader thread. `wake` is called after every event so the UI can repaint.
    pub fn start(path: PathBuf, events: Sender<Event>, wake: impl Fn() + Send + 'static, retry: Duration) -> Client {
        let writer: Arc<Mutex<Option<UnixStream>>> = Arc::new(Mutex::new(None));
        let shared = Arc::clone(&writer);
        std::thread::spawn(move || {
            let mut announced_down = false;
            loop {
                if let Ok(stream) = UnixStream::connect(&path) {
                    announced_down = false;
                    let _ = stream.set_write_timeout(Some(Duration::from_millis(200)));
                    if let Ok(clone) = stream.try_clone() {
                        *shared.lock().unwrap() = Some(clone);
                    }
                    for line in BufReader::new(stream).lines() {
                        let Ok(line) = line else { break };
                        if line.trim().is_empty() {
                            continue;
                        }
                        match parse_snapshot(&line) {
                            Ok(snapshot) => {
                                if events.send(Event::Snapshot(snapshot)).is_err() {
                                    return;
                                }
                                wake();
                            }
                            Err(error) => eprintln!("wavexlr window: ignoring a line from the daemon: {error}"),
                        }
                    }
                    *shared.lock().unwrap() = None;
                }
                if !announced_down {
                    announced_down = true;
                    if events.send(Event::Disconnected).is_err() {
                        return;
                    }
                    wake();
                }
                std::thread::sleep(retry);
            }
        });
        Client { writer }
    }

    /// Write one command. Returns false when there is no daemon to take it.
    pub fn send(&self, command: &Command) -> bool {
        let mut guard = self.writer.lock().unwrap();
        let Some(stream) = guard.as_mut() else { return false };
        if stream.write_all(encode_command(command).as_bytes()).is_err() {
            *guard = None;
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::{LevelsDto, StateName};
    use std::os::unix::net::UnixListener;
    use std::sync::mpsc::{channel, Receiver};
    use std::sync::atomic::{AtomicUsize, Ordering};

    const SNAPSHOT: &str = r#"{"state":"ready","error":"","levels":{"gain_db":38.5,"headphone_db":-23.5,"muted":false,"phantom":false,"low_z":false}}"#;

    fn temp_socket(name: &str) -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!("wxlr-{}-{}-{name}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("wavexlr.sock")
    }

    fn next(rx: &Receiver<Event>) -> Event {
        rx.recv_timeout(Duration::from_secs(5)).expect("no event in time")
    }

    fn levels() -> LevelsDto {
        LevelsDto {
            gain_db: 38.5,
            headphone_db: -23.5,
            muted: false,
            phantom: false,
            low_z: false,
            clipguard: false,
            gain_lock: false,
            mic_mix_percent: 0.0,
        }
    }

    #[test]
    fn parses_a_snapshot_line_and_ignores_unknown_fields() {
        let snapshot = parse_snapshot(SNAPSHOT).unwrap();
        assert_eq!(snapshot.state, StateName::Ready);
        assert_eq!(snapshot.levels, Some(levels()));
        let extra = r#"{"state":"absent","error":"","levels":null,"future":1}"#;
        assert_eq!(parse_snapshot(extra).unwrap().state, StateName::Absent);
        assert!(parse_snapshot("not json").is_err());
        assert_eq!(
            parse_snapshot(r#"{"state":"error","error":"it broke","levels":null}"#).unwrap().error,
            "it broke"
        );
    }

    #[test]
    fn commands_are_one_json_line() {
        assert_eq!(encode_command(&Command::SetGain { value: 38.5 }), "{\"cmd\":\"set_gain\",\"value\":38.5}\n");
        assert_eq!(encode_command(&Command::SetMuted { value: true }), "{\"cmd\":\"set_muted\",\"value\":true}\n");
        assert_eq!(encode_command(&Command::Retry), "{\"cmd\":\"retry\"}\n");
        assert_eq!(encode_command(&Command::SetClipguard { value: true }), "{\"cmd\":\"set_clipguard\",\"value\":true}\n");
        assert_eq!(encode_command(&Command::SetGainLock { value: false }), "{\"cmd\":\"set_gain_lock\",\"value\":false}\n");
        assert_eq!(encode_command(&Command::SetMicMix { value: 40.0 }), "{\"cmd\":\"set_mic_mix\",\"value\":40.0}\n");
        assert_eq!(encode_command(&Command::SetStartAtLogin { value: true }), "{\"cmd\":\"set_start_at_login\",\"value\":true}\n");
        assert_eq!(encode_command(&Command::SetStartMinimised { value: false }), "{\"cmd\":\"set_start_minimised\",\"value\":false}\n");
    }

    #[test]
    fn parses_the_device_and_the_settings_of_a_snapshot() {
        let line = r#"{"state":"ready","error":"","levels":{"gain_db":38.5,"headphone_db":-23.5,"muted":false,"phantom":false,"low_z":false,"clipguard":true,"gain_lock":true,"mic_mix_percent":40.0},"device":{"name":"Wave XLR","firmware":"1.3.5","serial":"ABC"},"settings":{"start_at_login":false,"start_minimised":true}}"#;
        let snapshot = parse_snapshot(line).unwrap();
        let levels = snapshot.levels.unwrap();
        assert!(levels.clipguard && levels.gain_lock);
        assert_eq!(levels.mic_mix_percent, 40.0);
        assert_eq!(snapshot.device.unwrap().firmware, "1.3.5");
        assert!(!snapshot.settings.start_at_login && snapshot.settings.start_minimised);
        // An older daemon sends none of it: the defaults apply.
        let old = parse_snapshot(SNAPSHOT).unwrap();
        assert!(old.device.is_none());
        assert_eq!(old.settings, Default::default());
    }

    #[test]
    fn reads_snapshots_and_sends_commands() {
        let path = temp_socket("rw");
        let listener = UnixListener::bind(&path).unwrap();
        let (tx, rx) = channel();
        let client = Client::start(path.clone(), tx, || {}, Duration::from_millis(20));

        let (mut server, _) = listener.accept().unwrap();
        server.write_all(format!("{SNAPSHOT}\n").as_bytes()).unwrap();
        match next(&rx) {
            Event::Snapshot(s) => assert_eq!(s.levels, Some(levels())),
            Event::Disconnected => panic!("expected a snapshot"),
        }

        // The client may need a moment to store its writer after connecting.
        let mut sent = false;
        for _ in 0..100 {
            if client.send(&Command::SetMuted { value: true }) {
                sent = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(sent);
        let mut line = String::new();
        BufReader::new(server.try_clone().unwrap()).read_line(&mut line).unwrap();
        assert_eq!(serde_json::from_str::<Command>(&line).unwrap(), Command::SetMuted { value: true });
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn reports_a_missing_daemon_once_and_connects_when_it_appears() {
        let path = temp_socket("late");
        let (tx, rx) = channel();
        let client = Client::start(path.clone(), tx, || {}, Duration::from_millis(20));
        assert!(matches!(next(&rx), Event::Disconnected));
        assert!(!client.send(&Command::Retry));
        // Several retries pass without a second Disconnected.
        std::thread::sleep(Duration::from_millis(120));
        let listener = UnixListener::bind(&path).unwrap();
        let (mut server, _) = listener.accept().unwrap();
        server.write_all(format!("{SNAPSHOT}\n").as_bytes()).unwrap();
        assert!(matches!(next(&rx), Event::Snapshot(_)));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn reconnects_after_the_daemon_restarts() {
        let path = temp_socket("restart");
        let listener = UnixListener::bind(&path).unwrap();
        let (tx, rx) = channel();
        let _client = Client::start(path.clone(), tx, || {}, Duration::from_millis(20));

        let (mut server, _) = listener.accept().unwrap();
        server.write_all(format!("{SNAPSHOT}\n").as_bytes()).unwrap();
        assert!(matches!(next(&rx), Event::Snapshot(_)));

        // The daemon stops: the connection and the socket go away.
        drop(server);
        drop(listener);
        let _ = std::fs::remove_file(&path);
        assert!(matches!(next(&rx), Event::Disconnected));

        // A new daemon comes up and the window picks it up again.
        let listener = UnixListener::bind(&path).unwrap();
        let (mut server, _) = listener.accept().unwrap();
        server.write_all(b"garbage\n").unwrap(); // a bad line is skipped, not fatal
        server.write_all(format!("{SNAPSHOT}\n").as_bytes()).unwrap();
        assert!(matches!(next(&rx), Event::Snapshot(_)));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
