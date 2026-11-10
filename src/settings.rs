//! Saved levels, one JSON file per user (same file and keys as the Python version).
//! Written atomically so a crash never leaves half a file.

use crate::protocol;
use serde::Serialize;
use serde_json::Value;
use std::path::{Path, PathBuf};

pub fn config_path() -> PathBuf {
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"),
    };
    base.join("wavexlr").join("settings.json")
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Saved {
    pub gain_db: f64,
    pub headphone_db: f64,
    pub muted: bool,
    /// `None` = the user never set it in the app, so the app leaves it alone.
    pub phantom: Option<bool>,
    pub low_z: Option<bool>,
    /// Same rule as `phantom`: `None` = never set in the app, so it is left alone.
    pub clipguard: Option<bool>,
    pub gain_lock: Option<bool>,
    /// Percent of the microphone in the headphone mix, 0..=100.
    pub mic_mix: Option<f64>,
}

impl Saved {
    pub fn new(gain_db: f64, headphone_db: f64, muted: bool) -> Self {
        Saved { gain_db, headphone_db, muted, phantom: None, low_z: None, clipguard: None, gain_lock: None, mic_mix: None }
    }
}

/// Python's `bool(x)` for the JSON values the old app could have written.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|x| x != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

fn number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        Value::Bool(b) => Some(f64::from(u8::from(*b))),
        _ => None,
    }
}

pub fn parse(text: &str) -> Option<Saved> {
    let data: Value = serde_json::from_str(text).ok()?;
    let optional = |key: &str| data.get(key).filter(|v| !v.is_null()).map(truthy);
    Some(Saved {
        gain_db: number(data.get("gain_db")?)?,
        headphone_db: number(data.get("headphone_db")?)?,
        muted: data.get("muted").is_some_and(truthy),
        phantom: optional("phantom"),
        low_z: optional("low_z"),
        clipguard: optional("clipguard"),
        gain_lock: optional("gain_lock"),
        mic_mix: data.get("mic_mix").and_then(number).map(protocol::clamp_mic_mix),
    })
}

pub fn load_from(path: &Path) -> Option<Saved> {
    parse(&std::fs::read_to_string(path).ok()?)
}

pub fn load() -> Option<Saved> {
    load_from(&config_path())
}

pub fn save_to(saved: &Saved, path: &Path) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let temporary = path.with_extension("tmp");
    let mut text = serde_json::to_string_pretty(saved).map_err(std::io::Error::other)?;
    text.push('\n');
    std::fs::write(&temporary, text)?;
    std::fs::rename(&temporary, path)
}

pub fn save(saved: &Saved) -> std::io::Result<()> {
    save_to(saved, &config_path())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wavexlr-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn round_trip_keeps_every_key() {
        let dir = temp_dir("round");
        let path = dir.join("wavexlr").join("settings.json");
        let saved = Saved { gain_db: 36.75, headphone_db: -28.0, muted: true, phantom: Some(true), low_z: None, clipguard: Some(false), gain_lock: None, mic_mix: Some(40.0) };
        save_to(&saved, &path).unwrap();
        assert_eq!(load_from(&path), Some(saved));
        assert!(!path.with_extension("tmp").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_looks_like_the_python_one() {
        let dir = temp_dir("format");
        let path = dir.join("settings.json");
        save_to(&Saved::new(30.0, -35.0, false), &path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            text,
            "{\n  \"gain_db\": 30.0,\n  \"headphone_db\": -35.0,\n  \"muted\": false,\n  \"phantom\": null,\n  \"low_z\": null,\n  \"clipguard\": null,\n  \"gain_lock\": null,\n  \"mic_mix\": null\n}\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reads_the_existing_python_file() {
        let text = "{\n  \"gain_db\": 36.75,\n  \"headphone_db\": -28.0,\n  \"muted\": false,\n  \"phantom\": true,\n  \"low_z\": true\n}\n";
        let saved = parse(text).unwrap();
        assert_eq!(saved, Saved { phantom: Some(true), low_z: Some(true), ..Saved::new(36.75, -28.0, false) });
    }

    #[test]
    fn missing_keys_and_garbage_give_none() {
        assert_eq!(parse("{\"gain_db\": 1.0}"), None);
        assert_eq!(parse("not json"), None);
        assert_eq!(parse("[]"), None);
        assert_eq!(load_from(Path::new("/nonexistent/wavexlr.json")), None);
    }

    #[test]
    fn optional_keys_default_like_python() {
        let saved = parse("{\"gain_db\": 10, \"headphone_db\": -20}").unwrap();
        assert_eq!(saved, Saved::new(10.0, -20.0, false));
    }

    #[test]
    fn new_keys_are_read_and_old_files_still_load() {
        let text = "{\"gain_db\": 30, \"headphone_db\": -20, \"clipguard\": false, \"gain_lock\": true, \"mic_mix\": 45.0}";
        let saved = parse(text).unwrap();
        assert_eq!((saved.clipguard, saved.gain_lock, saved.mic_mix), (Some(false), Some(true), Some(45.0)));
        // Without the keys, and with null, nothing is pinned.
        let saved = parse("{\"gain_db\": 30, \"headphone_db\": -20, \"mic_mix\": null}").unwrap();
        assert_eq!((saved.clipguard, saved.gain_lock, saved.mic_mix), (None, None, None));
        // Out of range is clamped, unknown keys are ignored.
        let saved = parse("{\"gain_db\": 30, \"headphone_db\": -20, \"mic_mix\": 250, \"future\": 1}").unwrap();
        assert_eq!(saved.mic_mix, Some(100.0));
    }
}
