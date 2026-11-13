//! StatusNotifierItem tray icon (ksni, blocking API). The icon is drawn here as ARGB pixmaps in the panel's
//! text colour, because Plasma does not recolour a symbolic icon for every tray.

use crate::controller::State;
use ksni::blocking::{Handle, TrayMethods};
use ksni::menu::{CheckmarkItem, StandardItem};
use ksni::{Category, Icon, MenuItem, Status, ToolTip};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

/// What the tray reports back to the daemon. The callbacks run on the tray's thread, so they must only post an event.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TrayAction {
    ToggleWindow,
    OpenWindow,
    SetMuted(bool),
    Reconnect,
    Quit,
}

pub type Actions = Arc<dyn Fn(TrayAction) + Send + Sync>;

/// What the tray shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrayView {
    pub state: State,
    /// Gain, headphones and muted, only in the Ready state.
    pub levels: Option<(f64, f64, bool)>,
}

impl TrayView {
    pub fn muted(&self) -> bool {
        self.levels.is_some_and(|(_, _, muted)| muted)
    }

    pub fn status_text(&self) -> &'static str {
        match self.state {
            State::Absent => "Wave XLR not connected",
            State::Initializing => "Setting up the Wave XLR …",
            State::Ready => "Wave XLR ready",
            State::Error => "Problem with the Wave XLR",
        }
    }

    fn glyph(&self) -> Glyph {
        match self.state {
            State::Ready if self.muted() => Glyph::Muted,
            State::Ready => Glyph::Ready,
            State::Error => Glyph::Error,
            State::Absent | State::Initializing => Glyph::Absent, // dimmed
        }
    }
}

// --- drawing -----------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
enum Glyph {
    Ready,
    Muted,
    Absent,
    Error,
}

const SIZES: [u32; 3] = [22, 32, 44];
const GRID: f64 = 22.0;
const CENTRE: (f64, f64) = (11.0, 11.5);
const RADIUS: f64 = 8.5;
const STROKE: f64 = 2.0;
/// A 270 degree level arc, open at the bottom like the range of a dial (degrees, 0 = right, 90 = up).
const ARC: (f64, f64) = (225.0, -45.0);
/// The pointer starts at the ring's centre and points to 1 o'clock (30 degrees clockwise from up).
const POINTER_ANGLE: f64 = 30.0;
const POINTER_LENGTH: f64 = 3.8;
/// Muted: the slash runs through the centre at 45 degrees, this far along each way.
const SLASH_REACH: f64 = 6.9;
/// Error: the dot sits outside the ring at the top right and the arc opens around it.
const ERROR_DOT: (f64, f64, f64) = (18.9, 4.9, 2.3);
const ABSENT_ALPHA: f64 = 0.45;
const SAMPLES: u32 = 4;

enum Shape {
    /// Arc on the level ring from `start` down to `end` degrees, round caps.
    Arc(f64, f64),
    Dot(f64, f64, f64),
    /// Segment with round caps.
    Line((f64, f64), (f64, f64)),
}

fn on_ring(angle_degrees: f64) -> (f64, f64) {
    let a = angle_degrees.to_radians();
    (CENTRE.0 + RADIUS * a.cos(), CENTRE.1 - RADIUS * a.sin())
}

fn distance(a: (f64, f64), b: (f64, f64)) -> f64 {
    (a.0 - b.0).hypot(a.1 - b.1)
}

impl Shape {
    fn contains(&self, p: (f64, f64)) -> bool {
        let half = STROKE / 2.0;
        match *self {
            Shape::Dot(x, y, r) => distance(p, (x, y)) <= r,
            Shape::Line(a, b) => {
                let (dx, dy) = (b.0 - a.0, b.1 - a.1);
                let t = (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
                distance(p, (a.0 + t * dx, a.1 + t * dy)) <= half
            }
            Shape::Arc(start, end) => {
                let angle = (CENTRE.1 - p.1).atan2(p.0 - CENTRE.0).to_degrees();
                let in_range = [-360.0, 0.0, 360.0].iter().any(|k| (end..=start).contains(&(angle + k)));
                (in_range && (distance(p, CENTRE) - RADIUS).abs() <= half)
                    || distance(p, on_ring(start)) <= half
                    || distance(p, on_ring(end)) <= half
            }
        }
    }
}

fn pointer() -> Shape {
    let a = POINTER_ANGLE.to_radians();
    Shape::Line(CENTRE, (CENTRE.0 + POINTER_LENGTH * a.sin(), CENTRE.1 - POINTER_LENGTH * a.cos()))
}

/// Which colour a part of the glyph takes: the level arc carries the state colour, the rest the panel's text colour.
#[derive(Clone, Copy, PartialEq)]
enum Role {
    Text,
    Accent,
    Alert,
}

/// The geometry of the tray icons on the 22 px grid.
fn shapes(glyph: Glyph) -> Vec<(Shape, Role)> {
    use Role::{Accent, Alert, Text};
    match glyph {
        Glyph::Ready => vec![(Shape::Arc(ARC.0, ARC.1), Accent), (pointer(), Text)],
        Glyph::Absent => vec![(Shape::Arc(ARC.0, ARC.1), Text), (pointer(), Text)],
        Glyph::Muted => {
            // No pointer; the slash breaks the arc, which steps aside on both sides of it.
            let r = SLASH_REACH;
            vec![
                (Shape::Arc(ARC.0, 150.0), Alert),
                (Shape::Arc(121.0, -21.0), Alert),
                (Shape::Line((CENTRE.0 - r, CENTRE.1 - r), (CENTRE.0 + r, CENTRE.1 + r)), Alert),
            ]
        }
        Glyph::Error => {
            // The pointer stays; a solid dot at the top right, the arc opens around it.
            vec![
                (Shape::Arc(ARC.0, 77.0), Role::Accent),
                (Shape::Arc(0.0, ARC.1), Role::Accent),
                (pointer(), Text),
                (Shape::Dot(ERROR_DOT.0, ERROR_DOT.1, ERROR_DOT.2), Alert),
            ]
        }
    }
}

/// The colours of one icon set: the panel's text colour, the colour of a healthy or warning arc, and the alert colour.
#[derive(Clone, Copy)]
struct Palette {
    text: [u8; 3],
    accent: [u8; 3],
    alert: [u8; 3],
}

impl Palette {
    fn mono(rgb: [u8; 3]) -> Self {
        Self { text: rgb, accent: rgb, alert: rgb }
    }

    fn for_scheme(dark_desktop: bool) -> Self {
        // The lime of the logo as the level arc, the same tokens as the window.
        if dark_desktop {
            Self { text: [0xfc, 0xfc, 0xfc], accent: [0xb4, 0xdc, 0x4c], alert: [0xf0, 0x5a, 0x4e] }
        } else {
            Self { text: [0x23, 0x26, 0x29], accent: [0x4a, 0x85, 0x10], alert: [0xc8, 0x35, 0x2b] }
        }
    }

    fn colour(&self, role: Role) -> [u8; 3] {
        match role {
            Role::Text => self.text,
            Role::Accent => self.accent,
            Role::Alert => self.alert,
        }
    }
}

/// One square image, ARGB32 in network byte order (bytes A, R, G, B), 4x4 supersampled.
fn draw(glyph: Glyph, size: u32, rgb: [u8; 3]) -> Vec<u8> {
    draw_coloured(glyph, size, Palette::mono(rgb))
}

fn draw_coloured(glyph: Glyph, size: u32, palette: Palette) -> Vec<u8> {
    let shapes = shapes(glyph);
    let alpha = if glyph == Glyph::Absent { ABSENT_ALPHA } else { 1.0 };
    let scale = GRID / f64::from(size);
    let mut data = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let (mut hits, mut sum) = (0u32, [0u32; 3]);
            for sy in 0..SAMPLES {
                for sx in 0..SAMPLES {
                    let px = (f64::from(x) + (f64::from(sx) + 0.5) / f64::from(SAMPLES)) * scale;
                    let py = (f64::from(y) + (f64::from(sy) + 0.5) / f64::from(SAMPLES)) * scale;
                    // The later shape wins where two overlap (the pointer's root sits on nothing else).
                    if let Some((_, role)) = shapes.iter().rev().find(|(s, _)| s.contains((px, py))) {
                        hits += 1;
                        let c = palette.colour(*role);
                        for i in 0..3 {
                            sum[i] += u32::from(c[i]);
                        }
                    }
                }
            }
            let coverage = f64::from(hits) / f64::from(SAMPLES * SAMPLES);
            data.push((coverage * alpha * 255.0).round() as u8);
            if hits == 0 {
                data.extend_from_slice(&palette.text);
            } else {
                data.extend(sum.iter().map(|v| (v / hits) as u8));
            }
        }
    }
    data
}

fn icons(glyph: Glyph, dark_desktop: bool) -> Vec<Icon> {
    let palette = Palette::for_scheme(dark_desktop);
    SIZES.iter().map(|&size| Icon { width: size as i32, height: size as i32, data: draw_coloured(glyph, size, palette) }).collect()
}

// --- desktop colour scheme ---------------------------------------------------------------------

fn kdeglobals_path() -> PathBuf {
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"),
    };
    base.join("kdeglobals")
}

/// True when the desktop colour scheme is dark: its name says Dark, else the window background is dark.
/// Unknown counts as dark.
pub fn is_dark_scheme(kdeglobals: &str) -> bool {
    let (mut section, mut name, mut background) = (String::new(), String::new(), None);
    for line in kdeglobals.lines() {
        let line = line.trim();
        if let Some(header) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = header.to_string();
        } else if let Some((key, value)) = line.split_once('=') {
            match (section.as_str(), key.trim()) {
                ("General", "ColorScheme") => name = value.trim().to_string(),
                ("Colors:Window", "BackgroundNormal") => {
                    let parts: Vec<f64> = value.split(',').filter_map(|p| p.trim().parse().ok()).collect();
                    if parts.len() >= 3 {
                        background = Some(0.299 * parts[0] + 0.587 * parts[1] + 0.114 * parts[2]);
                    }
                }
                _ => {}
            }
        }
    }
    if name.to_lowercase().contains("dark") {
        return true;
    }
    background.is_none_or(|luminance| luminance < 128.0)
}

/// Re-reads ~/.config/kdeglobals only when its modification time changed.
pub struct ThemeWatch {
    path: PathBuf,
    modified: Option<SystemTime>,
    dark: bool,
}

impl ThemeWatch {
    pub fn new() -> Self {
        let mut watch = ThemeWatch { path: kdeglobals_path(), modified: None, dark: true };
        watch.dark_desktop();
        watch
    }

    pub fn dark_desktop(&mut self) -> bool {
        let modified = std::fs::metadata(&self.path).and_then(|m| m.modified()).ok();
        if modified != self.modified {
            self.modified = modified;
            self.dark = std::fs::read_to_string(&self.path).map_or(true, |text| is_dark_scheme(&text));
        }
        self.dark
    }
}

// --- the tray ----------------------------------------------------------------------------------

struct WaveTray {
    view: TrayView,
    dark_desktop: bool,
    icons: Vec<Icon>,
    drawn: (Glyph, bool),
    actions: Actions,
}

impl WaveTray {
    fn new(view: TrayView, dark_desktop: bool, actions: Actions) -> Self {
        let glyph = view.glyph();
        WaveTray { view, dark_desktop, icons: icons(glyph, dark_desktop), drawn: (glyph, dark_desktop), actions }
    }

    fn set(&mut self, view: TrayView, dark_desktop: bool) {
        self.view = view;
        self.dark_desktop = dark_desktop;
        let wanted = (view.glyph(), dark_desktop);
        if wanted != self.drawn {
            self.icons = icons(wanted.0, wanted.1);
            self.drawn = wanted;
        }
    }
}

impl ksni::Tray for WaveTray {
    fn id(&self) -> String {
        "wavexlr".into()
    }

    fn title(&self) -> String {
        "Wave XLR".into()
    }

    fn category(&self) -> Category {
        Category::Hardware
    }

    fn status(&self) -> Status {
        Status::Active
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        self.icons.clone()
    }

    fn tool_tip(&self) -> ToolTip {
        let description = match self.view.levels {
            Some((gain, headphones, _)) => format!("Gain {gain:.1} dB, headphones {headphones:.1} dB"),
            None => String::new(),
        };
        ToolTip { icon_name: String::new(), icon_pixmap: Vec::new(), title: self.view.status_text().into(), description }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        (self.actions)(TrayAction::ToggleWindow);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        vec![
            StandardItem { label: self.view.status_text().into(), enabled: false, ..Default::default() }.into(),
            CheckmarkItem {
                label: "Mute microphone".into(),
                enabled: self.view.state == State::Ready,
                checked: self.view.muted(),
                activate: Box::new(|tray: &mut Self| (tray.actions)(TrayAction::SetMuted(!tray.view.muted()))),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Open window".into(),
                activate: Box::new(|tray: &mut Self| (tray.actions)(TrayAction::OpenWindow)),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Reconnect Wave XLR".into(),
                activate: Box::new(|tray: &mut Self| (tray.actions)(TrayAction::Reconnect)),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Quit".into(),
                activate: Box::new(|tray: &mut Self| (tray.actions)(TrayAction::Quit)),
                ..Default::default()
            }
            .into(),
        ]
    }
}

pub struct Tray(Handle<WaveTray>);

impl Tray {
    /// Registers the icon. The panel may not be up yet at login, so a missing watcher is not an error:
    /// the icon appears as soon as the watcher does.
    pub fn spawn(view: TrayView, dark_desktop: bool, actions: Actions) -> Result<Tray, String> {
        WaveTray::new(view, dark_desktop, actions)
            .assume_sni_available(true)
            .spawn()
            .map(Tray)
            .map_err(|error| error.to_string())
    }

    pub fn update(&self, view: TrayView, dark_desktop: bool) {
        self.0.update(|tray| tray.set(view, dark_desktop));
    }

    pub fn shutdown(self) {
        self.0.shutdown().wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alpha_at(data: &[u8], size: u32, x: u32, y: u32) -> u8 {
        data[((y * size + x) * 4) as usize]
    }

    fn total_alpha(data: &[u8]) -> u32 {
        data.chunks(4).map(|px| u32::from(px[0])).sum()
    }

    #[test]
    fn pixmaps_have_the_right_size_and_colour() {
        for icon in icons(Glyph::Ready, true) {
            assert_eq!(icon.data.len(), (icon.width * icon.height * 4) as usize);
        }
        // the level arc is lime, the pointer takes the panel's text colour (44 px: top of the arc, middle of the pointer)
        let dark = &icons(Glyph::Ready, true)[2].data;
        assert_eq!(dark[((6 * 44 + 22) * 4 + 1) as usize..][..3], [0xb4, 0xdc, 0x4c]);
        assert_eq!(dark[((19 * 44 + 23) * 4 + 1) as usize..][..3], [0xfc, 0xfc, 0xfc]);
        let light = &icons(Glyph::Ready, false)[2].data;
        assert_eq!(light[((6 * 44 + 22) * 4 + 1) as usize..][..3], [0x4a, 0x85, 0x10]);
        assert_eq!(light[((19 * 44 + 23) * 4 + 1) as usize..][..3], [0x23, 0x26, 0x29]);
        assert_eq!(icons(Glyph::Ready, true).iter().map(|i| i.width).collect::<Vec<_>>(), vec![22, 32, 44]);
    }

    #[test]
    fn ready_glyph_has_arc_and_pointer_but_an_open_bottom() {
        let data = draw(Glyph::Ready, 44, [0; 3]);
        // top of the arc (11, 3) in the 22 grid -> pixel (22, 6)
        assert_eq!(alpha_at(&data, 44, 22, 6), 255);
        // the pointer: its middle (11.75, 9.75) -> pixel (23, 19), and its root at the centre -> (22, 23)
        assert_eq!(alpha_at(&data, 44, 23, 19), 255);
        assert_eq!(alpha_at(&data, 44, 22, 23), 255);
        // nothing at the old centre pin positions left of the pointer, nor in the corner
        assert_eq!(alpha_at(&data, 44, 15, 27), 0);
        assert_eq!(alpha_at(&data, 44, 2, 2), 0);
        // the arc is open at the bottom: (11, 20) in the grid
        assert_eq!(alpha_at(&data, 44, 22, 40), 0);
        // and the round caps of its two ends are there: 225 degrees -> grid (4.99, 17.5)
        assert!(alpha_at(&data, 44, 10, 35) > 200);
    }

    #[test]
    fn absent_is_the_ready_glyph_at_45_percent() {
        let ready = draw(Glyph::Ready, 44, [0; 3]);
        let absent = draw(Glyph::Absent, 44, [0; 3]);
        let expected = (0.45_f64 * 255.0).round() as u8;
        assert_eq!(alpha_at(&absent, 44, 22, 6), expected);
        assert_eq!(alpha_at(&absent, 44, 23, 19), expected);
        assert!(total_alpha(&ready) > total_alpha(&absent));
    }

    #[test]
    fn muted_has_a_slash_and_no_pointer() {
        let ready = draw(Glyph::Ready, 44, [0; 3]);
        let muted = draw(Glyph::Muted, 44, [0; 3]);
        // the slash passes (7.25, 7.75), which is empty in Ready (inside the arc, away from the pointer)
        assert_eq!(alpha_at(&ready, 44, 14, 15), 0);
        assert_eq!(alpha_at(&muted, 44, 14, 15), 255);
        // the pointer is gone
        assert_eq!(alpha_at(&ready, 44, 23, 19), 255);
        assert_eq!(alpha_at(&muted, 44, 23, 19), 0);
        // the arc is broken: at the lower right it stops short of the slash (-33 degrees -> grid (18.1, 16.1))
        assert_eq!(alpha_at(&ready, 44, 36, 32), 255);
        assert_eq!(alpha_at(&muted, 44, 36, 32), 0);
        assert_ne!(total_alpha(&ready), total_alpha(&muted));
    }

    #[test]
    fn error_has_a_dot_at_the_top_right_and_an_open_arc() {
        let ready = draw(Glyph::Ready, 44, [0; 3]);
        let error = draw(Glyph::Error, 44, [0; 3]);
        // the dot is outside the ring: grid (18.9, 4.9) -> pixel (37, 9)
        assert_eq!(alpha_at(&error, 44, 37, 9), 255);
        assert_eq!(alpha_at(&ready, 44, 37, 9), 0);
        // the arc has a gap near the dot: grid (18.75, 7.75) -> pixel (37, 15)
        assert_eq!(alpha_at(&ready, 44, 37, 15), 255);
        assert_eq!(alpha_at(&error, 44, 37, 15), 0);
        // the pointer is still there
        assert_eq!(alpha_at(&error, 44, 23, 19), 255);
        // the opening in the ring is the only other difference at the top of the arc
        assert_eq!(alpha_at(&error, 44, 22, 6), 255);
    }

    #[test]
    fn the_glyphs_keep_a_margin_at_every_size() {
        // (the error dot may touch the right edge by design, as in the mockup)
        for glyph in [Glyph::Ready, Glyph::Muted, Glyph::Absent] {
            for size in SIZES {
                let data = draw(glyph, size, [0; 3]);
                // nothing touches the outermost pixel rows and columns
                let edge = (0..size).flat_map(|i| [(i, 0), (i, size - 1), (0, i), (size - 1, i)]);
                assert!(edge.into_iter().all(|(x, y)| alpha_at(&data, size, x, y) == 0), "{glyph:?} at {size}");
            }
        }
    }

    #[test]
    fn colour_scheme_by_name() {
        assert!(is_dark_scheme("[General]\nColorScheme=BreezeDark\n"));
        assert!(!is_dark_scheme("[General]\nColorScheme=BreezeLight\n[Colors:Window]\nBackgroundNormal=239,240,241\n"));
    }

    #[test]
    fn colour_scheme_by_window_background() {
        assert!(is_dark_scheme("[Colors:Window]\nBackgroundNormal=35,38,41\n"));
        assert!(!is_dark_scheme("[General]\nColorScheme=Breeze\n[Colors:Window]\nBackgroundNormal=239,240,241\n"));
    }

    #[test]
    fn colour_scheme_defaults_to_dark() {
        assert!(is_dark_scheme(""));
    }

    #[test]
    fn tooltip_and_status_texts() {
        let ready = TrayView { state: State::Ready, levels: Some((36.75, -28.0, false)) };
        assert_eq!(ready.status_text(), "Wave XLR ready");
        assert_eq!(ready.glyph(), Glyph::Ready);
        let muted = TrayView { state: State::Ready, levels: Some((36.75, -28.0, true)) };
        assert_eq!(muted.glyph(), Glyph::Muted);
        assert_eq!(TrayView { state: State::Initializing, levels: None }.glyph(), Glyph::Absent);
        assert_eq!(TrayView { state: State::Error, levels: None }.glyph(), Glyph::Error);
    }
}
