//! Pure parts of the window: number formatting, fader maths, meter ballistics, write throttle, screen state.
//! Nothing here touches egui, so it is all unit-tested.

use crate::ipc::{LevelsDto, Snapshot, StateName};

pub const FLOOR_DB: f64 = -60.0;
/// The meter turns from accent to warning here, and to danger at `CLIP_DB`.
pub const WARN_DB: f64 = -12.0;
pub const CLIP_DB: f64 = -3.0;
pub const HOLD_S: f64 = 1.0;
pub const FALL_DB_PER_S: f64 = 30.0;
/// Writes to the daemon are merged over this long.
pub const WRITE_INTERVAL_S: f64 = 0.04;
/// After the user lets go, device updates are ignored this long so an older snapshot cannot move the control back.
pub const SETTLE_S: f64 = 0.3;

/// One decimal, a real minus sign (U+2212), never "−0.0".
pub fn format_db(value: f64) -> String {
    if value.abs() < 0.05 {
        return "0.0".into();
    }
    format!("{value:.1}").replace('-', "\u{2212}")
}

/// Scale mark without decimals: "0", "−12", "75".
pub fn format_mark(value: f64) -> String {
    let text = format_db(value);
    text.strip_suffix(".0").unwrap_or(&text).to_string()
}

// --- fader maths ---------------------------------------------------------------------------------------------------

/// A slider covers `low..=high` in whole steps of `step`. Everything a slider does is in whole steps.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Range {
    pub low: f64,
    pub high: f64,
    pub step: f64,
}

pub const GAIN: Range = Range { low: 0.0, high: 75.0, step: 0.5 };
pub const HEADPHONE: Range = Range { low: -60.0, high: 0.0, step: 0.5 };
/// The Mic/PC mix as the slider shows it: the PC share, 0 (all microphone, left end) to 100 (all PC, right end).
pub const MIX: Range = Range { low: 0.0, high: 100.0, step: 1.0 };

impl Range {
    pub fn max_steps(self) -> i32 {
        ((self.high - self.low) / self.step).round() as i32
    }
    pub fn value(self, steps: i32) -> f64 {
        self.low + f64::from(steps) * self.step
    }
    pub fn steps_for(self, value: f64) -> i32 {
        (((value - self.low) / self.step).round() as i32).clamp(0, self.max_steps())
    }
    /// 0.0 at the low end, 1.0 at the high end.
    pub fn fraction(self, steps: i32) -> f32 {
        steps as f32 / self.max_steps().max(1) as f32
    }
    pub fn step_by(self, steps: i32, delta: i32) -> i32 {
        (steps + delta).clamp(0, self.max_steps())
    }
}

/// The daemon speaks in microphone percent, the slider in PC share: the left end is 100 % microphone.
pub fn mix_steps_from_mic_percent(mic_percent: f64) -> i32 {
    MIX.steps_for(100.0 - mic_percent.clamp(0.0, 100.0))
}

pub fn mic_percent_from_mix_steps(steps: i32) -> f64 {
    100.0 - MIX.value(steps)
}

/// "Mic / PC" as shown next to the slider, e.g. "40 / 60" for 40 % microphone.
pub fn format_mix(steps: i32) -> String {
    let pc = MIX.value(steps).round() as i32;
    format!("{} / {}", 100 - pc, pc)
}

/// Where a fader value sits on screen. `top` is the y of the maximum, `bottom` the y of the minimum.
pub fn y_for(fraction: f32, top: f32, bottom: f32) -> f32 {
    bottom - fraction * (bottom - top)
}

/// A drag is relative to the grab point: pressing alone changes nothing, the value follows the pointer's movement.
/// Shift makes it four times finer. Movement is added up per frame, so pressing Shift mid-drag does not jump.
#[derive(Clone, Copy, Debug)]
pub struct Drag {
    acc: f64,
    last_y: f32,
}

impl Drag {
    pub fn start(steps: i32, y: f32) -> Self {
        Self { acc: f64::from(steps), last_y: y }
    }

    pub fn update(&mut self, y: f32, track_px: f32, max_steps: i32, fine: bool) -> i32 {
        let per_px = f64::from(max_steps) / f64::from(track_px.max(1.0));
        let factor = if fine { 0.25 } else { 1.0 };
        self.acc += f64::from(self.last_y - y) * per_px * factor;
        self.acc = self.acc.clamp(0.0, f64::from(max_steps));
        self.last_y = y;
        self.acc.round() as i32
    }
}

// --- meter ---------------------------------------------------------------------------------------------------------

/// Peak meter ballistics: instant rise, the peak is held for one second, then both fall at 30 dB/s.
/// Time is passed in (seconds) so the maths can be tested.
#[derive(Clone, Copy, Debug)]
pub struct Ballistics {
    pub level: f64,
    target: f64,
    pub peak: f64,
    peak_at: f64,
    last: f64,
}

impl Default for Ballistics {
    fn default() -> Self {
        Self { level: FLOOR_DB, target: FLOOR_DB, peak: FLOOR_DB, peak_at: 0.0, last: 0.0 }
    }
}

impl Ballistics {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn animating(&self) -> bool {
        self.level > self.target || self.peak > self.level
    }

    pub fn push(&mut self, db: f64, now: f64, instant: bool) {
        let db = db.clamp(FLOOR_DB, 0.0);
        if !self.animating() {
            self.last = now;
        }
        self.target = db;
        if db >= self.level || instant {
            self.level = db;
        }
        if db >= self.peak {
            self.peak = db;
            self.peak_at = now;
        }
    }

    /// Advance the fall. Call once per frame while `animating()`.
    pub fn tick(&mut self, now: f64, instant: bool) {
        let fall = FALL_DB_PER_S * (now - self.last).max(0.0);
        self.last = now;
        self.level = if instant { self.target } else { self.target.max(self.level - fall) };
        if now - self.peak_at > HOLD_S && self.peak > self.level {
            self.peak = if instant { self.level } else { self.level.max(self.peak - fall) };
        }
    }
}

/// Colour zone of a level: accent up to -12, warning up to -3, danger above.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Zone {
    Accent,
    Warning,
    Danger,
}

pub fn zone_of(db: f64) -> Zone {
    if db > CLIP_DB {
        Zone::Danger
    } else if db > WARN_DB {
        Zone::Warning
    } else {
        Zone::Accent
    }
}

// --- write throttle ------------------------------------------------------------------------------------------------

/// Sends the first change at once, then at most one write per interval with the latest value.
#[derive(Clone, Copy, Debug)]
pub struct Throttle {
    interval: f64,
    last_sent: f64,
    pending: Option<f64>,
}

impl Throttle {
    pub fn new(interval: f64) -> Self {
        Self { interval, last_sent: f64::NEG_INFINITY, pending: None }
    }

    pub fn has_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// A new value: returns it if it can go out now, otherwise keeps it for `poll`.
    pub fn set(&mut self, value: f64, now: f64) -> Option<f64> {
        self.pending = Some(value);
        self.poll(now)
    }

    pub fn poll(&mut self, now: f64) -> Option<f64> {
        if now - self.last_sent >= self.interval {
            if let Some(v) = self.pending.take() {
                self.last_sent = now;
                return Some(v);
            }
        }
        None
    }

    /// Seconds until a pending value may go out.
    pub fn wait(&self, now: f64) -> Option<f64> {
        self.pending.map(|_| (self.last_sent + self.interval - now).max(0.0))
    }

    /// Send whatever is pending right now (the user let go).
    pub fn flush(&mut self, now: f64) -> Option<f64> {
        let v = self.pending.take();
        if v.is_some() {
            self.last_sent = now;
        }
        v
    }
}

// --- layout maths --------------------------------------------------------------------------------------------------

/// Width of each of the two columns: floor((W - 44) / 2). 198 at 440, 192 at 428.
pub fn column_width(window_w: f32) -> f32 {
    ((window_w - 2.0 * 16.0 - 12.0) / 2.0).floor()
}

/// Height of the status line: the title, and a hint of `hint_lines` lines under it 2 px apart. The "Try again"
/// button is taller than a lone title line and sets the height then.
pub fn status_height(hint_lines: usize, has_retry: bool) -> f32 {
    let text = 18.0 + if hint_lines == 0 { 0.0 } else { 2.0 + 16.0 * hint_lines as f32 };
    if has_retry {
        text.max(28.0)
    } else {
        text
    }
}

/// Height of a switch row: 40, 48 with one hint line, 64 with two (the text block plus 7 px above and below).
pub fn row_height(hint_lines: usize) -> f32 {
    let text = 18.0 + 16.0 * hint_lines as f32;
    (text + 14.0).max(40.0)
}

/// Height of the fader area A = H - 354 - status height, less what a hint that wraps longer than designed adds.
pub fn fader_area(window_h: f32, status_h: f32, extra: f32) -> f32 {
    (window_h - 354.0 - status_h - extra).max(60.0)
}

// --- screen state --------------------------------------------------------------------------------------------------

/// The status light: a solid accent dot, a grey ring, a warning ring or a solid red dot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Light {
    Ready,
    Idle,
    Busy,
    Problem,
}

/// Everything the window can look like. One value, so impossible combinations cannot exist.
#[derive(Clone, Debug, PartialEq)]
pub enum Screen {
    /// Before the first answer from the daemon: only the background is drawn.
    Connecting,
    /// The daemon is not running (the socket is not there).
    NoService,
    Absent,
    Initializing,
    Ready(LevelsDto),
    Error(String),
}

impl Screen {
    pub fn from_snapshot(snapshot: &Snapshot) -> Screen {
        match (snapshot.state, snapshot.levels) {
            (StateName::Ready, Some(levels)) => Screen::Ready(levels),
            // Ready without levels cannot be shown; treat it as still setting up.
            (StateName::Ready, None) | (StateName::Initializing, _) => Screen::Initializing,
            (StateName::Absent, _) => Screen::Absent,
            (StateName::Error, _) => Screen::Error(snapshot.error.clone()),
        }
    }

    pub fn is_ready(&self) -> bool {
        matches!(self, Screen::Ready(_))
    }

    /// The title of the status line. With a device name it reads "<name> ready", else "Wave XLR ready".
    pub fn status_text(&self, device_name: Option<&str>) -> String {
        match self {
            Screen::Connecting => String::new(),
            Screen::NoService => "Wave XLR background service is not running".into(),
            Screen::Absent => "Wave XLR not connected".into(),
            Screen::Initializing => "Setting up the Wave XLR \u{2026}".into(),
            Screen::Ready(_) => {
                let name = device_name.map(str::trim).filter(|n| !n.is_empty()).unwrap_or("Wave XLR");
                format!("{name} ready")
            }
            Screen::Error(_) => "Problem with the Wave XLR".into(),
        }
    }

    /// How the status light looks.
    pub fn light(&self) -> Light {
        match self {
            Screen::Ready(_) => Light::Ready,
            Screen::Initializing => Light::Busy,
            Screen::Error(_) => Light::Problem,
            Screen::Connecting | Screen::NoService | Screen::Absent => Light::Idle,
        }
    }

    pub fn hint(&self) -> String {
        match self {
            Screen::Connecting | Screen::Ready(_) => String::new(),
            Screen::NoService => "This window connects by itself as soon as it is back.".into(),
            Screen::Absent => "Connect it by USB. Your levels come back by themselves.".into(),
            Screen::Initializing => "Bringing back your last levels.".into(),
            Screen::Error(error) => {
                let error = error.trim().trim_end_matches('.');
                let mut chars = error.chars();
                match chars.next() {
                    Some(first) => format!(
                        "{}{}. Trying again in a few seconds.",
                        first.to_uppercase(),
                        chars.as_str()
                    ),
                    None => "Trying again in a few seconds.".into(),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_with_real_minus_and_no_negative_zero() {
        assert_eq!(format_db(-23.5), "\u{2212}23.5");
        assert_eq!(format_db(38.5), "38.5");
        assert_eq!(format_db(-0.01), "0.0");
        assert_eq!(format_db(0.0), "0.0");
        assert_eq!(format_mark(-12.0), "\u{2212}12");
        assert_eq!(format_mark(0.0), "0");
        assert_eq!(format_mark(75.0), "75");
    }

    #[test]
    fn range_maps_db_and_steps() {
        assert_eq!(GAIN.max_steps(), 150);
        assert_eq!(HEADPHONE.max_steps(), 120);
        assert_eq!(MIX.max_steps(), 100);
        assert_eq!(GAIN.steps_for(38.5), 77);
        assert_eq!(GAIN.value(77), 38.5);
        assert_eq!(HEADPHONE.steps_for(-23.5), 73);
        assert_eq!(HEADPHONE.value(73), -23.5);
        assert_eq!(GAIN.steps_for(99.0), 150);
        assert_eq!(GAIN.steps_for(-5.0), 0);
        assert_eq!(GAIN.step_by(149, 10), 150);
        assert_eq!(GAIN.step_by(3, -10), 0);
    }

    #[test]
    fn fader_position_is_linear_in_db() {
        assert_eq!(y_for(0.0, 10.0, 210.0), 210.0);
        assert_eq!(y_for(1.0, 10.0, 210.0), 10.0);
        assert_eq!(y_for(GAIN.fraction(75), 10.0, 210.0), 110.0);
        assert_eq!(HEADPHONE.fraction(HEADPHONE.steps_for(-30.0)), 0.5);
    }

    #[test]
    fn drag_is_relative_to_the_grab_point() {
        // Press at y=100 on a value of 40 steps: pressing alone changes nothing.
        let mut drag = Drag::start(40, 100.0);
        assert_eq!(drag.update(100.0, 200.0, 150, false), 40);
        // 150 steps over 200 px: 0.75 steps per px. Moving up 20 px adds 15 steps.
        assert_eq!(drag.update(80.0, 200.0, 150, false), 55);
        // Moving down again goes back along the same path.
        assert_eq!(drag.update(100.0, 200.0, 150, false), 40);
    }

    #[test]
    fn drag_with_shift_is_four_times_finer_and_does_not_jump() {
        let mut drag = Drag::start(40, 100.0);
        assert_eq!(drag.update(90.0, 200.0, 150, false), 48); // 40 + 7.5
        // Shift pressed without movement: no jump.
        assert_eq!(drag.update(90.0, 200.0, 150, true), 48);
        // 20 px at a quarter of the speed: 3.75 steps more.
        assert_eq!(drag.update(70.0, 200.0, 150, true), 51);
    }

    #[test]
    fn drag_stops_at_both_ends() {
        let mut drag = Drag::start(145, 100.0);
        assert_eq!(drag.update(0.0, 200.0, 150, false), 150);
        assert_eq!(drag.update(400.0, 200.0, 150, false), 0);
    }

    #[test]
    fn ballistics_rise_instantly_hold_then_fall() {
        let mut m = Ballistics::default();
        m.push(-6.0, 0.0, false);
        assert_eq!((m.level, m.peak), (-6.0, -6.0));
        assert!(!m.animating());
        // The signal drops away: the level falls at 30 dB/s, the peak waits one second.
        m.push(-60.0, 0.1, false);
        assert!(m.animating());
        m.tick(0.2, false);
        assert!((m.level + 9.0).abs() < 1e-9, "level {}", m.level);
        assert_eq!(m.peak, -6.0);
        m.tick(1.2, false);
        assert!((m.level + 39.0).abs() < 1e-9);
        assert!((m.peak + 36.0).abs() < 1e-9, "peak {}", m.peak);
        m.tick(3.0, false);
        assert_eq!((m.level, m.peak), (-60.0, -60.0));
        assert!(!m.animating());
    }

    #[test]
    fn ballistics_a_louder_sound_refreshes_the_hold() {
        let mut m = Ballistics::default();
        m.push(-20.0, 0.0, false);
        m.push(-10.0, 0.9, false);
        m.push(-60.0, 1.0, false);
        m.tick(1.5, false); // 1.5 s after the first peak but only 0.6 s after the second
        assert_eq!(m.peak, -10.0);
    }

    #[test]
    fn ballistics_without_motion_follow_the_signal() {
        let mut m = Ballistics::default();
        m.push(-6.0, 0.0, true);
        m.push(-40.0, 0.1, true);
        assert_eq!(m.level, -40.0);
        m.tick(1.2, true);
        assert_eq!(m.peak, -40.0);
    }

    #[test]
    fn ballistics_clamp_to_the_scale() {
        let mut m = Ballistics::default();
        m.push(12.0, 0.0, false);
        assert_eq!(m.level, 0.0);
        m.reset();
        m.push(-90.0, 0.0, false);
        assert_eq!(m.level, FLOOR_DB);
    }

    #[test]
    fn zones_split_at_minus_12_and_minus_3() {
        assert_eq!(zone_of(-12.0), Zone::Accent);
        assert_eq!(zone_of(-11.9), Zone::Warning);
        assert_eq!(zone_of(-3.0), Zone::Warning);
        assert_eq!(zone_of(-2.9), Zone::Danger);
    }

    #[test]
    fn throttle_sends_first_value_at_once_then_merges() {
        let mut t = Throttle::new(0.04);
        assert_eq!(t.set(1.0, 0.0), Some(1.0));
        assert_eq!(t.set(2.0, 0.01), None);
        assert_eq!(t.set(3.0, 0.02), None);
        assert!(t.has_pending());
        assert!((t.wait(0.02).unwrap() - 0.02).abs() < 1e-9);
        assert_eq!(t.poll(0.03), None);
        assert_eq!(t.poll(0.05), Some(3.0));
        assert!(!t.has_pending());
        assert_eq!(t.poll(0.2), None);
    }

    #[test]
    fn throttle_flush_sends_the_last_value() {
        let mut t = Throttle::new(0.04);
        t.set(1.0, 0.0);
        t.set(2.0, 0.01);
        assert_eq!(t.flush(0.012), Some(2.0));
        assert_eq!(t.flush(0.013), None);
    }

    fn snapshot(state: StateName, error: &str, levels: Option<LevelsDto>) -> Snapshot {
        Snapshot { state, error: error.into(), levels, device: None, settings: Default::default() }
    }

    #[test]
    fn screen_follows_the_snapshot() {
        let levels = LevelsDto {
            gain_db: 38.5,
            headphone_db: -23.5,
            muted: false,
            phantom: false,
            low_z: false,
            clipguard: false,
            gain_lock: false,
            mic_mix_percent: 0.0,
        };
        assert_eq!(Screen::from_snapshot(&snapshot(StateName::Ready, "", Some(levels))), Screen::Ready(levels));
        assert_eq!(Screen::from_snapshot(&snapshot(StateName::Ready, "", None)), Screen::Initializing);
        assert_eq!(Screen::from_snapshot(&snapshot(StateName::Absent, "", None)), Screen::Absent);
        assert_eq!(
            Screen::from_snapshot(&snapshot(StateName::Error, "boom", None)),
            Screen::Error("boom".into())
        );
    }

    #[test]
    fn error_hint_is_a_plain_sentence() {
        let screen = Screen::Error("the device did not take the saved levels.".into());
        assert_eq!(
            screen.hint(),
            "The device did not take the saved levels. Trying again in a few seconds."
        );
        assert_eq!(Screen::Error(String::new()).hint(), "Trying again in a few seconds.");
        assert_eq!(Screen::Absent.status_text(None), "Wave XLR not connected");
        assert_eq!(Screen::NoService.status_text(None), "Wave XLR background service is not running");
    }

    #[test]
    fn status_titles_and_lights_follow_the_five_states() {
        let ready = Screen::Ready(LevelsDto {
            gain_db: 0.0,
            headphone_db: 0.0,
            muted: false,
            phantom: false,
            low_z: false,
            clipguard: false,
            gain_lock: false,
            mic_mix_percent: 40.0,
        });
        assert_eq!(ready.status_text(None), "Wave XLR ready");
        assert_eq!(ready.status_text(Some("Wave XLR")), "Wave XLR ready");
        assert_eq!(ready.status_text(Some("Wave XLR (Studio desk, left)")), "Wave XLR (Studio desk, left) ready");
        assert_eq!(ready.status_text(Some("  ")), "Wave XLR ready");
        assert_eq!(Screen::Initializing.status_text(None), "Setting up the Wave XLR \u{2026}");
        assert_eq!(Screen::Error("x".into()).status_text(None), "Problem with the Wave XLR");
        assert_eq!(Screen::Initializing.hint(), "Bringing back your last levels.");
        assert_eq!(Screen::Absent.hint(), "Connect it by USB. Your levels come back by themselves.");
        assert_eq!(Screen::NoService.hint(), "This window connects by itself as soon as it is back.");
        assert_eq!(ready.light(), Light::Ready);
        assert_eq!(Screen::Absent.light(), Light::Idle);
        assert_eq!(Screen::NoService.light(), Light::Idle);
        assert_eq!(Screen::Initializing.light(), Light::Busy);
        assert_eq!(Screen::Error(String::new()).light(), Light::Problem);
    }

    #[test]
    fn the_mix_slider_shows_pc_share_but_the_command_carries_the_mic_share() {
        // 40 % microphone: the thumb sits at 60 % (towards PC) and the readout says "40 / 60".
        let steps = mix_steps_from_mic_percent(40.0);
        assert_eq!(steps, 60);
        assert_eq!(format_mix(steps), "40 / 60");
        assert_eq!(mic_percent_from_mix_steps(steps), 40.0);
        // Left end = all microphone = 100, right end = all PC = 0.
        assert_eq!(mic_percent_from_mix_steps(0), 100.0);
        assert_eq!(mic_percent_from_mix_steps(100), 0.0);
        assert_eq!(format_mix(0), "100 / 0");
        assert_eq!(format_mix(100), "0 / 100");
        assert_eq!(mix_steps_from_mic_percent(250.0), 0);
        assert_eq!(mix_steps_from_mic_percent(-5.0), 100);
    }

    #[test]
    fn layout_maths_match_the_spec() {
        assert_eq!(column_width(440.0), 198.0);
        assert_eq!(column_width(428.0), 192.0);
        assert_eq!(fader_area(600.0, status_height(0, false), 0.0), 228.0);
        assert_eq!(fader_area(560.0, status_height(0, false), 0.0), 188.0);
        assert_eq!(status_height(0, false), 18.0);
        assert_eq!(status_height(1, false), 36.0);
        assert_eq!(status_height(2, false), 52.0);
        assert_eq!(status_height(3, true), 68.0);
        assert_eq!(row_height(0), 40.0);
        assert_eq!(row_height(1), 48.0);
        assert_eq!(row_height(2), 64.0);
        // The right column is A + 314 high and the microphone group A + 297, both ending inside the padding.
        let a = fader_area(600.0, 18.0, 0.0);
        let phones = 74.0 + 12.0 + a + 12.0 + 46.0 + 12.0 + 1.0 + 40.0;
        let app = 40.0 + 1.0 + 40.0;
        assert_eq!(phones + 12.0 + app + 8.0 + 16.0, a + 314.0);
        assert_eq!(74.0 + 12.0 + a + 12.0 + 32.0 + 12.0 + (1.0 + 48.0 + 1.0 + 40.0 + 1.0 + 64.0), a + 297.0);
        assert_eq!(12.0 + 18.0 + 12.0 + (a + 314.0) + 16.0, 600.0);
    }
}
