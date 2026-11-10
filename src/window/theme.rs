//! Design tokens. Colours are read from `tokens.json` (compiled in), so the file stays the one source of truth.
//! Sizes are constants here; a test checks that they still match the file.

use eframe::egui::Color32;
use serde_json::Value;

static TOKENS_JSON: &str = include_str!("tokens.json");

// space
pub const XS: f32 = 4.0;
pub const S: f32 = 8.0;
pub const M: f32 = 12.0;
pub const L: f32 = 16.0;
// radius
pub const R_CONTROL: f32 = 6.0;
pub const R_GROUP: f32 = 10.0;
// window
pub const WINDOW_W: f32 = 440.0;
pub const WINDOW_H: f32 = 600.0;
pub const MIN_W: f32 = 428.0;
pub const MIN_H: f32 = 560.0;
pub const PAD_TOP: f32 = 12.0;
pub const PAD_SIDE: f32 = 16.0;
#[allow(dead_code)] // the layout ends where the columns end; kept so the token test covers every size
pub const PAD_BOTTOM: f32 = 16.0;
pub const COLUMN_GAP: f32 = 12.0;
// sizes
pub const FADER_FRAME: f32 = 54.0;
pub const METER_FRAME: f32 = 34.0;
pub const TRACK: f32 = 4.0;
pub const METER_BAR: f32 = 6.0;
pub const THUMB: f32 = 20.0;
pub const TRACK_INSET: f32 = 14.0;
pub const MUTE_H: f32 = 32.0;
pub const BUTTON_H: f32 = 28.0;
pub const SWITCH_W: f32 = 34.0;
pub const SWITCH_H: f32 = 20.0;
pub const SWITCH_KNOB: f32 = 16.0;
pub const ROW_H: f32 = 40.0;
pub const DIALOG_W: f32 = 340.0;
pub const DIALOG_PAD: f32 = 20.0;
pub const LED: f32 = 8.0;
pub const RING: f32 = 2.0;
pub const RING_GAP: f32 = 2.0;
/// Hit areas are never smaller than this.
pub const MIN_HIT: f32 = 40.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scheme {
    Dark,
    Light,
}

impl Scheme {
    /// `WAVEXLR_THEME=light|dark` forces one; otherwise `system` (if the toolkit knows it) and then KDE's own colours.
    pub fn detect(system: Option<Scheme>) -> Scheme {
        match std::env::var("WAVEXLR_THEME").as_deref() {
            Ok("light") => return Scheme::Light,
            Ok("dark") => return Scheme::Dark,
            _ => {}
        }
        system.or_else(kde_scheme).unwrap_or(Scheme::Dark)
    }
}

/// KDE writes the window background colour to `~/.config/kdeglobals`; a light background means a light scheme.
fn kde_scheme() -> Option<Scheme> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(".config")))?;
    let text = std::fs::read_to_string(base.join("kdeglobals")).ok()?;
    scheme_from_kdeglobals(&text)
}

pub fn scheme_from_kdeglobals(text: &str) -> Option<Scheme> {
    let mut in_window = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_window = line == "[Colors:Window]";
        } else if in_window {
            if let Some(rgb) = line.strip_prefix("BackgroundNormal=") {
                let v: Vec<f32> = rgb.split(',').filter_map(|p| p.trim().parse().ok()).collect();
                if v.len() >= 3 {
                    let luma = 0.2126 * v[0] + 0.7152 * v[1] + 0.0722 * v[2];
                    return Some(if luma > 140.0 { Scheme::Light } else { Scheme::Dark });
                }
            }
        }
    }
    None
}

/// `KDE animation speed: instant` or `WAVEXLR_REDUCED_MOTION=1`.
pub fn reduced_motion() -> bool {
    if let Ok(value) = std::env::var("WAVEXLR_REDUCED_MOTION") {
        return !(value.is_empty() || value == "0");
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(".config")));
    let Some(text) = base.and_then(|b| std::fs::read_to_string(b.join("kdeglobals")).ok()) else {
        return false;
    };
    animations_off(&text)
}

pub fn animations_off(kdeglobals: &str) -> bool {
    let mut in_kde = false;
    for line in kdeglobals.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_kde = line == "[KDE]";
        } else if in_kde {
            if let Some(v) = line.strip_prefix("AnimationDurationFactor=") {
                return v.trim().parse::<f32>().map(|f| f == 0.0).unwrap_or(false);
            }
        }
    }
    false
}

#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub bg: Color32,
    pub group: Color32,
    pub elevated: Color32,
    pub separator: Color32,
    pub track: Color32,
    pub control_fill: Color32,
    pub thumb: Color32,
    pub thumb_edge: Color32,
    pub text: Color32,
    pub text_secondary: Color32,
    pub text_tertiary: Color32,
    pub accent: Color32,
    pub accent_pressed: Color32,
    pub on_accent: Color32,
    pub warning: Color32,
    pub danger: Color32,
    pub danger_pressed: Color32,
    pub danger_text: Color32,
    pub on_danger: Color32,
    pub button: Color32,
    pub button_pressed: Color32,
    pub scrim: Color32,
}

/// `#rrggbb` or `#rrggbbaa`. A missing or broken value shows up as loud magenta (and a test fails).
fn hex(value: &Value, name: &str) -> Color32 {
    let text = value[name].as_str().unwrap_or("#ff00ff").trim_start_matches('#');
    let n = u32::from_str_radix(text, 16).unwrap_or(0xff00ff);
    if text.len() == 8 {
        Color32::from_rgba_unmultiplied((n >> 24) as u8, (n >> 16) as u8, (n >> 8) as u8, n as u8)
    } else {
        Color32::from_rgb((n >> 16) as u8, (n >> 8) as u8, n as u8)
    }
}

impl Palette {
    pub fn load(scheme: Scheme) -> Palette {
        let tokens: Value = serde_json::from_str(TOKENS_JSON).expect("tokens.json is valid");
        let key = if scheme == Scheme::Light { "light" } else { "dark" };
        let c = &tokens["color"][key];
        Palette {
            bg: hex(c, "bg"),
            group: hex(c, "group"),
            elevated: hex(c, "elevated"),
            separator: hex(c, "separator"),
            track: hex(c, "track"),
            control_fill: hex(c, "control-fill"),
            thumb: hex(c, "thumb"),
            thumb_edge: hex(c, "thumb-edge"),
            text: hex(c, "text"),
            text_secondary: hex(c, "text-secondary"),
            text_tertiary: hex(c, "text-tertiary"),
            accent: hex(c, "accent"),
            accent_pressed: hex(c, "accent-pressed"),
            on_accent: hex(c, "on-accent"),
            warning: hex(c, "warning"),
            danger: hex(c, "danger"),
            danger_pressed: hex(c, "danger-pressed"),
            danger_text: hex(c, "danger-text"),
            on_danger: hex(c, "on-danger"),
            button: hex(c, "button"),
            button_pressed: hex(c, "button-pressed"),
            scrim: hex(c, "scrim"),
        }
    }

    /// Every colour of the palette, for checks.
    #[cfg(test)]
    pub fn all(&self) -> [Color32; 22] {
        [
            self.bg, self.group, self.elevated, self.separator, self.track, self.control_fill, self.thumb,
            self.thumb_edge, self.text, self.text_secondary, self.text_tertiary, self.accent, self.accent_pressed,
            self.on_accent, self.warning, self.danger, self.danger_pressed, self.danger_text, self.on_danger,
            self.button, self.button_pressed, self.scrim,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::window::{anim, model};

    fn tokens() -> Value {
        serde_json::from_str(TOKENS_JSON).expect("tokens.json is valid JSON")
    }

    #[test]
    fn sizes_match_the_token_file() {
        let t = tokens();
        let size = |k: &str| t["size"][k].as_f64().unwrap_or_else(|| panic!("size {k}")) as f32;
        assert_eq!((size("window-width"), size("window-height")), (WINDOW_W, WINDOW_H));
        assert_eq!((size("min-width"), size("min-height")), (MIN_W, MIN_H));
        assert_eq!((size("pad-top"), size("pad-side"), size("pad-bottom")), (PAD_TOP, PAD_SIDE, PAD_BOTTOM));
        assert_eq!(size("column-gap"), COLUMN_GAP);
        assert_eq!((size("fader-frame"), size("meter-frame")), (FADER_FRAME, METER_FRAME));
        assert_eq!((size("track"), size("meter-bar"), size("thumb")), (TRACK, METER_BAR, THUMB));
        assert_eq!(size("track-inset"), TRACK_INSET);
        assert_eq!((size("mute-height"), size("button-height")), (MUTE_H, BUTTON_H));
        assert_eq!((size("switch-width"), size("switch-height"), size("switch-knob")), (SWITCH_W, SWITCH_H, SWITCH_KNOB));
        assert_eq!(size("row"), ROW_H);
        assert_eq!(size("min-hit"), MIN_HIT);
        assert_eq!((size("dialog-width"), size("dialog-pad")), (DIALOG_W, DIALOG_PAD));
        assert_eq!(size("led"), LED);
        assert_eq!((size("focus-ring"), size("focus-gap")), (RING, RING_GAP));
        let space = |k: &str| t["space"][k].as_f64().unwrap() as f32;
        assert_eq!((space("xs"), space("s"), space("m"), space("l")), (XS, S, M, L));
        let radius = |k: &str| t["radius"][k].as_f64().unwrap() as f32;
        assert_eq!((radius("control"), radius("group")), (R_CONTROL, R_GROUP));
    }

    #[test]
    fn meter_and_motion_match_the_token_file() {
        let t = tokens();
        assert_eq!(t["meter"]["floor-db"].as_f64().unwrap(), model::FLOOR_DB);
        assert_eq!(t["meter"]["warn-db"].as_f64().unwrap(), model::WARN_DB);
        assert_eq!(t["meter"]["clip-db"].as_f64().unwrap(), model::CLIP_DB);
        assert_eq!(t["meter"]["fall-db-per-s"].as_f64().unwrap(), model::FALL_DB_PER_S);
        assert_eq!(t["meter"]["hold-ms"].as_f64().unwrap() / 1000.0, model::HOLD_S);
        let m = |k: &str| t["motion"][k].as_f64().unwrap();
        assert_eq!(m("press-scale") as f32, anim::PRESS_SCALE);
        assert_eq!(m("press-in-ms") / 1000.0, anim::PRESS_IN_S);
        assert_eq!(m("press-out-ms") / 1000.0, anim::PRESS_OUT_S);
        assert_eq!(m("switch-ms") / 1000.0, anim::SWITCH_S);
        assert_eq!(m("light-ms") / 1000.0, anim::LIGHT_S);
        assert_eq!(m("dialog-in-ms") / 1000.0, anim::DIALOG_IN_S);
        assert_eq!(m("dialog-out-ms") / 1000.0, anim::DIALOG_OUT_S);
        assert_eq!(m("dialog-scale") as f32, anim::DIALOG_FROM_SCALE);
        assert_eq!(m("reduced-fade-ms") / 1000.0, anim::REDUCED_FADE_S);
    }

    #[test]
    fn type_scale_has_the_four_sizes_of_the_design() {
        let t = tokens();
        let sizes: std::collections::BTreeSet<u64> = t["type"].as_object().unwrap().values().map(|v| v["size"].as_u64().unwrap()).collect();
        assert_eq!(sizes.into_iter().collect::<Vec<_>>(), vec![11, 12, 13, 32]);
    }

    #[test]
    fn both_palettes_load_the_v2_colours() {
        for scheme in [Scheme::Dark, Scheme::Light] {
            let p = Palette::load(scheme);
            assert!(p.all().iter().all(|c| *c != Color32::from_rgb(255, 0, 255)), "a token is missing");
        }
        let dark = Palette::load(Scheme::Dark);
        assert_eq!(dark.bg, Color32::from_rgb(0x1c, 0x1c, 0x1b));
        assert_eq!(dark.accent, Color32::from_rgb(0xb4, 0xdc, 0x4c));
        assert_eq!(dark.separator, Color32::from_rgba_unmultiplied(0xff, 0xff, 0xff, 0x17));
        assert_eq!(Palette::load(Scheme::Light).group, Color32::from_rgb(0xfb, 0xfb, 0xfa));
        assert_eq!(Palette::load(Scheme::Light).thumb_edge, Color32::from_rgba_unmultiplied(0, 0, 0, 0x33));
    }

    #[test]
    fn reads_the_kde_scheme_and_animation_speed() {
        assert_eq!(scheme_from_kdeglobals("[Colors:Window]\nBackgroundNormal=239,240,241\n"), Some(Scheme::Light));
        assert_eq!(scheme_from_kdeglobals("[Colors:Button]\nBackgroundNormal=239,240,241\n[Colors:Window]\nBackgroundNormal=35,38,41\n"), Some(Scheme::Dark));
        assert_eq!(scheme_from_kdeglobals("[General]\nColorScheme=x\n"), None);
        assert!(animations_off("[KDE]\nAnimationDurationFactor=0\n"));
        assert!(!animations_off("[KDE]\nAnimationDurationFactor=0.5\n"));
        assert!(!animations_off(""));
    }
}
