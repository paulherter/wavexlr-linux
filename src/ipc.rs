//! Messages between the resident daemon and the window process. One JSON object per line over the Unix
//! socket `$XDG_RUNTIME_DIR/wavexlr.sock`. The daemon owns the device; the window never talks to USB itself.

use serde::{Deserialize, Serialize};

/// Device state, as the state machine in `controller.rs` sees it.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StateName {
    Absent,
    Initializing,
    Ready,
    Error,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct LevelsDto {
    pub gain_db: f64,
    pub headphone_db: f64,
    pub muted: bool,
    pub phantom: bool,
    pub low_z: bool,
    #[serde(default)]
    pub clipguard: bool,
    #[serde(default)]
    pub gain_lock: bool,
    /// 0..=100, whole steps in the UI.
    #[serde(default)]
    pub mic_mix_percent: f64,
}

/// Read-only facts about the connected device.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct DeviceInfoDto {
    pub name: String,
    /// e.g. "1.3.5"; the raw text if the device reports an unexpected format. Empty if unknown.
    pub firmware: String,
    pub serial: String,
}

/// Daemon -> window. Sent once on connect and again after every change.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub state: StateName,
    /// Plain English, empty unless `state` is `Error`.
    pub error: String,
    /// `None` unless `state` is `Ready`.
    pub levels: Option<LevelsDto>,
    /// `Some` exactly when `state` is `Ready`.
    #[serde(default)]
    pub device: Option<DeviceInfoDto>,
    /// App settings the window shows (always present).
    #[serde(default)]
    pub settings: SettingsDto,
}

/// What the "Start at login" and "Start minimised" rows show. The daemon keeps them in the autostart entry.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct SettingsDto {
    pub start_at_login: bool,
    pub start_minimised: bool,
}

impl Default for SettingsDto {
    fn default() -> Self {
        Self { start_at_login: true, start_minimised: true }
    }
}

/// Window -> daemon, and second launch -> daemon.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    SetGain { value: f64 },
    SetHeadphone { value: f64 },
    SetMuted { value: bool },
    SetPhantom { value: bool },
    SetLowZ { value: bool },
    SetClipguard { value: bool },
    SetGainLock { value: bool },
    /// Percent of the microphone in the headphone mix, 0..=100.
    SetMicMix { value: f64 },
    SetStartAtLogin { value: bool },
    SetStartMinimised { value: bool },
    /// Error state only: try the restore again now.
    Retry,
    /// A second `wavexlr` was started: open the window.
    ShowWindow,
}

pub fn socket_path() -> std::path::PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").unwrap_or_else(|| "/tmp".into());
    std::path::Path::new(&dir).join("wavexlr.sock")
}
