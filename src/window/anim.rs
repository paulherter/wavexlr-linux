//! Small interruptible tweens with the strong ease-out curve of the design (cubic-bezier 0.23, 1, 0.32, 1).
//! Time is passed in, so the maths is testable. A tween that is retargeted mid-flight starts from where it is.

const X1: f64 = 0.23;
const Y1: f64 = 1.0;
const X2: f64 = 0.32;
const Y2: f64 = 1.0;

/// Pointer press feedback: 0.97, 100 ms in, 160 ms out.
pub const PRESS_SCALE: f32 = 0.97;
pub const PRESS_IN_S: f64 = 0.100;
pub const PRESS_OUT_S: f64 = 0.160;
/// The switch knob slides this long after a click (a key press jumps).
pub const SWITCH_S: f64 = 0.150;
/// The status light crossfades between its looks this long.
pub const LIGHT_S: f64 = 0.200;
/// The 48 V dialog: the scrim and the card come in 200 ms, go out in 150 ms. The card starts at 0.96, never at 0.
pub const DIALOG_IN_S: f64 = 0.200;
pub const DIALOG_OUT_S: f64 = 0.150;
pub const DIALOG_FROM_SCALE: f32 = 0.96;
/// With reduced motion every change is a plain fade this long (or a jump).
pub const REDUCED_FADE_S: f64 = 0.100;

fn bezier(a: f64, b: f64, t: f64) -> f64 {
    let u = 1.0 - t;
    3.0 * u * u * t * a + 3.0 * u * t * t * b + t * t * t
}

/// CSS-style cubic-bezier easing: progress (0..1) in, eased progress out.
pub fn ease_out(x: f64) -> f64 {
    let x = x.clamp(0.0, 1.0);
    // Solve bezier_x(t) = x by bisection, then read bezier_y(t).
    let (mut lo, mut hi) = (0.0, 1.0);
    for _ in 0..32 {
        let mid = (lo + hi) / 2.0;
        if bezier(X1, X2, mid) < x {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    bezier(Y1, Y2, (lo + hi) / 2.0)
}

#[derive(Clone, Copy, Debug)]
pub struct Tween {
    from: f32,
    to: f32,
    start: f64,
    duration: f64,
}

impl Tween {
    pub fn new(value: f32) -> Self {
        Self { from: value, to: value, start: 0.0, duration: 0.0 }
    }

    pub fn value(&self, now: f64) -> f32 {
        if self.duration <= 0.0 {
            return self.to;
        }
        let t = (now - self.start) / self.duration;
        if t >= 1.0 {
            return self.to;
        }
        self.from + (self.to - self.from) * ease_out(t) as f32
    }

    pub fn active(&self, now: f64) -> bool {
        self.duration > 0.0 && now - self.start < self.duration
    }

    /// Head for `to`. With `duration` 0 the value jumps (reduced motion, keyboard).
    pub fn go(&mut self, to: f32, now: f64, duration: f64) {
        if to == self.to {
            if duration <= 0.0 {
                self.jump(to);
            }
            return;
        }
        self.from = self.value(now);
        self.to = to;
        self.start = now;
        self.duration = duration;
    }

    pub fn jump(&mut self, to: f32) {
        *self = Self::new(to);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ease_out_starts_fast_and_ends_at_one() {
        assert!(ease_out(0.0).abs() < 1e-6);
        assert!((ease_out(1.0) - 1.0).abs() < 1e-6);
        // Strong ease-out: well over half the way after a fifth of the time.
        assert!(ease_out(0.2) > 0.6, "{}", ease_out(0.2));
        // Never goes backwards.
        let mut last = 0.0;
        for i in 0..=100 {
            let v = ease_out(f64::from(i) / 100.0);
            assert!(v >= last - 1e-9);
            last = v;
        }
    }

    #[test]
    fn tween_runs_and_can_be_interrupted() {
        let mut t = Tween::new(1.0);
        t.go(0.97, 0.0, 0.1);
        assert!(t.active(0.05));
        assert!(t.value(0.05) < 1.0 && t.value(0.05) > 0.97);
        assert_eq!(t.value(0.2), 0.97);
        assert!(!t.active(0.2));
        // Let go half way: head back from where it is, not from 0.97.
        let mut t = Tween::new(1.0);
        t.go(0.97, 0.0, 0.1);
        let mid = t.value(0.03);
        t.go(1.0, 0.03, 0.16);
        assert!((t.value(0.03) - mid).abs() < 1e-6);
        assert_eq!(t.value(1.0), 1.0);
    }

    #[test]
    fn zero_duration_jumps() {
        let mut t = Tween::new(0.0);
        t.go(1.0, 5.0, 0.0);
        assert_eq!(t.value(5.0), 1.0);
        assert!(!t.active(5.0));
    }
}
