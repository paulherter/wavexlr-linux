//! `wavexlr window --demo`: fake snapshots and a swept fake meter, so the look can be checked without the device.
//! No socket, no audio. Keys 1 to 5 switch the state: ready, muted, not connected, setting up, problem. Keys 6 to 8
//! are for stress pictures: the background service is down, a very long error, a very long device name.

use crate::ipc::{Command, DeviceInfoDto, LevelsDto, SettingsDto};

use super::model::{Screen, FLOOR_DB};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Ready,
    Muted,
    Absent,
    Initializing,
    Error,
    NoService,
    LongError,
    LongNames,
}

const KINDS: [Kind; 8] = [
    Kind::Ready,
    Kind::Muted,
    Kind::Absent,
    Kind::Initializing,
    Kind::Error,
    Kind::NoService,
    Kind::LongError,
    Kind::LongNames,
];

pub struct Demo {
    kind: Kind,
    levels: LevelsDto,
    settings: SettingsDto,
    frozen: Option<f64>,
    last_push: f64,
}

impl Demo {
    pub fn new(state: Option<&str>, frozen_meter: Option<f64>) -> Demo {
        let mut demo = Demo {
            kind: Kind::Ready,
            levels: LevelsDto {
                gain_db: 38.5,
                headphone_db: -23.5,
                muted: false,
                phantom: false,
                low_z: false,
                clipguard: true,
                gain_lock: false,
                mic_mix_percent: 40.0,
            },
            settings: SettingsDto { start_at_login: true, start_minimised: false },
            frozen: frozen_meter,
            last_push: f64::NEG_INFINITY,
        };
        let index = match state {
            Some("muted") => 1,
            Some("absent") | Some("not-connected") => 2,
            Some("initializing") | Some("setting-up") => 3,
            Some("error") | Some("problem") => 4,
            Some("noservice") | Some("no-service") => 5,
            Some("long-error") => 6,
            Some("long-names") => 7,
            _ => 0,
        };
        demo.set_kind(index);
        demo
    }

    /// 0 ready, 1 muted, 2 not connected, 3 setting up, 4 problem, 5 no service, 6 long error, 7 long names.
    pub fn set_kind(&mut self, index: usize) {
        self.kind = KINDS[index.min(KINDS.len() - 1)];
        match self.kind {
            Kind::Muted => self.levels.muted = true,
            Kind::Ready | Kind::LongNames => self.levels.muted = false,
            _ => {}
        }
    }

    pub fn screen(&self) -> Screen {
        match self.kind {
            Kind::Ready | Kind::Muted | Kind::LongNames => Screen::Ready(self.levels),
            Kind::Absent => Screen::Absent,
            Kind::Initializing => Screen::Initializing,
            Kind::Error => Screen::Error("The device did not take the saved levels.".into()),
            Kind::LongError => Screen::Error(
                "The device did not accept the saved levels after the USB connection came back from sleep".into(),
            ),
            Kind::NoService => Screen::NoService,
        }
    }

    pub fn device(&self) -> Option<DeviceInfoDto> {
        match self.kind {
            Kind::Ready | Kind::Muted => Some(DeviceInfoDto { name: "Wave XLR".into(), firmware: "1.3.5".into(), serial: "DEMO".into() }),
            Kind::LongNames => Some(DeviceInfoDto {
                name: "Wave XLR (Studio desk, left)".into(),
                firmware: "1.3.5 (build 20261110, release candidate 2)".into(),
                serial: "DEMO".into(),
            }),
            _ => None,
        }
    }

    pub fn settings(&self) -> SettingsDto {
        self.settings
    }

    /// The fake meter sweeps (and so needs frames) unless a level was frozen for a picture.
    pub fn is_sweeping(&self) -> bool {
        self.frozen.is_none()
    }

    pub fn apply(&mut self, command: &Command) {
        match *command {
            Command::SetGain { value } => self.levels.gain_db = value,
            Command::SetHeadphone { value } => self.levels.headphone_db = value,
            Command::SetMuted { value } => {
                self.levels.muted = value;
                self.kind = if value { Kind::Muted } else { Kind::Ready };
            }
            Command::SetPhantom { value } => self.levels.phantom = value,
            Command::SetLowZ { value } => self.levels.low_z = value,
            Command::SetClipguard { value } => self.levels.clipguard = value,
            Command::SetGainLock { value } => self.levels.gain_lock = value,
            Command::SetMicMix { value } => self.levels.mic_mix_percent = value.clamp(0.0, 100.0),
            Command::SetStartAtLogin { value } => self.settings.start_at_login = value,
            Command::SetStartMinimised { value } => self.settings.start_minimised = value,
            Command::Retry => {
                // The retry works after a moment: back to ready.
                if matches!(self.kind, Kind::Error | Kind::LongError) {
                    self.kind = if self.levels.muted { Kind::Muted } else { Kind::Ready };
                }
            }
            Command::ShowWindow => {}
        }
    }

/// A fake chunk level every 50 ms, like the real meter.
    pub fn meter_sample(&mut self, now: f64) -> Option<f64> {
        if let Some(level) = self.frozen {
            return Some(level);
        }
        if now - self.last_push < 0.05 {
            return None;
        }
        self.last_push = now;
        Some(sweep(now))
    }
}

/// A slow swell with quick wiggles, so every segment colour is visited and peak hold and fall can be seen.
pub fn sweep(t: f64) -> f64 {
    let swell = 0.5 + 0.5 * (t * 0.9).sin();
    let wiggle = 6.0 * (t * 7.3).sin() + 3.0 * (t * 13.1).sin();
    (-52.0 + 56.0 * swell.powf(1.4) + wiggle).clamp(FLOOR_DB, 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sweep_stays_on_the_scale_and_moves() {
        let values: Vec<f64> = (0..400).map(|i| sweep(f64::from(i) * 0.05)).collect();
        assert!(values.iter().all(|v| (FLOOR_DB..=0.0).contains(v)));
        let min = values.iter().cloned().fold(f64::MAX, f64::min);
        let max = values.iter().cloned().fold(f64::MIN, f64::max);
        assert!(max - min > 30.0, "range {min}..{max}");
    }

    #[test]
    fn states_follow_the_keys_and_commands() {
        let mut demo = Demo::new(None, None);
        assert!(matches!(demo.screen(), Screen::Ready(l) if !l.muted));
        demo.set_kind(1);
        assert!(matches!(demo.screen(), Screen::Ready(l) if l.muted));
        demo.set_kind(2);
        assert_eq!(demo.screen(), Screen::Absent);
        demo.set_kind(4);
        assert!(matches!(demo.screen(), Screen::Error(_)));
        demo.set_kind(0);
        demo.apply(&Command::SetGain { value: 10.0 });
        assert!(matches!(demo.screen(), Screen::Ready(l) if l.gain_db == 10.0));
        assert!(Demo::new(Some("muted"), None).levels.muted);
        demo.set_kind(5);
        assert_eq!(demo.screen(), Screen::NoService);
        demo.set_kind(6);
        assert!(matches!(demo.screen(), Screen::Error(e) if e.len() > 60));
        demo.set_kind(7);
        assert!(demo.device().unwrap().name.len() > 20);
        demo.set_kind(99); // out of range keys stop at the last state
        assert!(demo.device().is_some());
    }

    #[test]
    fn the_new_controls_and_settings_are_kept() {
        let mut demo = Demo::new(None, None);
        assert_eq!(demo.levels.mic_mix_percent, 40.0);
        demo.apply(&Command::SetMicMix { value: 100.0 });
        demo.apply(&Command::SetClipguard { value: false });
        demo.apply(&Command::SetGainLock { value: true });
        demo.apply(&Command::SetStartAtLogin { value: false });
        demo.apply(&Command::SetStartMinimised { value: true });
        assert!(matches!(demo.screen(), Screen::Ready(l) if l.mic_mix_percent == 100.0 && !l.clipguard && l.gain_lock));
        assert_eq!(demo.settings(), SettingsDto { start_at_login: false, start_minimised: true });
        assert_eq!(demo.device().unwrap().firmware, "1.3.5");
        demo.set_kind(2);
        assert!(demo.device().is_none());
    }

    #[test]
    fn frozen_meter_always_reports_its_level() {
        let mut demo = Demo::new(None, Some(-9.5));
        assert_eq!(demo.meter_sample(0.0), Some(-9.5));
        assert_eq!(demo.meter_sample(0.001), Some(-9.5));
    }
}
