//! Drives the whole window headlessly with synthetic input: no display, no device.

use super::*;
use egui::{PointerButton, RawInput, ViewportId};

struct Harness {
    ctx: Context,
    app: App,
    t: f64,
    size: Vec2,
    last_repaint: Duration,
}

impl Harness {
    fn new(state: &str) -> Harness {
        Harness::with(state, WINDOW, false)
    }

    fn with(state: &str, size: Vec2, reduced: bool) -> Harness {
        Harness::build(state, size, reduced, Some(-30.0))
    }

    /// A demo whose meter keeps moving (no frozen level).
    fn sweeping(state: &str) -> Harness {
        Harness::build(state, WINDOW, false, None)
    }

    fn build(state: &str, size: Vec2, reduced: bool, meter: Option<f64>) -> Harness {
        let ctx = Context::default();
        draw::install_fonts(&ctx);
        let options = Options { demo: true, state: Some(state.into()), meter, theme: Some("dark".into()), ..Options::default() };
        let mut app = App::new(ctx.clone(), &options);
        app.reduced = reduced;
        let mut h = Harness { ctx, app, t: 1.0, size, last_repaint: Duration::ZERO };
        h.frame(vec![]);
        h.frame(vec![]);
        h
    }

    fn frame(&mut self, events: Vec<Event>) {
        self.t += 0.016;
        let input = RawInput {
            time: Some(self.t),
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, self.size)),
            events,
            ..RawInput::default()
        };
        let app = &mut self.app;
        let mut output = self.ctx.run_ui(input, |ui| app.show(ui));
        self.last_repaint = output.viewport_output.get(&ViewportId::ROOT).map_or(Duration::MAX, |v| v.repaint_delay);
        output.textures_delta.clear(); // a debug build panics if the deltas are dropped unhandled
    }

    /// Let `seconds` pass in ordinary frames.
    fn advance(&mut self, seconds: f64) {
        let end = self.t + seconds;
        while self.t < end {
            self.frame(vec![]);
        }
    }

    fn rect(&self, name: &str) -> Rect {
        *self.app.probes.get(name).unwrap_or_else(|| panic!("no control {name}"))
    }

    fn press(&mut self, pos: Pos2) {
        self.frame(vec![Event::PointerMoved(pos)]);
        self.frame(vec![Event::PointerButton { pos, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE }]);
    }

    fn release(&mut self, pos: Pos2) {
        self.frame(vec![Event::PointerButton { pos, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE }]);
        self.frame(vec![]);
    }

    fn click(&mut self, pos: Pos2) {
        self.press(pos);
        self.release(pos);
    }

    fn key(&mut self, key: Key) {
        let down = Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE };
        let up = Event::Key { key, physical_key: None, pressed: false, repeat: false, modifiers: Modifiers::NONE };
        self.frame(vec![down]);
        self.frame(vec![up]);
    }

    fn demo(&self) -> &Demo {
        match &self.app.backend {
            Backend::Demo(d) => d,
            Backend::Live { .. } => unreachable!(),
        }
    }

    fn levels(&self) -> crate::ipc::LevelsDto {
        match self.demo().screen() {
            Screen::Ready(l) => l,
            other => panic!("not ready: {other:?}"),
        }
    }

    fn settings(&self) -> SettingsDto {
        self.demo().settings()
    }

    fn focused(&self) -> Option<Id> {
        self.ctx.memory(|m| m.focused())
    }

    /// Focus a control with Tab presses and return how many it took.
    fn tab_to(&mut self, name: &'static str) {
        for _ in 0..16 {
            self.key(Key::Tab);
            if self.focused() == Some(wid(name)) {
                return;
            }
        }
        panic!("could not Tab to {name}");
    }

    fn dialog_open(&self) -> bool {
        self.app.dialog_blocks_input()
    }

    fn scale_of(&self, name: &'static str) -> f32 {
        self.app.presses.scale_of(wid(name), self.t)
    }

    /// Where a switch knob is, 0 (off) to 1 (on).
    fn knob(&self, name: &'static str) -> f32 {
        self.app.thumbs.get(name).map_or(0.0, |t| t.value(self.t))
    }
}

/// Screen position of a vertical fader's thumb for a value in steps, found from the fader's frame.
fn thumb_pos(h: &Harness, name: &str, steps: i32, max: i32) -> Pos2 {
    let r = h.rect(name);
    let (x, top, bottom) = view::fader_track(r);
    Pos2::new(x, bottom - steps as f32 / max as f32 * (bottom - top))
}

/// Screen position of the mix thumb for PC steps.
fn mix_thumb_pos(h: &Harness, steps: i32) -> Pos2 {
    let hit = h.rect("mix");
    let block = Rect::from_min_size(Pos2::new(hit.left(), hit.center().y - 36.0), Vec2::new(hit.width(), 46.0));
    let (track, lo, hi) = view::mix_track(block);
    Pos2::new(lo + (hi - lo) * model::MIX.fraction(steps), track.center().y)
}

// --- faders ----------------------------------------------------------------------------------------------------------

#[test]
fn pressing_the_fader_track_does_not_move_the_value() {
    let mut h = Harness::new("ready");
    let before = h.app.gain.steps;
    let far = thumb_pos(&h, "gain", 20, 150); // well away from the thumb
    h.press(far);
    h.frame(vec![]);
    assert_eq!(h.app.gain.steps, before, "a click on the track must not jump");
    h.release(far);
    assert_eq!(h.app.gain.steps, before);
    assert_eq!(h.levels().gain_db, 38.5);
}

#[test]
fn dragging_moves_relative_to_the_grab_point_and_sends_the_final_value() {
    let mut h = Harness::new("ready");
    let start = h.app.gain.steps;
    let grab = thumb_pos(&h, "gain", start, 150);
    h.press(grab);
    h.frame(vec![Event::PointerMoved(grab)]);
    assert_eq!(h.app.gain.steps, start, "no jump at the grab");
    let (_, top, bottom) = view::fader_track(h.rect("gain"));
    let per_px = 150.0 / (bottom - top);
    // Drag up by 30 px in a few moves.
    for i in 1..=6 {
        h.frame(vec![Event::PointerMoved(Pos2::new(grab.x, grab.y - 5.0 * i as f32))]);
    }
    let expected = start + (30.0 * per_px).round() as i32;
    assert_eq!(h.app.gain.steps, expected);
    h.release(Pos2::new(grab.x, grab.y - 30.0));
    // The released value reaches the daemon (here: the demo state).
    assert_eq!(h.levels().gain_db, model::GAIN.value(expected));
}

#[test]
fn the_headphone_fader_runs_from_minus_60_to_0() {
    let mut h = Harness::new("ready");
    let start = h.app.headphone.steps;
    assert_eq!(model::HEADPHONE.value(start), -23.5);
    let grab = thumb_pos(&h, "headphone", start, 120);
    h.press(grab);
    h.frame(vec![Event::PointerMoved(Pos2::new(grab.x, grab.y - 400.0))]);
    h.release(Pos2::new(grab.x, grab.y - 400.0));
    assert_eq!(h.levels().headphone_db, 0.0);
}

#[test]
fn keys_move_a_focused_fader_by_half_a_db_and_five_db_and_never_to_full() {
    let mut h = Harness::new("ready");
    h.tab_to("gain");
    let start = h.app.gain.steps;
    h.key(Key::ArrowUp);
    assert_eq!(h.app.gain.steps, start + 1);
    h.key(Key::ArrowDown);
    h.key(Key::ArrowDown);
    assert_eq!(h.app.gain.steps, start - 1);
    h.key(Key::PageUp);
    assert_eq!(h.app.gain.steps, start + 9);
    h.key(Key::PageDown);
    assert_eq!(h.app.gain.steps, start - 1);
    h.key(Key::PageUp);
    h.key(Key::Home);
    h.key(Key::End);
    assert_eq!(h.app.gain.steps, start + 9, "Home and End must not slam the level");
    // A moment later the last value is on the daemon side.
    h.t += 1.0;
    h.frame(vec![]);
    assert_eq!(h.levels().gain_db, model::GAIN.value(start + 9));
}

#[test]
fn a_held_fader_is_not_moved_by_device_updates() {
    let mut h = Harness::new("ready");
    let start = h.app.gain.steps;
    let grab = thumb_pos(&h, "gain", start, 150);
    h.press(grab);
    h.frame(vec![Event::PointerMoved(Pos2::new(grab.x, grab.y - 20.0))]);
    let held = h.app.gain.steps;
    assert_ne!(held, start);
    // The device reports its old value while the hand is on the fader.
    if let Backend::Demo(d) = &mut h.app.backend {
        d.apply(&Command::SetGain { value: 10.0 });
    }
    h.frame(vec![]);
    assert_eq!(h.app.gain.steps, held);
}

#[test]
fn writes_while_dragging_are_merged_over_40_ms() {
    let mut h = Harness::new("ready");
    let grab = thumb_pos(&h, "gain", h.app.gain.steps, 150);
    h.press(grab);
    let mut sent = Vec::new();
    for i in 1..=12 {
        let before = h.levels().gain_db;
        h.frame(vec![Event::PointerMoved(Pos2::new(grab.x, grab.y - 2.0 * i as f32))]);
        if h.levels().gain_db != before {
            sent.push(h.t);
        }
    }
    // 12 frames of 16 ms move the pointer 12 times, but at most one write per 40 ms leaves.
    assert!(sent.len() >= 2 && sent.len() <= 6, "writes at {sent:?}");
    for pair in sent.windows(2) {
        assert!(pair[1] - pair[0] >= 0.039, "two writes {} s apart", pair[1] - pair[0]);
    }
}

// --- the Mic/PC mix --------------------------------------------------------------------------------------------------

#[test]
fn the_mix_shows_mic_and_pc_and_starts_at_40_60() {
    let h = Harness::new("ready");
    assert_eq!(h.app.mix.steps, 60, "40 % microphone is 60 steps towards PC");
    assert_eq!(model::format_mix(h.app.mix.steps), "40 / 60");
}

#[test]
fn the_mix_sends_the_microphone_share_and_the_right_end_is_pc() {
    let mut h = Harness::new("ready");
    let grab = mix_thumb_pos(&h, 60);
    h.press(grab);
    h.frame(vec![Event::PointerMoved(grab)]);
    assert_eq!(h.app.mix.steps, 60, "no jump at the grab");
    // All the way right: all PC, so the microphone share is 0.
    h.frame(vec![Event::PointerMoved(Pos2::new(grab.x + 400.0, grab.y))]);
    h.release(Pos2::new(grab.x + 400.0, grab.y));
    assert_eq!(h.app.mix.steps, 100);
    assert_eq!(h.levels().mic_mix_percent, 0.0);
    // All the way left: all microphone, 100.
    let grab = mix_thumb_pos(&h, 100);
    h.press(grab);
    h.frame(vec![Event::PointerMoved(Pos2::new(grab.x - 400.0, grab.y))]);
    h.release(Pos2::new(grab.x - 400.0, grab.y));
    assert_eq!(h.app.mix.steps, 0);
    assert_eq!(h.levels().mic_mix_percent, 100.0);
}

#[test]
fn pressing_the_mix_track_does_not_jump_the_mix() {
    let mut h = Harness::new("ready");
    let far = mix_thumb_pos(&h, 5);
    h.press(far);
    h.frame(vec![]);
    h.release(far);
    assert_eq!(h.app.mix.steps, 60);
    assert_eq!(h.levels().mic_mix_percent, 40.0);
}

#[test]
fn mix_keys_are_left_right_one_and_page_ten() {
    let mut h = Harness::new("ready");
    h.tab_to("mix");
    h.key(Key::ArrowRight);
    assert_eq!(h.app.mix.steps, 61);
    h.key(Key::ArrowLeft);
    h.key(Key::ArrowLeft);
    assert_eq!(h.app.mix.steps, 59);
    h.key(Key::PageUp);
    assert_eq!(h.app.mix.steps, 69);
    h.key(Key::PageDown);
    h.key(Key::PageDown);
    assert_eq!(h.app.mix.steps, 49);
    h.key(Key::ArrowUp); // up and down belong to the vertical faders
    assert_eq!(h.app.mix.steps, 49);
    h.key(Key::Home);
    h.key(Key::End);
    assert_eq!(h.app.mix.steps, 49);
    h.advance(0.5);
    assert_eq!(h.levels().mic_mix_percent, 51.0, "49 towards PC is 51 % microphone");
}

// --- switches, mute, settings -----------------------------------------------------------------------------------------

#[test]
fn m_toggles_mute_and_the_ring_stays_hidden_for_the_mouse() {
    let mut h = Harness::new("ready");
    assert!(!h.app.mute.on);
    h.key(Key::M);
    assert!(h.app.mute.on);
    assert!(h.levels().muted);
    h.key(Key::M);
    assert!(!h.levels().muted);
    // The mouse does not turn the focus ring on; Tab does.
    let c = h.rect("mute").center();
    h.click(c);
    assert!(h.levels().muted);
    assert!(!h.app.focus_visible);
    h.key(Key::Tab);
    assert!(h.app.focus_visible);
}

#[test]
fn the_48v_dialog_defaults_to_cancel() {
    let mut h = Harness::new("ready");
    let switch = h.rect("phantom").center();
    h.click(switch);
    assert!(h.dialog_open(), "48 V asks first");
    assert!(!h.levels().phantom);
    h.frame(vec![]);
    // Enter hits the default button, which is Cancel.
    h.key(Key::Enter);
    assert!(!h.dialog_open());
    assert!(!h.levels().phantom);
    assert!(!h.app.phantom.on);
    h.advance(0.3);
    assert!(h.app.dialog.is_none(), "the dialog is gone once it has faded out");

    // Escape cancels too.
    h.click(switch);
    assert!(h.dialog_open());
    h.key(Key::Escape);
    assert!(!h.dialog_open());
    assert!(!h.levels().phantom);
    h.advance(0.3);

    // Only an explicit click on "Switch on" turns it on.
    h.click(switch);
    h.frame(vec![]);
    let accept = h.rect("dialog-accept").center();
    h.click(accept);
    assert!(!h.dialog_open());
    assert!(h.levels().phantom);
    assert!(h.app.phantom.on);
    h.advance(0.3);

    // Switching it off needs no question.
    h.click(switch);
    assert!(h.app.dialog.is_none());
    assert!(!h.levels().phantom);
}

#[test]
fn the_dialog_makes_the_window_behind_inert() {
    let mut h = Harness::new("ready");
    h.click(h.rect("phantom").center());
    assert!(h.dialog_open());
    h.frame(vec![]);
    let mute = h.rect("mute").center();
    h.click(mute);
    assert!(!h.levels().muted, "the scrim blocks clicks");
    h.key(Key::M);
    assert!(!h.levels().muted, "and the M key");
    let clip = h.rect("clipguard").center();
    h.click(clip);
    assert!(h.levels().clipguard, "Clipguard stays as it was (on in the demo)");
}

#[test]
fn the_dialog_returns_the_focus_to_the_48v_switch() {
    let mut h = Harness::new("ready");
    h.tab_to("phantom");
    h.key(Key::Space);
    assert!(h.dialog_open());
    h.frame(vec![]);
    h.key(Key::Escape);
    h.advance(0.3);
    assert_eq!(h.focused(), Some(wid("phantom")));
}

#[test]
fn low_impedance_is_a_plain_switch() {
    let mut h = Harness::new("ready");
    let c = h.rect("low_z").center();
    h.click(c);
    assert!(h.levels().low_z);
    h.click(c);
    assert!(!h.levels().low_z);
}

#[test]
fn clipguard_and_gain_lock_send_their_commands() {
    let mut h = Harness::new("ready");
    assert!(h.levels().clipguard);
    assert!(!h.levels().gain_lock);
    let clip = h.rect("clipguard").center();
    h.click(clip);
    assert!(!h.levels().clipguard);
    h.click(clip);
    assert!(h.levels().clipguard);
    let lock = h.rect("gain_lock").center();
    h.click(lock);
    assert!(h.levels().gain_lock);
    h.click(lock);
    assert!(!h.levels().gain_lock);
}

#[test]
fn the_app_settings_send_their_commands_and_follow_the_snapshot() {
    let mut h = Harness::new("ready");
    assert_eq!(h.settings(), SettingsDto { start_at_login: true, start_minimised: false });
    let login = h.rect("start_login").center();
    let minimised = h.rect("start_min").center();
    h.click(login);
    assert!(!h.settings().start_at_login);
    h.click(minimised);
    assert!(h.settings().start_minimised);
    // The state comes back from the daemon: a snapshot with other values moves the switches.
    h.advance(0.5);
    if let Backend::Demo(d) = &mut h.app.backend {
        d.apply(&Command::SetStartAtLogin { value: true });
    }
    h.advance(0.1);
    assert!(h.app.start_login.on);
    assert!(h.app.start_min.on);
}

#[test]
fn the_app_group_works_while_the_device_is_away_but_the_device_controls_do_not() {
    let mut h = Harness::new("absent");
    let login = h.rect("start_login").center();
    h.click(login);
    assert!(!h.settings().start_at_login);
    let before = h.app.gain.steps;
    let r = h.rect("gain");
    h.press(r.center());
    h.frame(vec![Event::PointerMoved(r.center() - Vec2::new(0.0, 40.0))]);
    h.release(r.center());
    assert_eq!(h.app.gain.steps, before);
    h.key(Key::M);
    assert!(!h.app.mute.on);
    for name in ["phantom", "clipguard", "gain_lock", "low_z", "mute"] {
        let c = h.rect(name).center();
        h.click(c);
        assert!(h.app.dialog.is_none(), "{name} must be inert");
    }
    let before = h.app.mix.steps;
    let mix = h.rect("mix").center();
    h.press(mix);
    h.frame(vec![Event::PointerMoved(mix + Vec2::new(40.0, 0.0))]);
    h.release(mix);
    assert_eq!(h.app.mix.steps, before);
}

#[test]
fn no_service_disables_the_app_rows_too() {
    let mut h = Harness::new("noservice");
    let c = h.rect("start_login").center();
    h.click(c);
    assert!(h.settings().start_at_login, "nobody is there to take the command");
}

// --- hit areas and keyboard ------------------------------------------------------------------------------------------

#[test]
fn hit_areas_are_at_least_40_px() {
    let mut h = Harness::new("ready");
    for name in ["gain", "headphone", "mix", "mute", "phantom", "clipguard", "gain_lock", "low_z", "start_login", "start_min"] {
        let r = h.rect(name);
        assert!(r.width() >= 40.0 && r.height() >= 40.0, "{name} is {:?}", r.size());
    }
    let e = Harness::new("error");
    let r = e.rect("retry");
    assert!(r.width() >= 40.0 && r.height() >= 40.0, "retry is {:?}", r.size());
    h.click(h.rect("phantom").center());
    h.frame(vec![]);
    for name in ["dialog-accept", "dialog-cancel"] {
        let r = h.rect(name);
        assert!(r.width() >= 40.0 && r.height() >= 40.0, "{name} is {:?}", r.size());
    }
}

#[test]
fn mute_is_32_high_with_12_px_of_hit_area_above_and_below() {
    let h = Harness::new("ready");
    assert_eq!(h.rect("mute").height(), 56.0);
}

#[test]
fn the_hit_area_of_the_mix_is_the_slider_row_widened_to_40() {
    let h = Harness::new("ready");
    assert_eq!(h.rect("mix").height(), 40.0);
}

#[test]
fn rows_are_40_48_and_64_high_and_as_wide_as_the_column() {
    let h = Harness::new("ready");
    assert_eq!(h.rect("phantom").height(), 48.0);
    assert_eq!(h.rect("clipguard").height(), 40.0);
    assert_eq!(h.rect("gain_lock").height(), 64.0);
    assert_eq!(h.rect("low_z").height(), 40.0);
    assert_eq!(h.rect("start_login").height(), 40.0);
    assert_eq!(h.rect("phantom").width(), 198.0);
    let min = Harness::with("ready", WINDOW_MIN, false);
    assert_eq!(min.rect("phantom").width(), 192.0);
    assert_eq!(min.rect("gain_lock").height(), 64.0, "the long hint still takes two lines at the minimum width");
}

#[test]
fn the_fader_area_is_228_high_by_default_and_188_at_the_minimum() {
    let h = Harness::new("ready");
    assert_eq!(h.rect("gain").height(), 228.0);
    assert_eq!(h.rect("gain").width(), 54.0);
    let min = Harness::with("ready", WINDOW_MIN, false);
    assert_eq!(min.rect("gain").height(), 188.0);
    // A hint under the status line takes the room from the faders.
    let away = Harness::new("absent");
    assert_eq!(away.rect("gain").height(), 228.0 - 18.0);
    // The problem hint takes two lines next to the Try again button: 18 + 2 + 32 = 52.
    let error = Harness::new("error");
    assert_eq!(error.rect("gain").height(), 228.0 - 34.0);
}

#[test]
fn the_columns_sit_16_px_from_the_edges_with_12_between() {
    let h = Harness::new("ready");
    let mic = h.rect("phantom");
    let phones = h.rect("low_z");
    assert_eq!(mic.left(), 16.0);
    assert_eq!(phones.right(), 440.0 - 16.0);
    assert_eq!(phones.left() - mic.right(), 12.0);
}

#[test]
fn tab_order_follows_the_design() {
    let mut h = Harness::new("ready");
    let mut seen = Vec::new();
    for _ in 0..12 {
        h.key(Key::Tab);
        seen.push(h.focused());
    }
    let order = [
        "gain", "mute", "phantom", "clipguard", "gain_lock", "headphone", "mix", "low_z", "start_login", "start_min", "gain", "mute",
    ];
    let expected: Vec<Option<Id>> = order.iter().map(|n| Some(wid(n))).collect();
    assert_eq!(seen, expected);
    assert!(h.app.focus_visible);
}

#[test]
fn tab_skips_what_is_inert() {
    let mut h = Harness::new("absent");
    let mut seen = Vec::new();
    for _ in 0..3 {
        h.key(Key::Tab);
        seen.push(h.focused());
    }
    assert_eq!(seen, vec![Some(wid("start_login")), Some(wid("start_min")), Some(wid("start_login"))]);
}

#[test]
fn error_state_puts_try_again_first_in_the_tab_order() {
    let mut h = Harness::new("error");
    h.key(Key::Tab);
    assert_eq!(h.focused(), Some(wid("retry")));
    h.key(Key::Tab);
    assert_eq!(h.focused(), Some(wid("start_login")));
}

#[test]
fn space_and_enter_press_the_focused_switch_and_button() {
    let mut h = Harness::new("ready");
    h.tab_to("clipguard");
    h.key(Key::Space);
    assert!(!h.levels().clipguard);
    h.key(Key::Enter);
    assert!(h.levels().clipguard);
    h.tab_to("mute");
    h.key(Key::Enter);
    assert!(h.levels().muted);
    h.key(Key::Space);
    assert!(!h.levels().muted);
}

#[test]
fn try_again_asks_the_daemon() {
    let mut h = Harness::new("error");
    let c = h.rect("retry").center();
    h.click(c);
    assert_eq!(h.app.screen(), Screen::Ready(h.levels()), "the demo recovers on Retry");
}

#[test]
fn demo_keys_switch_the_state() {
    let mut h = Harness::new("ready");
    h.key(Key::Num3);
    assert_eq!(h.app.screen(), Screen::Absent);
    h.key(Key::Num5);
    assert!(matches!(h.app.screen(), Screen::Error(_)));
    h.key(Key::Num2);
    assert!(matches!(h.app.screen(), Screen::Ready(l) if l.muted));
    h.key(Key::Num4);
    assert_eq!(h.app.screen(), Screen::Initializing);
    h.key(Key::Num6);
    assert_eq!(h.app.screen(), Screen::NoService);
    h.key(Key::Num7);
    assert!(matches!(h.app.screen(), Screen::Error(e) if e.len() > 60));
    h.key(Key::Num8);
    assert!(h.app.device().is_some_and(|d| d.name.len() > 20));
    h.key(Key::Num1);
    assert!(matches!(h.app.screen(), Screen::Ready(l) if !l.muted));
}

// --- accessibility ------------------------------------------------------------------------------------------------------

#[test]
fn every_control_has_an_accessible_name_and_faders_report_their_value() {
    let mut h = Harness::new("ready");
    let names = &h.app.names;
    let label = |k: &str| names.get(k).unwrap_or_else(|| panic!("{k} has no accessible name")).0.clone();
    assert_eq!(label("gain"), "Microphone gain");
    assert_eq!(label("mute"), "Mute");
    assert_eq!(label("phantom"), "48 V");
    assert_eq!(label("clipguard"), "Clipguard");
    assert_eq!(label("gain_lock"), "Gain lock");
    assert_eq!(label("headphone"), "Headphone volume");
    assert_eq!(label("mix"), "Mic/PC mix");
    assert_eq!(label("low_z"), "Low impedance");
    assert_eq!(label("start_login"), "Start at login");
    assert_eq!(label("start_min"), "Start minimised");
    assert_eq!(names["gain"].1.as_deref(), Some("38.5 dB"));
    assert_eq!(names["headphone"].1.as_deref(), Some("\u{2212}23.5 dB"));
    assert_eq!(names["mix"].1.as_deref(), Some("40 / 60"));
    h.click(h.rect("phantom").center());
    h.frame(vec![]);
    assert_eq!(h.app.names["dialog-cancel"].0, "Cancel");
    assert_eq!(h.app.names["dialog-accept"].0, "Switch on");
    let h = Harness::new("error");
    assert_eq!(h.app.names["retry"].0, "Try again");
}

// --- motion -------------------------------------------------------------------------------------------------------------

#[test]
fn the_pointer_press_scales_to_0_97_in_100_ms_and_back_in_160() {
    let mut h = Harness::new("ready");
    let c = h.rect("mute").center();
    h.press(c);
    h.advance(0.2);
    assert!((h.scale_of("mute") - 0.97).abs() < 1e-4, "{}", h.scale_of("mute"));
    h.release(c);
    h.advance(0.3);
    assert_eq!(h.scale_of("mute"), 1.0);
}

#[test]
fn text_buttons_and_switches_press_too() {
    let mut h = Harness::new("ready");
    let c = h.rect("clipguard").center();
    h.press(c);
    h.advance(0.2);
    assert!((h.scale_of("clipguard") - 0.97).abs() < 1e-4);
    h.release(c);
    let mut e = Harness::new("error");
    let c = e.rect("retry").center();
    e.press(c);
    e.advance(0.2);
    assert!((e.scale_of("retry") - 0.97).abs() < 1e-4);
}

#[test]
fn the_keyboard_never_scales_anything() {
    let mut h = Harness::new("ready");
    h.tab_to("mute");
    for _ in 0..3 {
        let down = Event::Key { key: Key::Space, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE };
        h.frame(vec![down]);
        assert_eq!(h.scale_of("mute"), 1.0);
        h.frame(vec![]);
    }
    h.tab_to("clipguard");
    h.key(Key::Space);
    assert_eq!(h.scale_of("clipguard"), 1.0);
}

#[test]
fn reduced_motion_removes_the_press_scale_and_the_switch_slide() {
    let mut h = Harness::with("ready", WINDOW, true);
    let c = h.rect("mute").center();
    h.press(c);
    h.advance(0.05);
    assert_eq!(h.scale_of("mute"), 1.0);
    h.release(c);
    let clip = h.rect("clipguard").center();
    h.click(clip);
    assert!(!h.app.clipguard.on);
    assert_eq!(h.knob("clipguard"), 0.0, "the knob jumps");
}

#[test]
fn the_switch_knob_slides_150_ms_for_a_click_and_jumps_for_a_key() {
    let mut h = Harness::new("ready");
    assert_eq!(h.knob("low_z"), 0.0);
    let c = h.rect("low_z").center();
    h.click(c);
    // A few frames after the click the knob is on its way.
    h.advance(0.04);
    let mid = h.knob("low_z");
    assert!(mid > 0.0 && mid < 1.0, "knob at {mid}");
    h.advance(0.2);
    assert_eq!(h.knob("low_z"), 1.0);
    assert!((anim::SWITCH_S - 0.150).abs() < 1e-9);
    // A key press jumps: no slide at all.
    h.tab_to("low_z");
    h.key(Key::Space);
    assert_eq!(h.knob("low_z"), 0.0, "off again at once");
    assert!(!h.app.thumbs["low_z"].active(h.t));
}

#[test]
fn the_knob_slides_between_the_ends_during_a_click() {
    let mut h = Harness::new("ready");
    let c = h.rect("clipguard").center();
    h.press(c);
    h.frame(vec![Event::PointerButton { pos: c, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE }]);
    // A few frames after the click the knob has left the end it was on but has not arrived.
    h.advance(0.05);
    let v = h.knob("clipguard");
    assert!(v < 1.0 && v > 0.0, "knob at {v}");
}

#[test]
fn faders_thumbs_readouts_and_the_mix_never_animate() {
    let mut h = Harness::new("ready");
    let grab = thumb_pos(&h, "gain", h.app.gain.steps, 150);
    h.press(grab);
    h.frame(vec![Event::PointerMoved(Pos2::new(grab.x, grab.y - 30.0))]);
    // The value is already the new one in the same frame: there is no tween to wait for.
    assert_ne!(h.app.gain.steps, 77);
    for name in ["gain", "headphone", "mix"] {
        assert!(!h.app.thumbs.contains_key(name), "{name} must not have an animation");
        assert!(!h.app.presses.any_active(h.t) || h.app.presses.scale_of(wid(name), h.t) == 1.0);
    }
}

#[test]
fn the_dialog_fades_in_over_200_ms_from_96_percent_and_out_over_150() {
    let mut h = Harness::new("ready");
    assert_eq!(h.app.dialog_duration(true), 0.2);
    assert_eq!(h.app.dialog_duration(false), 0.15);
    assert_eq!(dialog_scale(0.0, false), 0.96, "never from scale 0");
    assert_eq!(dialog_scale(1.0, false), 1.0);
    assert!(dialog_scale(0.5, false) > 0.96 && dialog_scale(0.5, false) < 1.0);
    h.click(h.rect("phantom").center());
    let d = h.app.dialog.as_ref().unwrap();
    assert!(d.t.active(h.t), "still fading in");
    assert!(d.t.value(h.t) < 1.0);
    h.advance(0.25);
    let d = h.app.dialog.as_ref().unwrap();
    assert!(!d.t.active(h.t));
    assert_eq!(d.t.value(h.t), 1.0);
    // Out: it starts to leave at once and is gone after 150 ms.
    h.key(Key::Escape);
    let d = h.app.dialog.as_ref().unwrap();
    assert!(d.closing && d.t.active(h.t));
    h.advance(0.2);
    assert!(h.app.dialog.is_none());
}

#[test]
fn the_dialog_can_be_reopened_while_it_closes() {
    let mut h = Harness::new("ready");
    let switch = h.rect("phantom").center();
    h.click(switch);
    h.advance(0.3);
    h.key(Key::Escape);
    let value_when_closing = h.app.dialog.as_ref().unwrap().t.value(h.t);
    assert!(value_when_closing < 1.0 || h.app.dialog.as_ref().unwrap().closing);
    h.click(switch); // the row is live again at once
    let d = h.app.dialog.as_ref().expect("still there");
    assert!(!d.closing, "turned around");
}

#[test]
fn reduced_motion_makes_the_dialog_a_plain_100_ms_fade() {
    let mut h = Harness::with("ready", WINDOW, true);
    assert_eq!(h.app.dialog_duration(true), anim::REDUCED_FADE_S);
    assert_eq!(h.app.dialog_duration(false), anim::REDUCED_FADE_S);
    assert_eq!(dialog_scale(0.0, true), 1.0, "no scale at all");
    h.click(h.rect("phantom").center());
    h.advance(0.15);
    assert_eq!(h.app.dialog.as_ref().unwrap().t.value(h.t), 1.0);
}

#[test]
fn the_status_light_crossfades_in_200_ms() {
    let mut h = Harness::new("ready");
    h.advance(0.3);
    let (_, solid) = h.app.led.shown(h.t);
    assert_eq!(solid, 1.0);
    h.key(Key::Num3); // not connected: a ring
    let (_, solid) = h.app.led.shown(h.t);
    assert!(solid > 0.0 && solid < 1.0 || h.app.led.tween.active(h.t), "crossfading, solid = {solid}");
    h.advance(0.25);
    let (_, solid) = h.app.led.shown(h.t);
    assert_eq!(solid, 0.0);
    assert_eq!(anim::LIGHT_S, 0.2);
}

#[test]
fn the_meter_shows_what_the_source_pushes() {
    let h = Harness::new("ready");
    // The demo holds -30 dB: the level and the peak are there.
    assert_eq!(h.app.ball.level, -30.0);
    assert_eq!(h.app.ball.peak, -30.0);
}

#[test]
fn an_idle_window_asks_for_no_more_frames() {
    let mut h = Harness::new("ready");
    h.advance(1.0);
    h.frame(vec![]);
    assert_eq!(h.last_repaint, Duration::MAX, "nothing moves, so nothing repaints");
    // The same in a state without a device.
    let mut a = Harness::new("absent");
    a.advance(1.0);
    a.frame(vec![]);
    assert_eq!(a.last_repaint, Duration::MAX);
}

#[test]
fn a_moving_meter_repaints_at_most_20_times_a_second() {
    let mut h = Harness::sweeping("ready");
    h.advance(0.5); // the status light has finished its crossfade
    for _ in 0..40 {
        h.frame(vec![]);
        assert!(h.last_repaint >= Duration::from_millis(50), "next frame asked for after {:?}", h.last_repaint);
    }
    assert_eq!(h.last_repaint, Duration::from_millis(50));
}

#[test]
fn a_pressed_button_keeps_the_frames_coming_until_it_is_back() {
    let mut h = Harness::new("ready");
    let c = h.rect("mute").center();
    h.press(c);
    h.frame(vec![]);
    assert_eq!(h.last_repaint, Duration::ZERO, "the press is animating");
    h.advance(0.3);
    h.release(c);
    h.advance(0.4);
    assert_eq!(h.last_repaint, Duration::MAX);
}

// --- states and look -----------------------------------------------------------------------------------------------------

#[test]
fn the_meter_is_empty_while_muted() {
    let mut h = Harness::new("ready");
    h.key(Key::M);
    assert!(h.app.mute.on);
    // The window still follows the signal, only the bar and the hold mark are not drawn (see view::draw_meter).
    assert_eq!(h.app.ball.level, -30.0);
}

#[test]
fn the_device_line_has_no_serial_number() {
    let device = DeviceInfoDto { name: "Wave XLR".into(), firmware: "1.3.5".into(), serial: "ABC123".into() };
    assert_eq!(device_line_text(Some(&device)), "Wave XLR \u{b7} Firmware 1.3.5");
    assert_eq!(device_line_text(None), "Wave XLR");
    let no_fw = DeviceInfoDto { name: "Wave XLR".into(), firmware: String::new(), serial: "ABC".into() };
    assert_eq!(device_line_text(Some(&no_fw)), "Wave XLR");
    let nameless = DeviceInfoDto { name: " ".into(), firmware: "1.0".into(), serial: String::new() };
    assert_eq!(device_line_text(Some(&nameless)), "Wave XLR \u{b7} Firmware 1.0");
}

#[test]
fn the_theme_can_be_forced() {
    let ctx = Context::default();
    let light = App::new(ctx.clone(), &Options { demo: true, theme: Some("light".into()), ..Options::default() });
    assert_eq!(light.scheme, Scheme::Light);
    assert_eq!(light.pal.bg, Palette::load(Scheme::Light).bg);
    let dark = App::new(ctx, &Options { demo: true, theme: Some("dark".into()), ..Options::default() });
    assert_eq!(dark.scheme, Scheme::Dark);
}

#[test]
fn long_text_never_breaks_the_layout() {
    for state in ["long-names", "long-error"] {
        let h = Harness::new(state);
        let col = h.rect("start_login");
        assert_eq!(col.width(), 198.0, "{state}");
        assert!(h.rect("gain").height() > 100.0, "{state}");
    }
    // The long error needs three lines: the faders give back 68 - 18 px.
    assert_eq!(Harness::new("long-error").rect("gain").height(), 228.0 - 50.0);
    // At the minimum size everything still fits.
    let min = Harness::with("long-error", WINDOW_MIN, false);
    assert!(min.rect("gain").height() >= 100.0);
}

#[test]
fn the_hint_does_not_wrap_early() {
    // "Dial no longer changes" (and the space after it) has to fit on the first line at 440 wide, as in the design,
    // and must not fit at the 428 minimum.
    let ctx = Context::default();
    draw::install_fonts(&ctx);
    let mut width = 0.0;
    let mut output = ctx.run_ui(RawInput::default(), |ui| {
        width = draw::text_width(ui.painter(), "Dial no longer changes ", HINT); // egui keeps the space with the word
    });
    output.textures_delta.clear();
    let available = 198.0 - 2.0 * M - theme::SWITCH_W - XS;
    assert!(width <= available, "{width} does not fit {available}");
    assert!(width > 192.0 - 2.0 * M - theme::SWITCH_W - XS, "{width} would also fit at the minimum width");
}

#[test]
fn parses_the_command_line() {
    let args: Vec<String> = ["wavexlr", "window", "--demo", "--state=muted", "--meter=-9.5", "--dialog", "--theme=light"].map(String::from).to_vec();
    let o = parse_args(&args);
    assert!(o.demo && o.dialog);
    assert_eq!(o.state.as_deref(), Some("muted"));
    assert_eq!(o.meter, Some(-9.5));
    assert_eq!(o.theme.as_deref(), Some("light"));
    assert_eq!(parse_args(&["wavexlr".to_owned(), "window".to_owned()]), Options::default());
}

#[test]
fn the_dialog_flag_opens_the_question_at_start() {
    let ctx = Context::default();
    draw::install_fonts(&ctx);
    let app = App::new(ctx, &Options { demo: true, dialog: true, ..Options::default() });
    assert!(app.dialog.is_some());
}
