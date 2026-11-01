//! Window animation engine.
//!
//! Three curves, chosen with `animation_easing`:
//!
//! * `spring` (the default): a critically damped spring. It keeps each
//!   window's velocity, so a window sent somewhere new mid-flight bends
//!   toward it without a stop or a jolt, and it eases in and out without
//!   overshooting. Each frame is the spring's exact solution over `dt`,
//!   so the motion does not depend on the frame rate.
//! * `ease_out`: a frame-rate independent exponential ease-out, where each
//!   frame covers `1 - e^(-speed * dt)` of the remaining distance. Snappy,
//!   but it starts at full speed.
//! * `ease_in_out` is a cubic ease-in-out over a fixed duration: the window
//!   gathers speed, then settles. It reads as smoother because no frame
//!   jumps. A window retargeted mid-flight is already moving, so it finishes
//!   with a cubic ease-out from where it is instead of stopping to restart.
//!
//! `dt` is capped at a 10 fps tick, so a frame after a stall does not
//! jump, while one a little late still keeps the animation on time; position and size move together; retargeting continues from
//! the in-flight frame. The in-flight frame is kept in `f32` and only
//! rounded on output, and the exponential curve snaps within 1 point, so
//! its last frame does not visibly jump.

use std::collections::HashMap;

use crate::bsp::WinId;
use crate::geometry::Rect;

/// Largest `dt` a single frame may consume. A late frame (an app slow to
/// answer) still covers the time that went by, so animations keep their
/// duration; only a real stall is cut short.
pub const MAX_STEP_SECS: f64 = 0.1;
/// Distance in points under which an exponential animation snaps to its target.
pub const SNAP_THRESHOLD: f32 = 1.0;
/// Duration of a speed-`s` animation: when the exponential curve has `e^-K`
/// (0.7 %) of the distance left. Keeps `animation_duration` meaning the same
/// for both curves.
pub const DURATION_K: f64 = 5.0;
/// The spring's angular frequency times the duration: a critically damped
/// spring starting at rest has `(1 + u) e^-u` of the distance left after
/// `u = ω t`, which is the same 0.7 % as the exponential curve at 7.1.
pub const SPRING_K: f64 = 7.1;
/// A spring at rest closer than this (points) to its target snaps to it.
const SPRING_SNAP: f32 = 0.5;
/// ...and slower than this (points per second).
const SPRING_REST_SPEED: f32 = 20.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Easing {
    /// Critically damped spring.
    #[default]
    Spring,
    /// Exponential ease-out.
    EaseOut,
    /// Cubic ease-in-out over the animation duration.
    EaseInOut,
}

impl Easing {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "ease_out" | "exponential" => Some(Self::EaseOut),
            "ease_in_out" | "smooth" => Some(Self::EaseInOut),
            "spring" => Some(Self::Spring),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::EaseOut => "ease_out",
            Self::EaseInOut => "ease_in_out",
            Self::Spring => "spring",
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
    /// The spring's velocity, points per second.
    velocity: [f32; 4],
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
            velocity: [0.0; 4],
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

    /// Spring step over `dt` seconds with angular frequency `omega`: the
    /// exact critically damped solution, carried over from the velocity the
    /// window has. Returns the frame and whether it is final.
    fn advance_spring(&mut self, dt: f64, omega: f64) -> (Rect, bool) {
        let t = dt.clamp(0.0, MAX_STEP_SECS);
        let decay = (-omega * t).exp();
        let target = parts(self.target);
        let mut rest = true;
        for ((now, velocity), goal) in self.now.iter_mut().zip(&mut self.velocity).zip(target) {
            let x0 = f64::from(*now - goal);
            let v0 = f64::from(*velocity);
            let c = v0 + omega * x0;
            let x = (x0 + c * t) * decay;
            let v = (v0 - omega * c * t) * decay;
            *now = goal + x as f32;
            *velocity = v as f32;
            rest &= x.abs() < f64::from(SPRING_SNAP) && v.abs() < f64::from(SPRING_REST_SPEED);
        }
        if rest {
            self.now = target;
            self.velocity = [0.0; 4];
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
        let omega = SPRING_K / duration;
        let easing = self.easing;
        let mut out = Vec::with_capacity(self.tracks.len());
        self.tracks.retain(|&id, track| {
            let (frame, done) = match easing {
                Easing::EaseOut => track.advance_exp(t),
                Easing::EaseInOut => track.advance_timed(dt, duration),
                Easing::Spring => track.advance_spring(dt, omega),
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

    /// An animator on the exponential curve, which the older tests describe.
    fn exp(speed: f64) -> Animator {
        let mut a = Animator::new(speed);
        a.set_easing(Easing::EaseOut);
        a
    }

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
        let mut a = exp(12.0);
        let target = Rect::new(500, 40, 800, 600);
        assert_eq!(a.animate_to(1, Rect::new(0, 0, 400, 300), target), None);
        let (frames, n) = run_to_end(&mut a, 200);
        assert_eq!(frames.last().unwrap(), &(1, target));
        assert!(n > 5, "should animate over several frames, took {n}");
    }

    #[test]
    fn motion_is_monotonic_ease_out() {
        let mut a = exp(12.0);
        a.animate_to(1, Rect::new(0, 0, 100, 100), Rect::new(1000, 0, 100, 100));
        let (frames, _) = run_to_end(&mut a, 200);
        let xs: Vec<i32> = frames.iter().map(|(_, r)| r.x).collect();
        assert!(xs.windows(2).all(|w| w[1] >= w[0]));
        // Decelerating: early steps are bigger than late ones.
        assert!(xs[1] - xs[0] > xs[xs.len() - 1] - xs[xs.len() - 2]);
    }

    #[test]
    fn retarget_continues_from_in_flight_frame() {
        let mut a = exp(12.0);
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
        let mut a = exp(12.0);
        let r = Rect::new(1, 2, 3, 4);
        a.animate_to(1, r, r);
        assert!(a.is_idle());
    }

    #[test]
    fn instant_mode_returns_target() {
        let mut a = exp(0.0);
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
        let mut a = exp(12.0);
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
        assert_eq!(Easing::parse("spring"), Some(Easing::Spring));
        assert_eq!(Easing::parse("bounce"), None);
        assert_eq!(Easing::EaseInOut.name(), "ease_in_out");
    }

    #[test]
    fn faster_speed_finishes_sooner() {
        let from = Rect::new(0, 0, 100, 100);
        let to = Rect::new(800, 400, 900, 700);
        let mut slow = exp(8.0);
        let mut fast = exp(25.0);
        slow.animate_to(1, from, to);
        fast.animate_to(1, from, to);
        assert!(run_to_end(&mut fast, 500).1 < run_to_end(&mut slow, 500).1);
    }
    fn spring(duration: f64) -> Animator {
        let mut a = Animator::new(DURATION_K / duration);
        a.set_easing(Easing::Spring);
        a
    }

    #[test]
    fn spring_settles_on_target_in_about_its_duration() {
        let mut a = spring(0.3);
        let target = Rect::new(1000, 40, 700, 500);
        a.animate_to(1, Rect::new(0, 40, 300, 500), target);
        let (frames, n) = run_to_end(&mut a, 200);
        assert_eq!(frames.last().unwrap().1, target);
        // 0.3 s is 18 frames at 60 fps; the last half point takes a little longer.
        assert!((14..=26).contains(&n), "{n} frames");
    }

    #[test]
    fn spring_eases_in_and_out_without_overshoot() {
        let mut a = spring(0.3);
        a.animate_to(1, Rect::new(0, 0, 100, 100), Rect::new(1000, 0, 100, 100));
        let (frames, _) = run_to_end(&mut a, 200);
        let xs: Vec<i32> = frames.iter().map(|(_, r)| r.x).collect();
        assert!(xs.iter().all(|x| *x <= 1000), "overshoots: {xs:?}");
        assert!(xs.windows(2).all(|w| w[1] >= w[0]), "{xs:?}");
        let steps: Vec<i32> = std::iter::once(xs[0])
            .chain(xs.windows(2).map(|w| w[1] - w[0]))
            .collect();
        let biggest = *steps.iter().max().unwrap();
        // Gathers speed instead of leaping off at full speed.
        assert!(steps[0] < biggest * 2 / 3, "{steps:?}");
        assert!(*steps.last().unwrap() <= 2, "{steps:?}");
    }

    #[test]
    fn spring_keeps_its_velocity_when_retargeted() {
        let mut a = spring(0.3);
        a.animate_to(1, Rect::new(0, 0, 100, 100), Rect::new(1000, 0, 100, 100));
        let mut last = 0;
        let mut step = 0;
        for _ in 0..6 {
            let x = a.step(FRAME)[0].1.x;
            step = x - last;
            last = x;
        }
        // Sent further while flying: it carries on at the speed it had.
        a.animate_to(1, Rect::default(), Rect::new(2000, 0, 100, 100));
        let next = a.step(FRAME)[0].1.x - last;
        assert!(next >= step * 9 / 10, "{step} -> {next}");
        // Sent back: it slows down before it turns, never jumps backwards.
        let mut b = spring(0.3);
        b.animate_to(1, Rect::new(0, 0, 100, 100), Rect::new(1000, 0, 100, 100));
        let mut prev = 0;
        for _ in 0..6 {
            prev = b.step(FRAME)[0].1.x;
        }
        b.animate_to(1, Rect::default(), Rect::new(0, 0, 100, 100));
        let after = b.step(FRAME)[0].1.x;
        assert!(after >= prev, "turned at once: {prev} -> {after}");
        let (frames, _) = run_to_end(&mut b, 200);
        assert_eq!(frames.last().unwrap().1.x, 0);
    }

    #[test]
    fn spring_does_not_depend_on_the_frame_rate() {
        let run = |fps: f64| {
            let mut a = spring(0.3);
            a.animate_to(1, Rect::new(0, 0, 100, 100), Rect::new(1000, 0, 100, 100));
            let frames = (0.15 * fps).round() as usize;
            let mut x = 0;
            for _ in 0..frames {
                if let Some(f) = a.step(1.0 / fps).last() {
                    x = f.1.x;
                }
            }
            x
        };
        let (at60, at120) = (run(60.0), run(120.0));
        assert!((at60 - at120).abs() <= 1, "{at60} vs {at120}");
    }
}
