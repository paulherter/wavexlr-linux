//! Control state and input handling: faders, switches and pressable buttons. Painting lives in `view.rs`.

use std::collections::HashMap;

use eframe::egui::{Context, CursorIcon, Event, EventFilter, Id, Key, Modifiers, MouseWheelUnit, Rect, Response, Sense, Ui};

use super::anim::{Tween, PRESS_IN_S, PRESS_OUT_S, PRESS_SCALE};
use super::model::{Drag, Range, Throttle, SETTLE_S, WRITE_INTERVAL_S};

/// Which way a slider runs. A vertical slider grows upwards, a horizontal one to the right.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    Vertical,
    Horizontal,
}

/// Everything the user did to a control this frame.
#[derive(Default, Clone, Copy)]
pub struct Touch {
    /// A new value to send to the daemon (already merged by the throttle).
    pub send: Option<f64>,
    /// The pointer is holding the control.
    pub held: bool,
    /// The control has keyboard focus.
    pub focused: bool,
}

/// A slider (the two faders and the Mic/PC mix). A drag is relative to where it was grabbed, never animates, and
/// device updates never move it while the hand is on it.
pub struct SliderCtl {
    pub range: Range,
    pub steps: i32,
    drag: Option<Drag>,
    throttle: Throttle,
    ignore_until: f64,
    wheel: f32,
}

impl SliderCtl {
    pub fn new(range: Range) -> Self {
        Self { range, steps: 0, drag: None, throttle: Throttle::new(WRITE_INTERVAL_S), ignore_until: 0.0, wheel: 0.0 }
    }

    pub fn value(&self) -> f64 {
        self.range.value(self.steps)
    }

    pub fn is_busy(&self) -> bool {
        self.drag.is_some() || self.throttle.has_pending()
    }

    /// Take a value from the device, unless the hand is on the slider or just let go of it.
    pub fn adopt_steps(&mut self, steps: i32, now: f64) {
        if !self.is_busy() && now >= self.ignore_until {
            self.steps = steps;
        }
    }

    pub fn cancel(&mut self) {
        self.drag = None;
        self.throttle = Throttle::new(WRITE_INTERVAL_S);
    }

    fn changed(&mut self, steps: i32, now: f64) -> Option<f64> {
        if steps == self.steps {
            return None;
        }
        self.steps = steps;
        self.throttle.set(self.value(), now)
    }

    /// Handle pointer, wheel and keys. `lo` is the screen coordinate of the minimum (y for a vertical slider, x for
    /// a horizontal one), `hi` the one of the maximum. The returned `send` is in slider units (steps converted).
    #[allow(clippy::too_many_arguments)]
    pub fn interact(&mut self, ui: &Ui, id: Id, rect: Rect, axis: Axis, lo: f32, hi: f32, now: f64, active: bool) -> Touch {
        let sense = if active { Sense::click_and_drag() } else { Sense::hover() };
        let response = ui.interact(rect, id, sense);
        let ctx = ui.ctx();
        if !active {
            self.cancel();
            return Touch::default();
        }
        if response.hovered() || self.drag.is_some() {
            // The thumb is what you hold: an open hand over it, a closed one while it is held.
            ctx.set_cursor_icon(if self.drag.is_some() { CursorIcon::Grabbing } else { CursorIcon::Grab });
        }
        let mut touch = Touch { focused: response.has_focus(), ..Touch::default() };
        let max = self.range.max_steps();

        // Pointer: relative to the grab point, so pressing alone never moves the value.
        if response.is_pointer_button_down_on() {
            if let Some(pos) = ctx.input(|i| i.pointer.latest_pos()) {
                match &mut self.drag {
                    None => {
                        let along = if axis == Axis::Vertical { pos.y } else { -pos.x };
                        self.drag = Some(Drag::start(self.steps, along));
                        response.request_focus();
                    }
                    Some(drag) => {
                        let fine = ctx.input(|i| i.modifiers.shift);
                        // Up and to the right both mean more; the drag maths counts "up", so x is flipped.
                        let along = if axis == Axis::Vertical { pos.y } else { -pos.x };
                        let steps = drag.update(along, (hi - lo).abs(), max, fine);
                        touch.send = self.changed(steps, now);
                    }
                }
            }
        } else if self.drag.take().is_some() {
            touch.send = self.throttle.flush(now).or(touch.send);
            self.ignore_until = now + SETTLE_S;
        }
        touch.held = self.drag.is_some();

        // Wheel over the fader: 1 dB per notch.
        if response.hovered() && self.drag.is_none() {
            let delta = ctx.input(|i| {
                i.events
                    .iter()
                    .map(|e| match e {
                        Event::MouseWheel { unit: MouseWheelUnit::Line, delta, .. } => delta.y * 2.0,
                        Event::MouseWheel { delta, .. } => delta.y / 20.0,
                        _ => 0.0,
                    })
                    .sum::<f32>()
            });
            self.wheel += delta;
            let whole = self.wheel.trunc();
            if whole != 0.0 {
                self.wheel -= whole;
                let steps = self.range.step_by(self.steps, whole as i32);
                touch.send = self.changed(steps, now).or(touch.send);
                self.ignore_until = now + SETTLE_S;
            }
        }

        // Keys: arrows 0.5 dB, Page Up/Down 5 dB. Home and End do nothing on purpose.
        if response.has_focus() {
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    id,
                    EventFilter { horizontal_arrows: true, vertical_arrows: true, ..EventFilter::default() },
                )
            });
            let delta = key_steps(ctx, axis);
            if delta != 0 {
                let steps = self.range.step_by(self.steps, delta);
                touch.send = self.changed(steps, now).or(touch.send);
                self.ignore_until = now + SETTLE_S;
            }
        }

        // A value held back by the throttle goes out as soon as the interval is over.
        if touch.send.is_none() {
            touch.send = self.throttle.poll(now);
        }
        if let Some(wait) = self.throttle.wait(now) {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(wait.max(0.001)));
        }
        touch
    }
}

/// Net key movement this frame in steps, and the keys are used up so nothing else reacts to them. A vertical slider
/// answers Up and Down, a horizontal one Left and Right, both Page Up and Page Down (10 steps).
fn key_steps(ctx: &Context, axis: Axis) -> i32 {
    let mut total = 0;
    ctx.input_mut(|i| {
        for event in &i.events {
            if let Event::Key { key, pressed: true, modifiers, .. } = event {
                if modifiers.any() {
                    continue;
                }
                total += match (key, axis) {
                    (Key::ArrowUp, Axis::Vertical) | (Key::ArrowRight, Axis::Horizontal) => 1,
                    (Key::ArrowDown, Axis::Vertical) | (Key::ArrowLeft, Axis::Horizontal) => -1,
                    (Key::PageUp, _) => 10,
                    (Key::PageDown, _) => -10,
                    _ => 0,
                };
            }
        }
        for key in [Key::ArrowUp, Key::ArrowRight, Key::ArrowDown, Key::ArrowLeft, Key::PageUp, Key::PageDown, Key::Home, Key::End] {
            i.consume_key(Modifiers::NONE, key);
        }
    });
    total
}

/// A boolean the user flips and the device confirms. After a click, older device news is ignored for a moment.
#[derive(Default)]
pub struct Toggle {
    pub on: bool,
    ignore_until: f64,
}

impl Toggle {
    pub fn adopt(&mut self, on: bool, now: f64) {
        if now >= self.ignore_until {
            self.on = on;
        }
    }

    pub fn set(&mut self, on: bool, now: f64) {
        self.on = on;
        self.ignore_until = now + SETTLE_S;
    }
}

/// What a pressable control reports for one frame.
#[derive(Clone, Copy)]
pub struct Press {
    pub clicked: bool,
    /// The click came from Space or Enter, not the pointer.
    pub by_key: bool,
    pub scale: f32,
    pub focused: bool,
    /// The pointer is holding it down right now.
    pub down: bool,
}

/// Press feedback: scale 0.97 while the pointer holds the control (100 ms in, 160 ms out, strong ease-out).
/// Never for the keyboard, and not at all with reduced motion.
#[derive(Default)]
pub struct Presses {
    tweens: HashMap<Id, Tween>,
}

impl Presses {
    pub fn handle(&mut self, ui: &Ui, id: Id, hit: Rect, active: bool, now: f64, reduced: bool) -> Press {
        let sense = if active { Sense::click() } else { Sense::hover() };
        let response: Response = ui.interact(hit, id, sense);
        let ctx = ui.ctx();
        let tween = self.tweens.entry(id).or_insert_with(|| Tween::new(1.0));
        if !active {
            tween.jump(1.0);
            return Press { clicked: false, by_key: false, scale: 1.0, focused: false, down: false };
        }
        if response.hovered() {
            ctx.set_cursor_icon(CursorIcon::PointingHand);
        }
        let down = response.is_pointer_button_down_on() && response.contains_pointer();
        // Reduced motion: no scale at all (the colour change of the pressed state stays).
        let (target, duration) = if down && !reduced { (PRESS_SCALE, PRESS_IN_S) } else { (1.0, PRESS_OUT_S) };
        tween.go(target, now, if reduced { 0.0 } else { duration });
        let scale = tween.value(now);
        if tween.active(now) {
            ctx.request_repaint();
        }
        let clicked = response.clicked();
        let by_key = clicked && ctx.input(|i| i.key_pressed(Key::Space) || i.key_pressed(Key::Enter));
        Press { clicked, by_key, scale, focused: response.has_focus(), down }
    }

    pub fn any_active(&self, now: f64) -> bool {
        self.tweens.values().any(|t| t.active(now))
    }

    /// The current press scale of a control (1.0 when it was never pressed).
    #[cfg(test)]
    pub fn scale_of(&self, id: Id, now: f64) -> f32 {
        self.tweens.get(&id).map_or(1.0, |t| t.value(now))
    }
}
