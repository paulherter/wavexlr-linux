//! Painting of the window's parts. Everything is drawn from the palette; nothing here changes state.

use eframe::egui::{Align2, Color32, Painter, Pos2, Rect, Stroke, Vec2};

use super::draw::{self, fade, radius, snap, snap_rect, BODY, BUTTON, HINT, READOUT, SCALE, UNIT};
use super::model::{self, Ballistics, Range, Zone, FLOOR_DB};
use super::theme::{self, Palette, FADER_FRAME, METER_BAR, METER_FRAME, R_CONTROL, SWITCH_H, SWITCH_KNOB, SWITCH_W, THUMB, TRACK, TRACK_INSET, XS};

/// Disabled parts are drawn at 40 % opacity.
pub const DISABLED: f32 = 0.4;

pub struct Look<'a> {
    pub p: &'a Palette,
    pub painter: Painter,
    pub ppp: f32,
}

impl Look<'_> {
    /// A hairline: 1 logical px, snapped to whole physical pixels (never thinner than one).
    pub fn hline(&self, x0: f32, x1: f32, y: f32, color: Color32) {
        let thick = self.ppp.floor().max(1.0) / self.ppp;
        let top = snap(y, self.ppp);
        self.painter.rect_filled(Rect::from_min_max(Pos2::new(snap(x0, self.ppp), top), Pos2::new(snap(x1, self.ppp), top + thick)), 0, color);
    }
}

pub fn draw_group(look: &Look, rect: Rect) {
    look.painter.rect_filled(snap_rect(rect, look.ppp), radius(theme::R_GROUP), look.p.group);
}

/// The hairline between rows, inset 12 px on both sides.
pub fn draw_separator(look: &Look, group: Rect, y: f32) {
    look.hline(group.left() + theme::M, group.right() - theme::M, y, look.p.separator);
}

// --- faders and the meter -------------------------------------------------------------------------------------------

/// Track x (the centre line) and the y of the two track ends for a fader frame. The track is inset 14 px at both
/// ends (thumb radius 10 plus 4 px of ring room); the meter uses the same inset so the scales line up.
pub fn fader_track(frame: Rect) -> (f32, f32, f32) {
    (frame.left() + 40.0, frame.top() + TRACK_INSET, frame.bottom() - TRACK_INSET)
}

pub struct FaderView<'a> {
    pub frame: Rect,
    pub marks: &'a [f64],
    pub range: Range,
    /// `None` while there is no value (not ready): only the track and the numbers are drawn.
    pub steps: Option<i32>,
    pub ring: bool,
}

pub fn draw_fader(look: &Look, v: &FaderView) {
    let p = look.p;
    let (tx, top, bottom) = fader_track(v.frame);
    for &mark in v.marks {
        let y = model::y_for(v.range.fraction(v.range.steps_for(mark)), top, bottom);
        draw::text(&look.painter, Pos2::new(v.frame.left() + 18.0, y), Align2::RIGHT_CENTER, &model::format_mark(mark), SCALE, p.text_tertiary);
    }

    let slot = Rect::from_min_max(Pos2::new(tx - TRACK / 2.0, top), Pos2::new(tx + TRACK / 2.0, bottom));
    look.painter.rect_filled(snap_rect(slot, look.ppp), radius(TRACK / 2.0), p.track);
    let Some(steps) = v.steps else { return };

    let y = model::y_for(v.range.fraction(steps), top, bottom);
    let filled = Rect::from_min_max(Pos2::new(slot.left(), y), Pos2::new(slot.right(), bottom));
    look.painter.rect_filled(snap_rect(filled, look.ppp), radius(TRACK / 2.0), p.control_fill);
    draw_thumb(look, Pos2::new(tx, y), v.ring);
}

/// The round thumb of the faders and the mix slider: 20 px, a 0.5 px inner edge and a soft shadow.
pub fn draw_thumb(look: &Look, centre: Pos2, ring: bool) {
    let p = look.p;
    let c = Pos2::new(snap(centre.x, look.ppp), snap(centre.y, look.ppp));
    let rect = Rect::from_center_size(c, Vec2::splat(THUMB));
    if ring {
        draw::focus_ring(&look.painter, rect, THUMB / 2.0, p.accent);
    }
    draw::drop_shadow(&look.painter, rect, THUMB / 2.0, 1, 3, Color32::from_black_alpha(0x59));
    look.painter.circle_filled(c, THUMB / 2.0, p.thumb);
    look.painter.circle_stroke(c, THUMB / 2.0 - 0.25, Stroke::new(0.5, p.thumb_edge));
}

pub struct MeterView<'a> {
    pub frame: Rect,
    pub ballistics: &'a Ballistics,
    /// Fill and peak hold are shown (ready and not muted).
    pub live: bool,
}

/// Where a dB level sits on the meter bar.
pub fn meter_y(db: f64, top: f32, bottom: f32) -> f32 {
    bottom - ((db.clamp(FLOOR_DB, 0.0) - FLOOR_DB) / -FLOOR_DB) as f32 * (bottom - top)
}

/// The plain bar: a track filled bottom-up in solid sections (accent, warning above -12, danger above -3) and a
/// small hold mark in the colour of its zone. No gradient, no segments.
pub fn draw_meter(look: &Look, v: &MeterView) {
    let p = look.p;
    let top = v.frame.top() + TRACK_INSET;
    let bottom = v.frame.bottom() - TRACK_INSET;
    let bar = snap_rect(Rect::from_min_max(Pos2::new(v.frame.left(), top), Pos2::new(v.frame.left() + METER_BAR, bottom)), look.ppp);
    let r = METER_BAR / 2.0;
    look.painter.rect_filled(bar, radius(r), p.track);

    if v.live && v.ballistics.level > FLOOR_DB + 0.05 {
        let level_y = snap(meter_y(v.ballistics.level, top, bottom), look.ppp);
        let warn_y = snap(meter_y(model::WARN_DB, top, bottom), look.ppp);
        let clip_y = snap(meter_y(model::CLIP_DB, top, bottom), look.ppp);
        // Clip to the filled part, so only the zone boundaries need painting.
        let painter = look.painter.with_clip_rect(Rect::from_min_max(Pos2::new(bar.left(), level_y), Pos2::new(bar.right(), bar.bottom() + 1.0)));
        let sections = [
            (bar.top(), clip_y, p.danger, [r, r, 0.0, 0.0]),
            (clip_y, warn_y, p.warning, [0.0; 4]),
            (warn_y, bar.bottom(), p.accent, [0.0, 0.0, r, r]),
        ];
        for (y0, y1, color, [nw, ne, sw, se]) in sections {
            let corner = eframe::egui::CornerRadius { nw: nw as u8, ne: ne as u8, sw: sw as u8, se: se as u8 };
            painter.rect_filled(Rect::from_min_max(Pos2::new(bar.left(), y0), Pos2::new(bar.right(), y1)), corner, color);
        }
    }
    if v.live && v.ballistics.peak > FLOOR_DB + 0.05 {
        let y = snap(meter_y(v.ballistics.peak, top, bottom), look.ppp);
        let color = match model::zone_of(v.ballistics.peak) {
            Zone::Accent => p.accent,
            Zone::Warning => p.warning,
            Zone::Danger => p.danger,
        };
        let mark = Rect::from_center_size(Pos2::new(bar.center().x, y), Vec2::new(METER_BAR, 2.0));
        look.painter.rect_filled(mark, radius(1.0), color);
    }
    for mark in [0.0, -12.0, -24.0, -36.0, -48.0, -60.0] {
        draw::text(
            &look.painter,
            Pos2::new(v.frame.left() + 14.0, meter_y(mark, top, bottom)),
            Align2::LEFT_CENTER,
            &model::format_mark(mark),
            SCALE,
            p.text_tertiary,
        );
    }
}

// --- readout ----------------------------------------------------------------------------------------------------------

/// Big level number with the small unit on its baseline. Digits are tabular so the number never shifts. `None`
/// shows a dash and a quiet unit.
pub fn draw_readout(look: &Look, top_left: Pos2, value: Option<f64>) {
    let p = look.p;
    let text = value.map_or_else(|| "\u{2013}".to_owned(), model::format_db);
    let big = draw::layout(&look.painter, &text, READOUT, if value.is_some() { p.text } else { p.text_tertiary }, f32::INFINITY);
    let width = big.size().x;
    look.painter.galley(top_left, big, p.text);
    // The unit's 18 px line box starts 18 px below the top of the 40 px readout row, 4 px right of the number.
    draw::text(
        &look.painter,
        Pos2::new(top_left.x + width + XS, top_left.y + 18.0),
        Align2::LEFT_TOP,
        "dB",
        UNIT,
        if value.is_some() { p.text_secondary } else { p.text_tertiary },
    );
}

// --- switches and buttons -----------------------------------------------------------------------------------------

pub struct SwitchView {
    /// Where the 34 x 20 pill sits (its unscaled rectangle).
    pub pill: Rect,
    /// 0 = off, 1 = on (animated).
    pub amount: f32,
    /// Press scale (0.97 while the pointer holds it).
    pub scale: f32,
    pub enabled: bool,
    pub ring: bool,
    pub on_color: Color32,
}

/// Pill and knob. While pressed the knob stretches to 20 x 16 toward the middle.
pub fn draw_switch(look: &Look, v: &SwitchView) {
    let p = look.p;
    let alpha = if v.enabled { 1.0 } else { DISABLED };
    let pill = snap_rect(v.pill, look.ppp);
    if v.ring {
        draw::focus_ring(&look.painter, pill, SWITCH_H / 2.0, p.accent);
    }
    let shown = snap_rect(draw::scaled(pill, v.scale), look.ppp);
    let amount = v.amount.clamp(0.0, 1.0);
    let h = shown.height();
    look.painter.rect_filled(shown, radius(h / 2.0), fade(p.track.lerp_to_gamma(v.on_color, amount), alpha));

    // Press stretches the knob from 16 to 20 px, 0.97 is the full press.
    let stretch = ((1.0 - v.scale) / (1.0 - super::anim::PRESS_SCALE)).clamp(0.0, 1.0);
    let knob_w = (SWITCH_KNOB + 4.0 * stretch) * v.scale;
    let knob_h = SWITCH_KNOB * v.scale;
    let inset = (SWITCH_H - SWITCH_KNOB) / 2.0 * v.scale;
    let left = shown.left() + inset + ((shown.width() - 2.0 * inset - knob_w) * amount);
    let knob = Rect::from_min_size(Pos2::new(left, shown.center().y - knob_h / 2.0), Vec2::new(knob_w, knob_h));
    draw::drop_shadow(&look.painter, knob, knob_h / 2.0, 1, 2, fade(Color32::from_black_alpha(0x40), alpha));
    look.painter.rect_filled(knob, radius(knob_h / 2.0), fade(p.thumb, alpha));
}

/// The pill rectangle for a switch whose row is `row`: 12 px from the right edge, centred vertically.
pub fn switch_pill(row: Rect) -> Rect {
    Rect::from_center_size(Pos2::new(row.right() - theme::M - SWITCH_W / 2.0, row.center().y), Vec2::new(SWITCH_W, SWITCH_H))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonKind {
    /// ui/button with text.
    Secondary,
    /// ui/accent with on-accent text: the default choice of a dialog.
    Default,
}

pub struct ButtonView<'a> {
    pub rect: Rect,
    pub label: &'a str,
    pub scale: f32,
    pub down: bool,
    pub enabled: bool,
    pub ring: bool,
    /// "Trying …": shown in the secondary text colour, not clickable.
    pub loading: bool,
    pub kind: ButtonKind,
    /// Overall opacity (the dialog fades in and out).
    pub opacity: f32,
}

/// The full-width Mute button. Red and saying "Muted" while the microphone is off.
pub fn draw_mute(look: &Look, v: &ButtonView, muted: bool) {
    let p = look.p;
    let alpha = v.opacity * if v.enabled { 1.0 } else { DISABLED };
    if v.ring {
        draw::focus_ring(&look.painter, v.rect, R_CONTROL, p.accent);
    }
    let shown = snap_rect(draw::scaled(v.rect, v.scale), look.ppp);
    let (fill, ink) = if muted {
        (if v.down { p.danger_pressed } else { p.danger }, p.on_danger)
    } else {
        (if v.down { p.button_pressed } else { p.button }, p.text)
    };
    look.painter.rect_filled(shown, radius(R_CONTROL * v.scale), fade(fill, alpha));
    let label = if muted { "Muted" } else { "Mute" };
    let galley = draw::layout(&look.painter, label, BUTTON, fade(ink, alpha), f32::INFINITY);
    let glyph = 16.0 * v.scale;
    let width = glyph + theme::S * v.scale + galley.size().x;
    let left = shown.center().x - width / 2.0;
    draw::mic_glyph(&look.painter, Rect::from_min_size(Pos2::new(left, shown.center().y - glyph / 2.0), Vec2::splat(glyph)), fade(ink, alpha), muted);
    look.painter.galley(Pos2::new(left + glyph + theme::S * v.scale, shown.center().y - galley.size().y / 2.0), galley, fade(ink, alpha));
}

/// A text button ("Try again", the dialog buttons): 28 px high, radius 6.
pub fn draw_text_button(look: &Look, v: &ButtonView) {
    let p = look.p;
    let alpha = v.opacity * if v.enabled { 1.0 } else { DISABLED };
    if v.ring {
        draw::focus_ring(&look.painter, v.rect, R_CONTROL, fade(p.accent, v.opacity));
    }
    let shown = snap_rect(draw::scaled(v.rect, v.scale), look.ppp);
    let (fill, ink) = match v.kind {
        ButtonKind::Secondary => (if v.down { p.button_pressed } else { p.button }, if v.loading { p.text_secondary } else { p.text }),
        ButtonKind::Default => (if v.down { p.accent_pressed } else { p.accent }, p.on_accent),
    };
    look.painter.rect_filled(shown, radius(R_CONTROL * v.scale), fade(fill, alpha));
    draw::text(&look.painter, shown.center(), Align2::CENTER_CENTER, v.label, BUTTON, fade(ink, alpha));
}

/// The state light: a solid dot, or a ring while something is not (yet) fine. `solid` (0 ring, 1 dot) and the
/// colour are animated by the caller, which makes the crossfade.
pub fn draw_led(look: &Look, centre: Pos2, color: Color32, solid: f32) {
    let d = theme::LED;
    let c = Pos2::new(snap(centre.x, look.ppp), snap(centre.y, look.ppp));
    look.painter.circle_filled(c, d / 2.0, fade(color, solid));
    if solid < 1.0 {
        look.painter.circle_stroke(c, d / 2.0 - 0.75, Stroke::new(1.5, fade(color, 1.0 - solid)));
    }
}

// --- the Mic/PC mix ---------------------------------------------------------------------------------------------------

pub struct MixView {
    /// The 46 px block: label row, 8 px, slider row. It is as wide as the group minus its padding.
    pub block: Rect,
    /// PC share in steps, `None` without a device.
    pub steps: Option<i32>,
    pub ring: bool,
    pub enabled: bool,
}

/// Track rectangle of the mix slider and the x range the thumb centre travels over (10 px in from each end).
pub fn mix_track(block: Rect) -> (Rect, f32, f32) {
    let slider_row = Rect::from_min_size(Pos2::new(block.left(), block.top() + 26.0), Vec2::new(block.width(), 20.0));
    let track = Rect::from_min_max(
        Pos2::new(slider_row.left() + 30.0, slider_row.center().y - TRACK / 2.0),
        Pos2::new(slider_row.right() - 30.0, slider_row.center().y + TRACK / 2.0),
    );
    (track, track.left() + THUMB / 2.0, track.right() - THUMB / 2.0)
}

pub fn draw_mix(look: &Look, v: &MixView) {
    let p = look.p;
    let alpha = if v.enabled { 1.0 } else { DISABLED };
    let block = v.block;
    draw::text(&look.painter, Pos2::new(block.left(), block.top()), Align2::LEFT_TOP, "Mic/PC mix", BODY, fade(p.text, alpha));
    let value = v.steps.map_or_else(|| "\u{2013}".to_owned(), model::format_mix);
    draw::text(&look.painter, Pos2::new(block.right(), block.top() + 9.0), Align2::RIGHT_CENTER, &value, HINT, fade(p.text_secondary, alpha));

    let (track, lo, hi) = mix_track(block);
    let row_mid = track.center().y;
    draw::text(&look.painter, Pos2::new(block.left(), row_mid), Align2::LEFT_CENTER, "Mic", SCALE, fade(p.text_tertiary, alpha));
    draw::text(&look.painter, Pos2::new(block.right(), row_mid), Align2::RIGHT_CENTER, "PC", SCALE, fade(p.text_tertiary, alpha));
    look.painter.rect_filled(snap_rect(track, look.ppp), radius(TRACK / 2.0), fade(p.track, alpha));
    if let Some(steps) = v.steps {
        let x = lo + (hi - lo) * model::MIX.fraction(steps);
        draw_thumb(look, Pos2::new(x, row_mid), v.ring);
    }
}

/// Width of a fader frame plus a meter frame and the 8 px between, for centring the pair.
pub const MIC_PAIR_W: f32 = FADER_FRAME + theme::S + METER_FRAME;
