//! Custom painting helpers: fonts, text, pixel snapping, focus ring, shadows and the little glyphs. No state lives here.

use std::sync::Arc;

use eframe::egui::epaint::{CubicBezierShape, Shadow};
use eframe::egui::text::{LayoutJob, TextFormat};
use eframe::egui::{
    Align2, Color32, Context, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Galley, Painter, Pos2, Rect,
    Shape, Stroke, StrokeKind, Vec2,
};

use super::theme::{RING, RING_GAP};

/// The fonts are Noto Sans (SIL OFL 1.1), subset to Latin and cut at three weights; see fonts/FONTS.md.
const NOTO_400: &[u8] = include_bytes!("fonts/NotoSans-400.ttf");
const NOTO_500: &[u8] = include_bytes!("fonts/NotoSans-500.ttf");
const NOTO_600: &[u8] = include_bytes!("fonts/NotoSans-600.ttf");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Weight {
    Regular,
    Medium,
    Semi,
}

pub fn family(weight: Weight) -> FontFamily {
    FontFamily::Name(
        match weight {
            Weight::Regular => "w400",
            Weight::Medium => "w500",
            Weight::Semi => "w600",
        }
        .into(),
    )
}

pub fn install_fonts(ctx: &Context) {
    let mut defs = FontDefinitions::empty();
    for (name, bytes) in [("w400", NOTO_400), ("w500", NOTO_500), ("w600", NOTO_600)] {
        defs.font_data.insert(name.to_owned(), Arc::new(FontData::from_static(bytes)));
        defs.families.insert(FontFamily::Name(name.into()), vec![name.to_owned()]);
    }
    defs.families.insert(FontFamily::Proportional, vec!["w400".to_owned()]);
    defs.families.insert(FontFamily::Monospace, vec!["w400".to_owned()]);
    ctx.set_fonts(defs);
}

/// One text role of the design: size, weight, line box and tracking (in em).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Type {
    pub size: f32,
    pub weight: Weight,
    pub line: f32,
    pub tracking: f32,
}

const fn ty(size: f32, weight: Weight, line: f32, tracking: f32) -> Type {
    Type { size, weight, line, tracking }
}

/// Gain and volume: 32 / 600, line 40, tracking -0.02 em (-0.64 px).
pub const READOUT: Type = ty(32.0, Weight::Semi, 40.0, -0.02);
/// Group titles and the dialog title.
pub const TITLE: Type = ty(13.0, Weight::Semi, 18.0, 0.0);
/// Row labels and the mix label.
pub const BODY: Type = ty(13.0, Weight::Regular, 18.0, 0.0);
/// Button labels and the status title.
pub const BUTTON: Type = ty(13.0, Weight::Medium, 18.0, 0.0);
/// The unit "dB".
pub const UNIT: Type = ty(13.0, Weight::Regular, 18.0, 0.0);
/// Hints, the mix value, Peak, the device line, the dialog body.
pub const HINT: Type = ty(12.0, Weight::Regular, 16.0, 0.0);
/// Scale numbers and "Mic" / "PC".
pub const SCALE: Type = ty(11.0, Weight::Medium, 14.0, 0.0);

fn job(text: &str, t: Type, color: Color32, wrap_width: f32) -> LayoutJob {
    let mut job = LayoutJob::single_section(
        text.to_owned(),
        TextFormat {
            font_id: FontId::new(t.size, family(t.weight)),
            color,
            extra_letter_spacing: t.tracking * t.size,
            line_height: Some(t.line),
            ..Default::default()
        },
    );
    job.wrap.max_width = wrap_width;
    job
}

/// Lay text out in a role. Lines are exactly `t.line` high.
pub fn layout(painter: &Painter, text: &str, t: Type, color: Color32, wrap_width: f32) -> Arc<Galley> {
    painter.layout_job(job(text, t, color, wrap_width))
}

/// One line that ends in an ellipsis when it does not fit `width`.
pub fn layout_one_line(painter: &Painter, text: &str, t: Type, color: Color32, width: f32) -> Arc<Galley> {
    let mut j = job(text, t, color, width);
    j.wrap.max_rows = 1;
    j.wrap.break_anywhere = true;
    j.wrap.overflow_character = Some('\u{2026}');
    painter.layout_job(j)
}

/// Draw one line of text anchored at `pos`. Returns the rectangle it covers.
pub fn text(painter: &Painter, pos: Pos2, anchor: Align2, text: &str, t: Type, color: Color32) -> Rect {
    let galley = layout(painter, text, t, color, f32::INFINITY);
    let rect = anchor.anchor_size(pos, galley.size());
    painter.galley(rect.min, galley, color);
    rect
}

pub fn text_width(painter: &Painter, text: &str, t: Type) -> f32 {
    layout(painter, text, t, Color32::WHITE, f32::INFINITY).size().x
}

/// How many lines `text` takes in `width`.
pub fn line_count(painter: &Painter, text: &str, t: Type, width: f32) -> usize {
    layout(painter, text, t, Color32::WHITE, width).rows.len().max(1)
}

/// Round to a whole physical pixel, so 1 px lines and edges stay crisp at any scale.
pub fn snap(value: f32, ppp: f32) -> f32 {
    (value * ppp).round() / ppp
}

pub fn snap_rect(rect: Rect, ppp: f32) -> Rect {
    Rect::from_min_max(
        Pos2::new(snap(rect.min.x, ppp), snap(rect.min.y, ppp)),
        Pos2::new(snap(rect.max.x, ppp), snap(rect.max.y, ppp)),
    )
}

pub fn fade(color: Color32, alpha: f32) -> Color32 {
    color.gamma_multiply(alpha)
}

pub fn radius(value: f32) -> CornerRadius {
    CornerRadius::from(value)
}

/// Scale a rectangle around its centre (the pressed look of buttons and switches).
pub fn scaled(rect: Rect, scale: f32) -> Rect {
    Rect::from_center_size(rect.center(), rect.size() * scale)
}

/// 2 px ring, 2 px outside the control, so its outer radius is the control's plus 4. Shown only after keyboard focus.
pub fn focus_ring(painter: &Painter, rect: Rect, control_radius: f32, color: Color32) {
    painter.rect_stroke(
        rect.expand(RING_GAP + RING / 2.0),
        radius(control_radius + RING_GAP + RING / 2.0),
        Stroke::new(RING, color),
        StrokeKind::Middle,
    );
}

/// A soft drop shadow under a rounded rectangle (offset down by `dy`).
pub fn drop_shadow(painter: &Painter, rect: Rect, corner: f32, dy: i8, blur: u8, color: Color32) {
    painter.add(Shadow { offset: [0, dy], blur, spread: 0, color }.as_shape(rect, radius(corner)));
}

/// Microphone glyph in a 16 px box with a 1.5 px stroke; `slashed` for the muted look.
pub fn mic_glyph(painter: &Painter, area: Rect, color: Color32, slashed: bool) {
    let stroke = Stroke::new(1.5, color);
    let s = area.width() / 16.0;
    let p = |x: f32, y: f32| Pos2::new(area.left() + x * s, area.top() + y * s);
    painter.rect_stroke(
        Rect::from_min_size(p(5.5, 1.0), Vec2::new(5.0, 8.5) * s),
        radius(2.5 * s),
        stroke,
        StrokeKind::Middle,
    );
    painter.add(Shape::CubicBezier(CubicBezierShape::from_points_stroke(
        [p(3.0, 7.0), p(3.0, 14.0), p(13.0, 14.0), p(13.0, 7.0)],
        false,
        Color32::TRANSPARENT,
        stroke,
    )));
    painter.line_segment([p(8.0, 12.3), p(8.0, 15.0)], stroke);
    if slashed {
        painter.line_segment([p(2.0, 1.5), p(14.0, 14.5)], stroke);
    }
}

/// Warning triangle for the 48 V dialog.
pub fn warning_glyph(painter: &Painter, area: Rect, fill: Color32, ink: Color32) {
    let c = area.center();
    let h = area.height();
    let top = Pos2::new(c.x, area.top() + h * 0.08);
    let left = Pos2::new(area.left() + h * 0.02, area.bottom() - h * 0.1);
    let right = Pos2::new(area.right() - h * 0.02, area.bottom() - h * 0.1);
    painter.add(Shape::convex_polygon(vec![top, right, left], fill, Stroke::new(2.0, fill)));
    painter.line_segment(
        [Pos2::new(c.x, area.top() + h * 0.38), Pos2::new(c.x, area.top() + h * 0.64)],
        Stroke::new(2.6, ink),
    );
    painter.circle_filled(Pos2::new(c.x, area.top() + h * 0.78), 1.6, ink);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapping_goes_to_whole_physical_pixels() {
        assert_eq!(snap(10.2, 1.0), 10.0);
        assert_eq!(snap(10.2, 1.5), 10.0);
        assert_eq!(snap(10.3, 2.0), 10.5);
    }

    #[test]
    fn the_focus_ring_sits_two_px_outside_and_is_two_px_thick() {
        // The outer edge of the ring is control + 4 px, its radius control radius + 4.
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(34.0, 20.0));
        let outer = rect.expand(RING_GAP + RING);
        assert_eq!(outer.size(), Vec2::new(42.0, 28.0));
        assert_eq!(RING_GAP + RING, 4.0);
    }

    #[test]
    fn the_type_scale_has_four_sizes() {
        let sizes: std::collections::BTreeSet<u32> = [READOUT, TITLE, BODY, BUTTON, UNIT, HINT, SCALE].iter().map(|t| t.size as u32).collect();
        assert_eq!(sizes.into_iter().collect::<Vec<_>>(), vec![11, 12, 13, 32]);
        assert!((READOUT.tracking * READOUT.size + 0.64).abs() < 1e-4);
    }
}
