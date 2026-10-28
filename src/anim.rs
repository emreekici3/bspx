//! Window animation engine.
//!
//! Two curves, chosen with `animation_easing`:
//!
//! * `ease_out`: a frame-rate independent exponential ease-out, where each
//!   frame covers `1 - e^(-speed * dt)` of the remaining distance. Snappy,
//!   but it starts at full speed.
//! * `ease_in_out` is a cubic ease-in-out over a fixed duration: the window
//!   gathers speed, then settles. It reads as smoother because no frame
//!   jumps. A window retargeted mid-flight is already moving, so it finishes
//!   with a cubic ease-out from where it is instead of stopping to restart.
//!
//! `dt` is capped at a 30 fps tick so a frame after a long idle sleep does
//! not jump; position and size move together; retargeting continues from
//! the in-flight frame. The in-flight frame is kept in `f32` and only
//! rounded on output, and the exponential curve snaps within 1 point, so
//! its last frame does not visibly jump.

use std::collections::HashMap;

use crate::bsp::WinId;
use crate::geometry::Rect;

/// Largest `dt` a single frame may consume.
pub const MAX_STEP_SECS: f64 = 1.0 / 30.0;
/// Distance in points under which an exponential animation snaps to its target.
pub const SNAP_THRESHOLD: f32 = 1.0;
/// Duration of a speed-`s` animation: when the exponential curve has `e^-K`
/// (0.7 %) of the distance left. Keeps `animation_duration` meaning the same
/// for both curves.
pub const DURATION_K: f64 = 5.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Easing {
    /// Exponential ease-out.
    #[default]
    EaseOut,
    /// Cubic ease-in-out over the animation duration.
    EaseInOut,
}

impl Easing {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "ease_out" | "exponential" => Some(Self::EaseOut),
            "ease_in_out" | "smooth" => Some(Self::EaseInOut),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::EaseOut => "ease_out",
            Self::EaseInOut => "ease_in_out",
        }
    }
}

/// Fraction of the remaining distance an exponential ease-out consumes in a
/// frame, given a decay `rate` (per second) and the frame's `delta` in seconds.
pub fn ease_out_factor(rate: f64, delta: f64) -> f32 {
    let delta = delta.clamp(0.0, MAX_STEP_SECS);
    (1.0 - (-rate * delta).exp()).clamp(0.0, 1.0) as f32
}

fn cubic_in_out(p: f64) -> f64 {
    if p < 0.5 {
        4.0 * p * p * p
    } else {
        1.0 - (-2.0 * p + 2.0).powi(3) / 2.0
    }
}

fn cubic_out(p: f64) -> f64 {
    1.0 - (1.0 - p).powi(3)
}

#[derive(Clone, Copy, Debug)]
struct Track {
    /// Current, sub-point frame: x, y, w, h.
    now: [f32; 4],
    /// Where the timed curve started, and how far along it is.
    from: [f32; 4],
    elapsed: f64,
    /// The window was already moving when this segment began.
    retargeted: bool,
    target: Rect,
    last_sent: Rect,
}

fn parts(r: Rect) -> [f32; 4] {
    [r.x as f32, r.y as f32, r.w as f32, r.h as f32]
}

impl Track {
    fn new(from: Rect, target: Rect) -> Self {
        Self {
            now: parts(from),
            from: parts(from),
            elapsed: 0.0,
            retargeted: false,
            target,
            last_sent: from,
        }
    }

    fn retarget(&mut self, target: Rect) {
        if target != self.target {
            self.from = self.now;
            self.elapsed = 0.0;
            self.retargeted = true;
            self.target = target;
        }
    }

    fn rounded(&self) -> Rect {
        let [x, y, w, h] = self.now.map(|v| v.round() as i32);
        Rect::new(x, y, w, h)
    }

    /// Exponential step by factor `t`. Returns the frame and whether it is final.
    fn advance_exp(&mut self, t: f32) -> (Rect, bool) {
        let target = parts(self.target);
        for (v, goal) in self.now.iter_mut().zip(target) {
            *v += (goal - *v) * t;
        }
        let left = |i: usize, j: usize| (target[i] - self.now[i]).hypot(target[j] - self.now[j]);
        if left(0, 1) <= SNAP_THRESHOLD && left(2, 3) <= SNAP_THRESHOLD {
            return (self.target, true);
        }
        (self.rounded(), false)
    }

    /// Timed step on the cubic curve lasting `duration` seconds.
    fn advance_timed(&mut self, dt: f64, duration: f64) -> (Rect, bool) {
        self.elapsed += dt.clamp(0.0, MAX_STEP_SECS);
        let p = (self.elapsed / duration).min(1.0);
        // Tolerate float drift: 15 frames of 1/60 s sum to just under 0.25.
        if p >= 1.0 - 1e-9 {
            self.now = parts(self.target);
            return (self.target, true);
        }
        let e = if self.retargeted {
            cubic_out(p)
        } else {
            cubic_in_out(p)
        } as f32;
        let target = parts(self.target);
        for ((now, from), goal) in self.now.iter_mut().zip(self.from).zip(target) {
            *now = from + (goal - from) * e;
        }
        (self.rounded(), false)
    }
}

/// Drives every window toward its target frame.
#[derive(Debug)]
pub struct Animator {
    /// Decay rate per second; higher is snappier. `<= 0` or very large
    /// values make moves instant.
    speed: f64,
    easing: Easing,
    tracks: HashMap<WinId, Track>,
}

impl Animator {
    pub fn new(speed: f64) -> Self {
        Self {
            speed,
            easing: Easing::default(),
            tracks: HashMap::new(),
        }
    }

    pub fn set_speed(&mut self, speed: f64) {
        self.speed = speed;
    }

    pub fn set_easing(&mut self, easing: Easing) {
        self.easing = easing;
    }

    fn instant(&self) -> bool {
        self.speed <= 0.0 || self.speed >= 10_000.0
    }

    /// Sends window `id` toward `target`. `current` is where the window is
    /// now; it is ignored while the window is already animating, so the
    /// motion continues smoothly from its in-flight frame.
    ///
    /// Returns a frame to apply immediately when animations are disabled.
    pub fn animate_to(&mut self, id: WinId, current: Rect, target: Rect) -> Option<Rect> {
        if self.instant() {
            self.tracks.remove(&id);
            return (current != target).then_some(target);
        }
        if let Some(track) = self.tracks.get_mut(&id) {
            track.retarget(target);
            return None;
        }
        if current != target {
            self.tracks.insert(id, Track::new(current, target));
        }
        None
    }

    /// Stops animating `id` where it is.
    pub fn cancel(&mut self, id: WinId) {
        self.tracks.remove(&id);
    }

    pub fn is_idle(&self) -> bool {
        self.tracks.is_empty()
    }

    pub fn is_animating(&self, id: WinId) -> bool {
        self.tracks.contains_key(&id)
    }

    /// The frame `id` will end up in, if it is animating.
    pub fn target(&self, id: WinId) -> Option<Rect> {
        self.tracks.get(&id).map(|t| t.target)
    }

    /// Advances all animations by `dt` seconds and returns the frames that
    /// changed and must be pushed to the windows.
    pub fn step(&mut self, dt: f64) -> Vec<(WinId, Rect)> {
        let t = ease_out_factor(self.speed, dt);
        let duration = DURATION_K / self.speed.max(f64::EPSILON);
        let easing = self.easing;
        let mut out = Vec::with_capacity(self.tracks.len());
        self.tracks.retain(|&id, track| {
            let (frame, done) = match easing {
                Easing::EaseOut => track.advance_exp(t),
                Easing::EaseInOut => track.advance_timed(dt, duration),
            };
            if frame != track.last_sent {
                track.last_sent = frame;
                out.push((id, frame));
            }
            !done
        });
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: f64 = 1.0 / 60.0;

    fn run_to_end(a: &mut Animator, max_frames: usize) -> (Vec<(WinId, Rect)>, usize) {
        let mut all = Vec::new();
        for i in 0..max_frames {
            all.extend(a.step(FRAME));
            if a.is_idle() {
                return (all, i + 1);
            }
        }
        panic!("animation did not finish in {max_frames} frames");
    }

    #[test]
    fn ease_factor_is_capped_and_bounded() {
        assert_eq!(ease_out_factor(12.0, 0.0), 0.0);
        // A 2 s sleep counts as a single 30 fps tick.
        assert_eq!(
            ease_out_factor(12.0, 2.0),
            ease_out_factor(12.0, MAX_STEP_SECS)
        );
        assert!(ease_out_factor(1e9, MAX_STEP_SECS) <= 1.0);
    }

    #[test]
    fn reaches_target_exactly() {
        let mut a = Animator::new(12.0);
        let target = Rect::new(500, 40, 800, 600);
        assert_eq!(a.animate_to(1, Rect::new(0, 0, 400, 300), target), None);
        let (frames, n) = run_to_end(&mut a, 200);
        assert_eq!(frames.last().unwrap(), &(1, target));
        assert!(n > 5, "should animate over several frames, took {n}");
    }

    #[test]
    fn motion_is_monotonic_ease_out() {
        let mut a = Animator::new(12.0);
        a.animate_to(1, Rect::new(0, 0, 100, 100), Rect::new(1000, 0, 100, 100));
        let (frames, _) = run_to_end(&mut a, 200);
        let xs: Vec<i32> = frames.iter().map(|(_, r)| r.x).collect();
        assert!(xs.windows(2).all(|w| w[1] >= w[0]));
        // Decelerating: early steps are bigger than late ones.
        assert!(xs[1] - xs[0] > xs[xs.len() - 1] - xs[xs.len() - 2]);
    }

    #[test]
    fn retarget_continues_from_in_flight_frame() {
        let mut a = Animator::new(12.0);
        a.animate_to(1, Rect::new(0, 0, 100, 100), Rect::new(1000, 0, 100, 100));
        let mid = *a.step(FRAME).last().unwrap();
        // The stale `current` passed here is ignored.
        a.animate_to(1, Rect::new(0, 0, 100, 100), Rect::new(0, 0, 100, 100));
        let next = a.step(FRAME)[0];
        assert!(next.1.x < mid.1.x && next.1.x > 0);
        assert_eq!(a.target(1), Some(Rect::new(0, 0, 100, 100)));
    }

    #[test]
    fn no_op_when_already_there() {
        let mut a = Animator::new(12.0);
        let r = Rect::new(1, 2, 3, 4);
        a.animate_to(1, r, r);
        assert!(a.is_idle());
    }

    #[test]
    fn instant_mode_returns_target() {
        let mut a = Animator::new(0.0);
        let target = Rect::new(10, 10, 10, 10);
        assert_eq!(a.animate_to(1, Rect::default(), target), Some(target));
        assert!(a.is_idle());
    }

    #[test]
    fn ease_in_out_takes_the_duration_and_is_symmetric() {
        let mut a = Animator::new(DURATION_K / 0.25); // 0.25 s
        a.set_easing(Easing::EaseInOut);
        a.animate_to(1, Rect::new(0, 0, 100, 100), Rect::new(1000, 0, 100, 100));
        let (frames, n) = run_to_end(&mut a, 100);
        assert_eq!(n, 15, "0.25 s at 60 fps");
        assert_eq!(frames.last().unwrap().1, Rect::new(1000, 0, 100, 100));
        let xs: Vec<i32> = frames.iter().map(|(_, r)| r.x).collect();
        assert!(xs.windows(2).all(|w| w[1] >= w[0]), "{xs:?}");
        // Slow start, slow end: the first and last steps are the smallest.
        let steps: Vec<i32> = std::iter::once(xs[0])
            .chain(xs.windows(2).map(|w| w[1] - w[0]))
            .collect();
        let biggest = *steps.iter().max().unwrap();
        assert!(
            steps[0] < biggest / 4 && *steps.last().unwrap() < biggest / 4,
            "{steps:?}"
        );
    }

    #[test]
    fn ease_in_out_retarget_keeps_moving() {
        let mut a = Animator::new(DURATION_K / 0.25);
        a.set_easing(Easing::EaseInOut);
        a.animate_to(1, Rect::new(0, 0, 100, 100), Rect::new(1000, 0, 100, 100));
        for _ in 0..7 {
            a.step(FRAME);
        }
        let before = a.step(FRAME)[0].1.x;
        a.animate_to(1, Rect::default(), Rect::new(2000, 0, 100, 100));
        let after = a.step(FRAME)[0].1.x;
        // No stop-and-restart: the next step is at least as big as the last.
        assert!(after - before > 40, "{before} -> {after}");
        let (frames, _) = run_to_end(&mut a, 100);
        assert_eq!(frames.last().unwrap().1.x, 2000);
    }

    #[test]
    fn exponential_ends_without_a_visible_jump() {
        let mut a = Animator::new(12.0);
        a.animate_to(1, Rect::new(0, 0, 100, 100), Rect::new(1000, 0, 100, 100));
        let (frames, _) = run_to_end(&mut a, 500);
        let n = frames.len();
        assert!(frames[n - 1].1.x - frames[n - 2].1.x <= 2);
    }

    #[test]
    fn easing_names() {
        assert_eq!(Easing::parse("ease_in_out"), Some(Easing::EaseInOut));
        assert_eq!(Easing::parse("smooth"), Some(Easing::EaseInOut));
        assert_eq!(Easing::parse("ease_out"), Some(Easing::EaseOut));
        assert_eq!(Easing::parse("bounce"), None);
        assert_eq!(Easing::EaseInOut.name(), "ease_in_out");
    }

    #[test]
    fn faster_speed_finishes_sooner() {
        let from = Rect::new(0, 0, 100, 100);
        let to = Rect::new(800, 400, 900, 700);
        let mut slow = Animator::new(8.0);
        let mut fast = Animator::new(25.0);
        slow.animate_to(1, from, to);
        fast.animate_to(1, from, to);
        assert!(run_to_end(&mut fast, 500).1 < run_to_end(&mut slow, 500).1);
    }
}
