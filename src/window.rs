//! The Wave XLR window: an egui process that is a thin client of the daemon socket (see `ipc.rs`).
//! It paints every control itself, runs the live meter through libpulse-simple (loaded at runtime) while the device
//! is ready, and has a `--demo` mode with fake data to check the look without the device.

mod anim;
mod client;
mod controls;
mod demo;
mod draw;
mod meter;
mod model;
mod theme;
#[cfg(test)]
mod ui_tests;
mod view;

use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

use eframe::egui::{self, accesskit, Align2, Color32, Context, Event, Id, Key, Modifiers, Pos2, Rect, Theme, Ui, Vec2};

use crate::ipc::{self, Command, DeviceInfoDto, SettingsDto, Snapshot};
use anim::Tween;
use controls::{Axis, Presses, SliderCtl, Toggle};
use demo::Demo;
use draw::{fade, BODY, BUTTON, HINT, TITLE};
use meter::LiveMeter;
use model::{Ballistics, Light, Screen};
use theme::{Palette, Scheme, BUTTON_H, COLUMN_GAP, DIALOG_PAD, DIALOG_W, L, M, MUTE_H, PAD_SIDE, PAD_TOP, S, XS};
use view::{ButtonKind, ButtonView, FaderView, Look, MeterView, MixView, SwitchView};

/// A stable id for one control.
fn wid(name: &'static str) -> Id {
    Id::unique(("wavexlr", name))
}

const WINDOW: Vec2 = Vec2::new(theme::WINDOW_W, theme::WINDOW_H);
const WINDOW_MIN: Vec2 = Vec2::new(theme::MIN_W, theme::MIN_H);

const MIC_HINT_48V: &str = "Condenser mics only";
const MIC_HINT_LOCK: &str = "Dial no longer changes the gain";
const DIALOG_BODY: &str = "Use 48 V only with a condenser microphone. It can damage ribbon microphones and some dynamic microphones. Switch it off before you plug in a different microphone.";

/// How long "Trying \u{2026}" stays on the retry button if no answer comes.
const RETRY_LOADING_S: f64 = 4.0;

/// What the command line asked for. Only `--demo` and its helpers matter; the daemon starts this process with
/// the plain `window` subcommand.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Options {
    pub demo: bool,
    /// `--state=ready|muted|absent|initializing|error|noservice|long-error|long-names`
    pub state: Option<String>,
    /// `--meter=-9.5`: hold the fake meter at one level (for pictures).
    pub meter: Option<f64>,
    /// `--dialog`: open the 48 V dialog at start.
    pub dialog: bool,
    /// `--theme=light|dark`: force one theme (also `WAVEXLR_THEME`).
    pub theme: Option<String>,
}

pub fn parse_args(args: &[String]) -> Options {
    let mut options = Options::default();
    for arg in args {
        if arg == "--demo" {
            options.demo = true;
        } else if let Some(v) = arg.strip_prefix("--state=") {
            options.state = Some(v.to_owned());
        } else if let Some(v) = arg.strip_prefix("--meter=") {
            options.meter = v.replace('\u{2212}', "-").parse().ok();
        } else if arg == "--dialog" {
            options.dialog = true;
        } else if let Some(v) = arg.strip_prefix("--theme=") {
            options.theme = Some(v.to_owned());
        }
    }
    options
}

pub fn run() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--meter-probe") {
        std::process::exit(meter::probe());
    }
    let options = parse_args(&args);
    let zoom = zoom_from_env();
    let size = size_from_env().unwrap_or(WINDOW);
    let native = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Wave XLR")
            .with_app_id("wavexlr")
            .with_inner_size(size * zoom)
            .with_min_inner_size(WINDOW_MIN * zoom),
        ..Default::default()
    };
    let result = eframe::run_native(
        "Wave XLR",
        native,
        Box::new(move |cc| {
            draw::install_fonts(&cc.egui_ctx);
            cc.egui_ctx.set_zoom_factor(zoom);
            Ok(Box::new(App::new(cc.egui_ctx.clone(), &options)))
        }),
    );
    if let Err(error) = result {
        eprintln!("wavexlr window: {error}");
        std::process::exit(1);
    }
}

/// `WAVEXLR_SCALE=1.5` zooms everything (and the window) by that factor on top of the desktop scale. For pictures.
fn zoom_from_env() -> f32 {
    std::env::var("WAVEXLR_SCALE").ok().and_then(|v| v.parse::<f32>().ok()).filter(|s| (0.25..=4.0).contains(s)).unwrap_or(1.0)
}

/// `WAVEXLR_SIZE=428x560` opens the window at that size (logical px). For pictures of the minimum size.
fn size_from_env() -> Option<Vec2> {
    let text = std::env::var("WAVEXLR_SIZE").ok()?;
    let (w, h) = text.split_once('x')?;
    let size = Vec2::new(w.parse().ok()?, h.parse().ok()?);
    (size.x >= WINDOW_MIN.x && size.y >= WINDOW_MIN.y).then_some(size)
}

// --- state ---------------------------------------------------------------------------------------------------------

enum Link {
    Connecting,
    Down,
    Up(Snapshot),
}

enum Backend {
    Live { client: client::Client, events: Receiver<client::Event>, link: Link },
    Demo(Demo),
}

/// The colour and fill of the state light, animated between looks.
struct LedAnim {
    from: (Color32, f32),
    to: (Color32, f32),
    tween: Tween,
    /// Has it shown a look yet? The first one appears without a fade.
    primed: bool,
}

impl LedAnim {
    fn shown(&self, now: f64) -> (Color32, f32) {
        let t = self.tween.value(now);
        (self.from.0.lerp_to_gamma(self.to.0, t), self.from.1 + (self.to.1 - self.from.1) * t)
    }

    fn aim(&mut self, target: (Color32, f32), now: f64, reduced: bool) {
        if !self.primed {
            *self = LedAnim { from: target, to: target, tween: Tween::new(1.0), primed: true };
        } else if target != self.to {
            self.from = self.shown(now);
            self.to = target;
            self.tween.jump(0.0);
            self.tween.go(1.0, now, if reduced { anim::REDUCED_FADE_S } else { anim::LIGHT_S });
        }
    }
}

/// The 48 V question. `t` is how visible it is (0 to 1): it eases in over 200 ms and out over 150 ms.
struct Dialog {
    t: Tween,
    closing: bool,
    /// Cancel has taken the keyboard focus (it is the default button).
    focused: bool,
}

pub struct App {
    ctx: Context,
    backend: Backend,
    forced_scheme: Option<Scheme>,
    scheme: Scheme,
    pal: Palette,
    system_theme: Option<Theme>,
    reduced: bool,
    focus_visible: bool,
    gain: SliderCtl,
    headphone: SliderCtl,
    /// The PC share: 0 is the left end (all microphone).
    mix: SliderCtl,
    mute: Toggle,
    phantom: Toggle,
    clipguard: Toggle,
    gain_lock: Toggle,
    low_z: Toggle,
    start_login: Toggle,
    start_min: Toggle,
    presses: Presses,
    thumbs: HashMap<&'static str, Tween>,
    /// Switches that were just toggled by key: their knob jumps in the next frame instead of sliding.
    jump_next: std::collections::HashSet<&'static str>,
    led: LedAnim,
    ball: Ballistics,
    live_meter: Option<LiveMeter>,
    dialog: Option<Dialog>,
    /// After the dialog closes, the 48 V switch gets the focus back.
    refocus_phantom: bool,
    /// When "Try again" was pressed: it reads "Trying \u{2026}" until the next answer or a few seconds.
    retry_at: Option<f64>,
    /// The device controls were live in the last frame, to jump (not slide) the switches when that changes.
    was_live: bool,
    #[cfg(test)]
    probes: HashMap<&'static str, Rect>,
    #[cfg(test)]
    names: HashMap<&'static str, (String, Option<String>)>,
}

/// Per-frame facts the painting code needs.
#[derive(Clone, Copy)]
struct FrameInfo {
    now: f64,
    /// The device is ready: its controls show values and work.
    ready: bool,
    /// The daemon is reachable: the app rows work.
    app_live: bool,
    /// The window answers input (not while the 48 V question is open).
    interactive: bool,
    /// The device controls just came or went: switches jump instead of sliding.
    jump: bool,
}

fn scheme_from_name(name: &str) -> Option<Scheme> {
    match name {
        "light" => Some(Scheme::Light),
        "dark" => Some(Scheme::Dark),
        _ => None,
    }
}

impl App {
    pub fn new(ctx: Context, options: &Options) -> App {
        let forced_scheme = options.theme.as_deref().and_then(scheme_from_name);
        let scheme = forced_scheme.unwrap_or_else(|| Scheme::detect(None));
        let pal = Palette::load(scheme);
        let backend = if options.demo {
            Backend::Demo(Demo::new(options.state.as_deref(), options.meter))
        } else {
            let (tx, rx) = channel();
            let wake = ctx.clone();
            let client = client::Client::start(ipc::socket_path(), tx, move || wake.request_repaint(), client::RETRY);
            Backend::Live { client, events: rx, link: Link::Connecting }
        };
        let mut app = App {
            ctx,
            backend,
            forced_scheme,
            scheme,
            pal,
            system_theme: None,
            reduced: theme::reduced_motion(),
            focus_visible: false,
            gain: SliderCtl::new(model::GAIN),
            headphone: SliderCtl::new(model::HEADPHONE),
            mix: SliderCtl::new(model::MIX),
            mute: Toggle::default(),
            phantom: Toggle::default(),
            clipguard: Toggle::default(),
            gain_lock: Toggle::default(),
            low_z: Toggle::default(),
            start_login: Toggle::default(),
            start_min: Toggle::default(),
            presses: Presses::default(),
            thumbs: HashMap::new(),
            jump_next: std::collections::HashSet::new(),
            led: LedAnim { from: (pal.text_tertiary, 0.0), to: (pal.text_tertiary, 0.0), tween: Tween::new(1.0), primed: false },
            ball: Ballistics::default(),
            live_meter: None,
            dialog: None,
            refocus_phantom: false,
            retry_at: None,
            was_live: false,
            #[cfg(test)]
            probes: HashMap::new(),
            #[cfg(test)]
            names: HashMap::new(),
        };
        if options.dialog {
            app.open_dialog(0.0);
        }
        app.apply_style();
        app
    }

    fn apply_style(&self) {
        let mut visuals = if self.scheme == Scheme::Light { egui::Visuals::light() } else { egui::Visuals::dark() };
        visuals.window_fill = self.pal.elevated;
        visuals.override_text_color = Some(self.pal.text);
        self.ctx.set_visuals(visuals);
    }

    /// Tests find controls by name.
    #[cfg(test)]
    fn probe(&mut self, name: &'static str, rect: Rect) {
        self.probes.insert(name, rect);
    }

    #[cfg(not(test))]
    fn probe(&mut self, _name: &'static str, _rect: Rect) {}

    fn screen(&self) -> Screen {
        match &self.backend {
            Backend::Live { link: Link::Connecting, .. } => Screen::Connecting,
            Backend::Live { link: Link::Down, .. } => Screen::NoService,
            Backend::Live { link: Link::Up(snapshot), .. } => Screen::from_snapshot(snapshot),
            Backend::Demo(demo) => demo.screen(),
        }
    }

    fn device(&self) -> Option<DeviceInfoDto> {
        match &self.backend {
            Backend::Live { link: Link::Up(snapshot), .. } => snapshot.device.clone(),
            Backend::Live { .. } => None,
            Backend::Demo(demo) => demo.device(),
        }
    }

    fn settings(&self) -> SettingsDto {
        match &self.backend {
            Backend::Live { link: Link::Up(snapshot), .. } => snapshot.settings,
            Backend::Live { .. } => SettingsDto::default(),
            Backend::Demo(demo) => demo.settings(),
        }
    }

    fn send(&mut self, command: Command) {
        match &mut self.backend {
            Backend::Live { client, .. } => {
                client.send(&command);
            }
            Backend::Demo(demo) => demo.apply(&command),
        }
    }

    fn poll_events(&mut self) {
        let mut answered = false;
        if let Backend::Live { events, link, .. } = &mut self.backend {
            while let Ok(event) = events.try_recv() {
                answered = true;
                *link = match event {
                    client::Event::Snapshot(snapshot) => Link::Up(snapshot),
                    client::Event::Disconnected => Link::Down,
                };
            }
        }
        if answered {
            self.retry_at = None;
        }
    }

    /// Duration of the dialog's fade: 200 ms in, 150 ms out, and 100 ms for both with reduced motion.
    fn dialog_duration(&self, opening: bool) -> f64 {
        if self.reduced {
            anim::REDUCED_FADE_S
        } else if opening {
            anim::DIALOG_IN_S
        } else {
            anim::DIALOG_OUT_S
        }
    }

    fn open_dialog(&mut self, now: f64) {
        let duration = self.dialog_duration(true);
        match &mut self.dialog {
            Some(dialog) => {
                // Opened again while it was still closing: turn around from where it is.
                dialog.closing = false;
                dialog.t.go(1.0, now, duration);
            }
            None => {
                let mut t = Tween::new(0.0);
                t.go(1.0, now, duration);
                self.dialog = Some(Dialog { t, closing: false, focused: false });
            }
        }
    }

    fn close_dialog(&mut self, now: f64) {
        let duration = self.dialog_duration(false);
        if let Some(dialog) = &mut self.dialog {
            dialog.closing = true;
            dialog.t.go(0.0, now, duration);
        }
        self.refocus_phantom = true;
    }

    fn dialog_blocks_input(&self) -> bool {
        self.dialog.as_ref().is_some_and(|d| !d.closing)
    }

    /// Theme, scale and the "focus ring only after Tab" rule.
    fn begin_frame(&mut self, ctx: &Context) {
        let system = ctx.system_theme();
        if system != self.system_theme && self.forced_scheme.is_none() {
            self.system_theme = system;
            let scheme = Scheme::detect(system.map(|t| if t == Theme::Light { Scheme::Light } else { Scheme::Dark }));
            if scheme != self.scheme {
                self.scheme = scheme;
                self.pal = Palette::load(scheme);
                self.apply_style();
            }
        }
        ctx.input(|i| {
            for event in &i.events {
                match event {
                    Event::PointerButton { pressed: true, .. } => self.focus_visible = false,
                    Event::Key {
                        pressed: true,
                        key:
                            Key::Tab
                            | Key::ArrowUp
                            | Key::ArrowDown
                            | Key::ArrowLeft
                            | Key::ArrowRight
                            | Key::PageUp
                            | Key::PageDown
                            | Key::Space
                            | Key::Enter,
                        ..
                    } => self.focus_visible = true,
                    _ => {}
                }
            }
        });
    }

    /// Bring the controls in line with what the device says, and start or stop the meter.
    fn sync(&mut self, screen: &Screen, now: f64) {
        match screen {
            Screen::Ready(levels) => {
                self.gain.adopt_steps(model::GAIN.steps_for(levels.gain_db), now);
                self.headphone.adopt_steps(model::HEADPHONE.steps_for(levels.headphone_db), now);
                self.mix.adopt_steps(model::mix_steps_from_mic_percent(levels.mic_mix_percent), now);
                self.mute.adopt(levels.muted, now);
                self.phantom.adopt(levels.phantom, now);
                self.clipguard.adopt(levels.clipguard, now);
                self.gain_lock.adopt(levels.gain_lock, now);
                self.low_z.adopt(levels.low_z, now);
            }
            _ => {
                self.gain.cancel();
                self.headphone.cancel();
                self.mix.cancel();
                self.dialog = None;
            }
        }
        if !matches!(screen, Screen::Connecting | Screen::NoService) {
            let settings = self.settings();
            self.start_login.adopt(settings.start_at_login, now);
            self.start_min.adopt(settings.start_minimised, now);
        }
        let want_live_meter = screen.is_ready() && matches!(self.backend, Backend::Live { .. });
        if want_live_meter && self.live_meter.is_none() {
            let wake = self.ctx.clone();
            self.live_meter = Some(LiveMeter::start(move || wake.request_repaint()));
        } else if !want_live_meter {
            self.live_meter = None; // dropping it stops the reader thread (waits at most 200 ms)
        }
        if !screen.is_ready() {
            self.ball.reset();
        }
    }

    /// Feed the meter and ask for the next frame only while it moves: 20 frames a second at most.
    fn feed_meter(&mut self, screen: &Screen, now: f64) {
        if !screen.is_ready() {
            return;
        }
        let sample = match (&mut self.backend, &self.live_meter) {
            (Backend::Demo(demo), _) => demo.meter_sample(now),
            (_, Some(live)) => live.take(),
            _ => None,
        };
        if let Some(db) = sample {
            self.ball.push(db, now, self.reduced);
        }
        if self.ball.animating() {
            self.ball.tick(now, self.reduced);
        }
        if self.ball.animating() {
            self.ctx.request_repaint_after(Duration::from_millis(50));
        }
        if matches!(&self.backend, Backend::Demo(demo) if demo.is_sweeping()) {
            self.ctx.request_repaint_after(Duration::from_millis(50));
        }
    }

    /// The knob position of a switch, 0 to 1. It slides over 150 ms after a click and jumps for keys, reduced
    /// motion and when the whole device section changes.
    fn thumb(&mut self, key: &'static str, on: bool, now: f64, instant: bool) -> f32 {
        let instant = self.jump_next.remove(key) || instant;
        let tween = self.thumbs.entry(key).or_insert_with(|| Tween::new(if on { 1.0 } else { 0.0 }));
        tween.go(if on { 1.0 } else { 0.0 }, now, if instant || self.reduced { 0.0 } else { anim::SWITCH_S });
        if tween.active(now) {
            self.ctx.request_repaint();
        }
        tween.value(now)
    }

    /// Tell the accessibility tree what a control is. Faders and the mix report their value as the description.
    #[allow(clippy::too_many_arguments)]
    fn describe(&mut self, name: &'static str, role: accesskit::Role, label: &str, description: Option<String>, toggled: Option<bool>, enabled: bool) {
        #[cfg(test)]
        self.names.insert(name, (label.to_owned(), description.clone()));
        #[cfg(not(test))]
        let _ = name;
        let id = wid(name);
        self.ctx.accesskit_node_builder(id, |node| {
            node.set_role(role);
            node.set_label(label);
            if let Some(text) = description {
                node.set_description(text);
            }
            if let Some(on) = toggled {
                node.set_toggled(if on { accesskit::Toggled::True } else { accesskit::Toggled::False });
            }
            if !enabled {
                node.set_disabled();
            }
        });
    }

    // --- one frame ---

    /// Paint and handle one frame. `ui` covers the whole window.
    pub fn show(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let now = ctx.input(|i| i.time);
        self.begin_frame(&ctx);
        self.poll_events();
        let screen = self.screen();
        self.sync(&screen, now);
        self.feed_meter(&screen, now);
        if self.dialog.as_ref().is_some_and(|d| d.closing && !d.t.active(now)) {
            self.dialog = None;
        }

        let window = ui.max_rect();
        let pal = self.pal;
        let look = Look { p: &pal, painter: ui.painter().clone(), ppp: ctx.pixels_per_point() };
        look.painter.rect_filled(window, 0, pal.bg);
        if matches!(screen, Screen::Connecting) {
            return;
        }

        // Keys that work anywhere: M for mute (not while the dialog is open); the demo switches states with 1 to 8.
        let ready = screen.is_ready();
        let interactive = !self.dialog_blocks_input();
        if ready && interactive && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::M)) {
            let on = !self.mute.on;
            self.mute.set(on, now);
            self.send(Command::SetMuted { value: on });
        }
        if let Backend::Demo(demo) = &mut self.backend {
            for (n, key) in [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8].into_iter().enumerate() {
                if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, key)) {
                    demo.set_kind(n);
                }
            }
        }

        let jump = ready != self.was_live;
        self.was_live = ready;
        let f = FrameInfo { now, ready, app_live: !matches!(screen, Screen::NoService), interactive, jump };
        let device = self.device();
        let status_h = self.status(ui, &look, window, &screen, device.as_ref(), f);

        // Rows whose hint wraps longer than designed make their group taller; the faders give that room back.
        let col_w = model::column_width(window.width());
        let hint_w = col_w - 2.0 * M - theme::SWITCH_W - XS;
        let lines_48v = draw::line_count(&look.painter, MIC_HINT_48V, HINT, hint_w);
        let lines_lock = draw::line_count(&look.painter, MIC_HINT_LOCK, HINT, hint_w);
        let (h_48v, h_lock) = (model::row_height(lines_48v), model::row_height(lines_lock));
        let extra = (h_48v - model::row_height(1)).max(0.0) + (h_lock - model::row_height(2)).max(0.0);
        let area = model::fader_area(window.height(), status_h, extra);

        let top = (window.top() + PAD_TOP + status_h + COLUMN_GAP).round();
        let mic_x = window.left() + PAD_SIDE;
        let phones_x = window.right() - PAD_SIDE - col_w;
        let mic = Rect::from_min_size(Pos2::new(mic_x, top), Vec2::new(col_w, 297.0 + area + extra));
        let phones = Rect::from_min_size(Pos2::new(phones_x, top), Vec2::new(col_w, 197.0 + area));
        self.mic_group(ui, &look, mic, area, (h_48v, h_lock), f);
        self.phones_group(ui, &look, phones, area, f);

        let app = Rect::from_min_size(Pos2::new(phones_x, phones.bottom() + COLUMN_GAP), Vec2::new(col_w, 2.0 * theme::ROW_H + 1.0));
        self.app_group(ui, &look, app, f);
        self.device_line(&look, Rect::from_min_size(Pos2::new(phones_x, app.bottom() + S), Vec2::new(col_w, 16.0)), device.as_ref());

        if self.dialog.is_some() {
            self.dialog_ui(ui, &look, window, now);
        }

        if self.presses.any_active(now) {
            ctx.request_repaint();
        }
    }

    /// The status line at the top. Returns its height.
    fn status(&mut self, ui: &Ui, look: &Look, window: Rect, screen: &Screen, device: Option<&DeviceInfoDto>, f: FrameInfo) -> f32 {
        let p = look.p;
        let has_retry = matches!(screen, Screen::Error(_));
        if self.retry_at.is_some_and(|t| f.now - t >= RETRY_LOADING_S) || !has_retry {
            self.retry_at = None;
        }
        let loading = self.retry_at.is_some();
        // The button keeps one width for "Try again" and "Trying \u{2026}", so nothing jumps.
        let retry_w = draw::text_width(&look.painter, "Try again", BUTTON).max(draw::text_width(&look.painter, "Trying \u{2026}", BUTTON)) + 2.0 * M;
        let text_left = window.left() + PAD_SIDE + theme::LED + S;
        let text_right = window.right() - PAD_SIDE - if has_retry { retry_w + L } else { 0.0 };
        let width = (text_right - text_left).max(80.0);

        let title = draw::layout_one_line(&look.painter, &screen.status_text(device.map(|d| d.name.as_str())), BUTTON, p.text, width);
        let hint_text = screen.hint();
        let hint = (!hint_text.is_empty()).then(|| draw::layout(&look.painter, &hint_text, HINT, p.text_secondary, width));
        let height = model::status_height(hint.as_ref().map_or(0, |h| h.rows.len().max(1)), has_retry);
        let top = window.top() + PAD_TOP;

        // The state light: 8 x 18 box, the light on the title line.
        let target = match screen.light() {
            Light::Ready => (p.accent, 1.0),
            Light::Idle => (p.text_tertiary, 0.0),
            Light::Busy => (p.warning, 0.0),
            Light::Problem => (p.danger, 1.0),
        };
        self.led.aim(target, f.now, self.reduced);
        let (color, solid) = self.led.shown(f.now);
        if self.led.tween.active(f.now) {
            self.ctx.request_repaint();
        }
        view::draw_led(look, Pos2::new(window.left() + PAD_SIDE + theme::LED / 2.0, top + 9.0), color, solid);

        look.painter.galley(Pos2::new(text_left, top), title, p.text);
        if let Some(hint) = hint {
            look.painter.galley(Pos2::new(text_left, top + 18.0 + 2.0), hint, p.text_secondary);
        }

        if has_retry {
            let rect = Rect::from_min_size(Pos2::new(window.right() - PAD_SIDE - retry_w, top), Vec2::new(retry_w, BUTTON_H));
            let hit = Rect::from_center_size(rect.center(), Vec2::new(retry_w + 2.0 * S, theme::MIN_HIT));
            let id = wid("retry");
            self.probe("retry", hit);
            let press = self.presses.handle(ui, id, hit, !loading && f.interactive, f.now, self.reduced);
            view::draw_text_button(
                look,
                &ButtonView {
                    rect,
                    label: if loading { "Trying \u{2026}" } else { "Try again" },
                    scale: press.scale,
                    down: press.down,
                    enabled: true,
                    ring: self.focus_visible && press.focused,
                    loading,
                    kind: ButtonKind::Secondary,
                    opacity: 1.0,
                },
            );
            self.describe("retry", accesskit::Role::Button, "Try again", None, None, !loading);
            if press.clicked {
                self.retry_at = Some(f.now);
                self.send(Command::Retry);
            }
            if let Some(at) = self.retry_at {
                // Back to "Try again" if no answer comes.
                self.ctx.request_repaint_after(Duration::from_secs_f64((RETRY_LOADING_S - (f.now - at)).max(0.0) + 0.05));
            }
        }
        height
    }

    /// A group's head: the title, an aside on the right, and the readout under it.
    fn group_head(&self, look: &Look, group: Rect, title: &str, aside: Option<(String, Color32)>, readout: Option<f64>) {
        let p = look.p;
        draw::text(&look.painter, Pos2::new(group.left() + M, group.top() + M), Align2::LEFT_TOP, title, TITLE, p.text);
        if let Some((text, color)) = aside {
            draw::text(&look.painter, Pos2::new(group.right() - M, group.top() + M + 9.0), Align2::RIGHT_CENTER, &text, HINT, color);
        }
        view::draw_readout(look, Pos2::new(group.left() + M, group.top() + M + 18.0 + XS), readout);
    }

    fn mic_group(&mut self, ui: &Ui, look: &Look, group: Rect, area: f32, row_heights: (f32, f32), f: FrameInfo) {
        let p = look.p;
        view::draw_group(look, group);
        let muted = f.ready && self.mute.on;
        let aside = if muted {
            Some(("Muted".to_owned(), p.danger_text))
        } else if f.ready && self.ball.peak > model::FLOOR_DB + 0.1 {
            Some((format!("Peak {}", model::format_db(self.ball.peak)), p.text_tertiary))
        } else {
            None
        };
        self.group_head(look, group, "Microphone", aside, f.ready.then(|| self.gain.value()));

        // Fader and meter side by side, centred in the group.
        let area_top = group.top() + 74.0 + M;
        let x0 = (group.center().x - view::MIC_PAIR_W / 2.0).round();
        let fader = Rect::from_min_size(Pos2::new(x0, area_top), Vec2::new(theme::FADER_FRAME, area));
        let meter = Rect::from_min_size(Pos2::new(x0 + theme::FADER_FRAME + S, area_top), Vec2::new(theme::METER_FRAME, area));

        let (_, track_top, track_bottom) = view::fader_track(fader);
        self.probe("gain", fader);
        let touch = self.gain.interact(ui, wid("gain"), fader, Axis::Vertical, track_bottom, track_top, f.now, f.ready && f.interactive);
        if let Some(value) = touch.send {
            self.send(Command::SetGain { value });
        }
        view::draw_fader(
            look,
            &FaderView {
                frame: fader,
                marks: &[0.0, 15.0, 30.0, 45.0, 60.0, 75.0],
                range: model::GAIN,
                steps: f.ready.then_some(self.gain.steps),
                ring: self.focus_visible && touch.focused,
            },
        );
        view::draw_meter(look, &MeterView { frame: meter, ballistics: &self.ball, live: f.ready && !self.mute.on });
        let gain_text = format!("{} dB", model::format_db(self.gain.value()));
        self.describe("gain", accesskit::Role::Slider, "Microphone gain", f.ready.then_some(gain_text), None, f.ready);

        // Mute: 32 px high, 12 px of hit area above and below.
        let mute_rect = Rect::from_min_size(Pos2::new(group.left() + M, area_top + area + M), Vec2::new(group.width() - 2.0 * M, MUTE_H));
        self.probe("mute", mute_rect.expand2(Vec2::new(M, M)));
        let press = self.presses.handle(ui, wid("mute"), mute_rect.expand2(Vec2::new(M, M)), f.ready && f.interactive, f.now, self.reduced);
        if press.clicked {
            let on = !self.mute.on;
            self.mute.set(on, f.now);
            self.send(Command::SetMuted { value: on });
        }
        view::draw_mute(
            look,
            &ButtonView {
                rect: mute_rect,
                label: "",
                scale: press.scale,
                down: press.down,
                enabled: f.ready,
                ring: self.focus_visible && press.focused,
                loading: false,
                kind: ButtonKind::Secondary,
                opacity: 1.0,
            },
            muted,
        );
        self.describe("mute", accesskit::Role::Button, "Mute", None, Some(muted), f.ready);

        // Rows: separator, 48 V, separator, Clipguard, separator, Gain lock.
        let mut y = mute_rect.bottom() + M;
        view::draw_separator(look, group, y);
        y += 1.0;
        let on = self.phantom.on && f.ready;
        if self.switch_row(ui, look, group, y, row_heights.0, &RowSpec { name: "phantom", label: "48 V", hint: Some(MIC_HINT_48V), on, on_color: p.warning, enabled: f.ready }, f) {
            if self.phantom.on {
                self.phantom.set(false, f.now);
                self.send(Command::SetPhantom { value: false });
            } else {
                // 48 V always asks first.
                self.open_dialog(f.now);
            }
        }
        if self.refocus_phantom && f.interactive {
            self.refocus_phantom = false;
            ui.memory_mut(|m| m.request_focus(wid("phantom")));
        }
        y += row_heights.0;
        view::draw_separator(look, group, y);
        y += 1.0;
        let on = self.clipguard.on && f.ready;
        if self.switch_row(ui, look, group, y, theme::ROW_H, &RowSpec { name: "clipguard", label: "Clipguard", hint: None, on, on_color: p.accent, enabled: f.ready }, f) {
            let on = !self.clipguard.on;
            self.clipguard.set(on, f.now);
            self.send(Command::SetClipguard { value: on });
        }
        y += theme::ROW_H;
        view::draw_separator(look, group, y);
        y += 1.0;
        let on = self.gain_lock.on && f.ready;
        if self.switch_row(ui, look, group, y, row_heights.1, &RowSpec { name: "gain_lock", label: "Gain lock", hint: Some(MIC_HINT_LOCK), on, on_color: p.accent, enabled: f.ready }, f) {
            let on = !self.gain_lock.on;
            self.gain_lock.set(on, f.now);
            self.send(Command::SetGainLock { value: on });
        }
    }

    fn phones_group(&mut self, ui: &Ui, look: &Look, group: Rect, area: f32, f: FrameInfo) {
        let p = look.p;
        view::draw_group(look, group);
        self.group_head(look, group, "Headphones", None, f.ready.then(|| self.headphone.value()));

        let area_top = group.top() + 74.0 + M;
        let x0 = (group.center().x - theme::FADER_FRAME / 2.0).round();
        let fader = Rect::from_min_size(Pos2::new(x0, area_top), Vec2::new(theme::FADER_FRAME, area));
        let (_, track_top, track_bottom) = view::fader_track(fader);
        self.probe("headphone", fader);
        let touch = self.headphone.interact(ui, wid("headphone"), fader, Axis::Vertical, track_bottom, track_top, f.now, f.ready && f.interactive);
        if let Some(value) = touch.send {
            self.send(Command::SetHeadphone { value });
        }
        view::draw_fader(
            look,
            &FaderView {
                frame: fader,
                marks: &[0.0, -12.0, -24.0, -36.0, -48.0, -60.0],
                range: model::HEADPHONE,
                steps: f.ready.then_some(self.headphone.steps),
                ring: self.focus_visible && touch.focused,
            },
        );
        let volume_text = format!("{} dB", model::format_db(self.headphone.value()));
        self.describe("headphone", accesskit::Role::Slider, "Headphone volume", f.ready.then_some(volume_text), None, f.ready);

        // The Mic/PC mix: label row, 8 px, slider row.
        let block = Rect::from_min_size(Pos2::new(group.left() + M, area_top + area + M), Vec2::new(group.width() - 2.0 * M, 46.0));
        let (_, lo, hi) = view::mix_track(block);
        let slider_row = Rect::from_min_size(Pos2::new(block.left(), block.top() + 26.0), Vec2::new(block.width(), 20.0));
        let hit = Rect::from_center_size(slider_row.center(), Vec2::new(slider_row.width(), theme::MIN_HIT));
        self.probe("mix", hit);
        let touch = self.mix.interact(ui, wid("mix"), hit, Axis::Horizontal, lo, hi, f.now, f.ready && f.interactive);
        if let Some(pc) = touch.send {
            self.send(Command::SetMicMix { value: model::mic_percent_from_mix_steps(model::MIX.steps_for(pc)) });
        }
        view::draw_mix(
            look,
            &MixView { block, steps: f.ready.then_some(self.mix.steps), ring: self.focus_visible && touch.focused, enabled: f.ready },
        );
        let mix_text = model::format_mix(self.mix.steps);
        self.describe("mix", accesskit::Role::Slider, "Mic/PC mix", f.ready.then_some(mix_text), None, f.ready);

        let sep_y = block.bottom() + M;
        view::draw_separator(look, group, sep_y);
        let on = self.low_z.on && f.ready;
        if self.switch_row(ui, look, group, sep_y + 1.0, theme::ROW_H, &RowSpec { name: "low_z", label: "Low impedance", hint: None, on, on_color: p.accent, enabled: f.ready }, f) {
            let on = !self.low_z.on;
            self.low_z.set(on, f.now);
            self.send(Command::SetLowZ { value: on });
        }
    }

    fn app_group(&mut self, ui: &Ui, look: &Look, group: Rect, f: FrameInfo) {
        let p = look.p;
        view::draw_group(look, group);
        let enabled = f.app_live;
        let on = self.start_login.on;
        if self.switch_row(ui, look, group, group.top(), theme::ROW_H, &RowSpec { name: "start_login", label: "Start at login", hint: None, on, on_color: p.accent, enabled }, f) {
            let on = !self.start_login.on;
            self.start_login.set(on, f.now);
            self.send(Command::SetStartAtLogin { value: on });
        }
        view::draw_separator(look, group, group.top() + theme::ROW_H);
        let on = self.start_min.on;
        if self.switch_row(ui, look, group, group.top() + theme::ROW_H + 1.0, theme::ROW_H, &RowSpec { name: "start_min", label: "Start minimised", hint: None, on, on_color: p.accent, enabled }, f) {
            let on = !self.start_min.on;
            self.start_min.set(on, f.now);
            self.send(Command::SetStartMinimised { value: on });
        }
    }

    /// "Wave XLR · Firmware 1.3.5" under the App group: one line, an ellipsis at the end, no serial number.
    fn device_line(&self, look: &Look, rect: Rect, device: Option<&DeviceInfoDto>) {
        let text = device_line_text(device);
        let galley = draw::layout_one_line(&look.painter, &text, HINT, look.p.text_tertiary, rect.width() - M);
        look.painter.galley(Pos2::new(rect.left() + M, rect.top()), galley, look.p.text_tertiary);
    }

    /// One row with a label, an optional hint and a switch on the right. The whole row is the hit area. Returns true
    /// when it was clicked or toggled by key. Painting only; the caller applies the change.
    #[allow(clippy::too_many_arguments)]
    fn switch_row(&mut self, ui: &Ui, look: &Look, group: Rect, y: f32, h: f32, row: &RowSpec, f: FrameInfo) -> bool {
        let p = look.p;
        let rect = Rect::from_min_size(Pos2::new(group.left(), y), Vec2::new(group.width(), h));
        self.probe(row.name, rect);
        let press = self.presses.handle(ui, wid(row.name), rect, row.enabled && f.interactive, f.now, self.reduced);
        let amount = self.thumb(row.name, row.on, f.now, f.jump);
        if press.by_key {
            // The state changes after this frame; its knob then jumps instead of sliding.
            self.jump_next.insert(row.name);
        }

        let alpha = if row.enabled { 1.0 } else { view::DISABLED };
        let text_w = group.width() - 2.0 * M - theme::SWITCH_W - XS;
        let hint = row.hint.map(|h| draw::layout(&look.painter, h, HINT, fade(p.text_secondary, alpha), text_w));
        let text_h = 18.0 + hint.as_ref().map_or(0.0, |h| h.size().y);
        let top = rect.top() + ((h - text_h) / 2.0).max(0.0);
        let label = draw::layout(&look.painter, row.label, BODY, fade(p.text, alpha), text_w);
        look.painter.galley(Pos2::new(rect.left() + M, top), label, p.text);
        if let Some(hint) = hint {
            look.painter.galley(Pos2::new(rect.left() + M, top + 18.0), hint, p.text_secondary);
        }
        view::draw_switch(
            look,
            &SwitchView {
                pill: view::switch_pill(rect),
                amount,
                scale: press.scale,
                enabled: row.enabled,
                ring: self.focus_visible && press.focused,
                on_color: row.on_color,
            },
        );
        self.describe(row.name, accesskit::Role::Switch, row.label, row.hint.map(str::to_owned), Some(row.on), row.enabled);
        press.clicked
    }

    /// The 48 V question. Cancel is the default button: Enter and Escape both cancel.
    fn dialog_ui(&mut self, ui: &Ui, look: &Look, window: Rect, now: f64) {
        let p = look.p;
        let Some((t, closing)) = self.dialog.as_ref().map(|d| (d.t.value(now), d.closing)) else { return };
        if self.dialog.as_ref().is_some_and(|d| d.t.active(now)) {
            self.ctx.request_repaint();
        }
        // Dim the window behind the card.
        look.painter.rect_filled(window, 0, fade(p.scrim, t));

        let card_w = (window.width() - 2.0 * L).min(DIALOG_W);
        let icon = 28.0;
        let text_left = DIALOG_PAD + icon + L;
        let text_w = card_w - text_left - DIALOG_PAD;
        let title = draw::layout(&look.painter, "Switch on 48 V?", TITLE, fade(p.text, t), text_w);
        let body = draw::layout(&look.painter, DIALOG_BODY, HINT, fade(p.text_secondary, t), text_w);
        let content_h = (18.0 + XS + body.size().y).max(icon);
        let card_h = DIALOG_PAD + content_h + DIALOG_PAD + BUTTON_H + DIALOG_PAD;
        // The card grows from 96 % of its size around its centre, never from nothing. With reduced motion it only fades.
        let scale = dialog_scale(t, self.reduced);
        let full = Rect::from_center_size(window.center(), Vec2::new(card_w, card_h));
        let card = Rect::from_center_size(full.center(), full.size() * scale);
        draw::drop_shadow(&look.painter, card, theme::R_GROUP, 12, 32, fade(Color32::from_black_alpha(0x59), t));
        look.painter.rect_filled(card, draw::radius(theme::R_GROUP), fade(p.elevated, t));
        let map = |q: Pos2| full.center() + (q - full.center()) * scale;

        let origin = full.min;
        draw::warning_glyph(
            &look.painter,
            Rect::from_min_size(map(origin + Vec2::new(DIALOG_PAD, DIALOG_PAD)), Vec2::splat(icon * scale)),
            fade(p.warning, t),
            fade(p.on_accent, t),
        );
        look.painter.galley(map(origin + Vec2::new(text_left, DIALOG_PAD)), title, p.text);
        look.painter.galley(map(origin + Vec2::new(text_left, DIALOG_PAD + 18.0 + XS)), body, p.text_secondary);

        // Buttons, right aligned: "Switch on", then Cancel (the default) last.
        let y = origin.y + card_h - DIALOG_PAD - BUTTON_H;
        let cancel_w = draw::text_width(&look.painter, "Cancel", BUTTON) + 2.0 * M;
        let accept_w = draw::text_width(&look.painter, "Switch on", BUTTON) + 2.0 * M;
        let cancel = Rect::from_min_size(Pos2::new(origin.x + card_w - DIALOG_PAD - cancel_w, y), Vec2::new(cancel_w, BUTTON_H));
        let accept = Rect::from_min_size(Pos2::new(cancel.left() - S - accept_w, y), Vec2::new(accept_w, BUTTON_H));
        let mut accepted = false;
        let mut cancelled = !closing && ui.ctx().input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape));
        for (rect, label, id_name, is_default) in [(accept, "Switch on", "dialog-accept", false), (cancel, "Cancel", "dialog-cancel", true)] {
            let shown = Rect::from_min_size(map(rect.min), rect.size() * scale);
            let hit = Rect::from_center_size(shown.center(), Vec2::new(rect.width() + S, theme::MIN_HIT));
            self.probe(id_name, hit);
            let press = self.presses.handle(ui, wid(id_name), hit, !closing, now, self.reduced);
            if is_default && !closing && self.dialog.as_ref().is_some_and(|d| !d.focused) {
                // Cancel is the default button: it takes the focus as soon as it exists.
                ui.memory_mut(|m| m.request_focus(wid(id_name)));
                if press.focused {
                    if let Some(d) = self.dialog.as_mut() {
                        d.focused = true;
                    }
                }
            }
            view::draw_text_button(
                look,
                &ButtonView {
                    rect: shown,
                    label,
                    scale: press.scale,
                    down: press.down,
                    enabled: true,
                    ring: self.focus_visible && press.focused,
                    loading: false,
                    kind: if is_default { ButtonKind::Default } else { ButtonKind::Secondary },
                    opacity: t,
                },
            );
            self.describe(id_name, accesskit::Role::Button, label, None, None, !closing);
            if press.clicked && !closing {
                if is_default {
                    cancelled = true;
                } else {
                    accepted = true;
                }
            }
        }
        if accepted {
            self.phantom.set(true, now);
            self.send(Command::SetPhantom { value: true });
            self.close_dialog(now);
        } else if cancelled {
            self.close_dialog(now);
        }
    }
}

/// The card's scale while the dialog is `t` (0 to 1) visible: from 0.96 to 1, never from nothing. With reduced
/// motion the card only fades.
fn dialog_scale(t: f32, reduced: bool) -> f32 {
    if reduced {
        1.0
    } else {
        anim::DIALOG_FROM_SCALE + (1.0 - anim::DIALOG_FROM_SCALE) * t.clamp(0.0, 1.0)
    }
}

/// What a switch row needs to be drawn.
struct RowSpec<'a> {
    name: &'static str,
    label: &'a str,
    hint: Option<&'a str>,
    on: bool,
    on_color: Color32,
    enabled: bool,
}

/// The device line: "Wave XLR · Firmware 1.3.5"; "Wave XLR" alone without a device or a firmware. No serial number.
fn device_line_text(device: Option<&DeviceInfoDto>) -> String {
    let Some(device) = device else { return "Wave XLR".to_owned() };
    let name = if device.name.trim().is_empty() { "Wave XLR" } else { device.name.trim() };
    if device.firmware.trim().is_empty() {
        name.to_owned()
    } else {
        format!("{name} \u{b7} Firmware {}", device.firmware.trim())
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.show(ui);
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        self.pal.bg.to_normalized_gamma_f32()
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.live_meter = None;
    }
}
