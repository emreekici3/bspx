//! Window motion.
//!
//! A window is four edges, not a position and a size. Each edge follows its
//! goal through three critically damped poles: `(x - goal)` decays as
//! `(c0 + c1 t + c2 t^2) e^(-w t)`. Three things come of that:
//!
//! * Position, velocity and acceleration are all continuous, so a window
//!   neither jerks off the mark nor when it is sent somewhere new in flight:
//!   it bends toward the new goal, carrying what it had.
//! * Each step is the exact solution over `dt`, so the path does not depend
//!   on the frame rate.
//! * From rest it never overshoots.
//!
//! Edges are also what makes the rounding to whole points clean. Rounding
//! `x` and `w` apart lets a still edge wobble by a point; here a still edge
//! is the exact number it started as, and a window only moving keeps its
//! exact size, so it is never asked to lay itself out again.

use std::collections::HashMap;

use crate::bsp::WinId;
use crate::geometry::Rect;

/// Largest step a single frame may take, in seconds. A late frame still
/// covers the time that went by; only a real stall is cut short.
const MAX_STEP: f64 = 0.1;
/// `w * duration`, so that `duration` is when 0.7 % of the distance is left:
/// `(1 + u + u^2 / 2) e^-u = 0.007` at this `u`.
const DURATION_POLES: f64 = 8.8;
/// An edge this close to its goal (points) and this slow (points a second)
/// has arrived.
const REST_DISTANCE: f64 = 0.25;
const REST_SPEED: f64 = 8.0;
/// How far a frame's time may be from a whole number of refreshes and still
/// count as exactly that many, as a share of a refresh.
const PACE_TOLERANCE: f64 = 0.2;

/// The time a frame stands for. The clock read at each frame wobbles by
/// a fraction of a millisecond; the display shows whole refreshes, so steps
/// taken in whole refreshes come out even.
pub fn paced(dt: f64, refresh: f64) -> f64 {
    if refresh <= 0.0 {
        return dt;
    }
    let frames = (dt / refresh).round().max(1.0);
    let whole = frames * refresh;
    if (dt - whole).abs() <= PACE_TOLERANCE * refresh {
        whole
    } else {
        dt
    }
}

#[derive(Clone, Copy, Debug)]
struct Edge {
    pos: f64,
    vel: f64,
    acc: f64,
    goal: f64,
}

impl Edge {
    fn at(pos: i32, goal: i32) -> Self {
        Self {
            pos: f64::from(pos),
            vel: 0.0,
            acc: 0.0,
            goal: f64::from(goal),
        }
    }

    fn step(&mut self, t: f64, w: f64) {
        let c0 = self.pos - self.goal;
        let c1 = self.vel + w * c0;
        let c2 = (self.acc + 2.0 * w * c1 - w * w * c0) / 2.0;
        let decay = (-w * t).exp();
        let x = c0 + t * (c1 + t * c2);
        let dx = c1 + 2.0 * c2 * t;
        self.pos = self.goal + decay * x;
        self.vel = decay * (dx - w * x);
        self.acc = decay * (2.0 * c2 - 2.0 * w * dx + w * w * x);
    }

    fn arrived(&self) -> bool {
        (self.pos - self.goal).abs() < REST_DISTANCE && self.vel.abs() < REST_SPEED
    }

    /// Exactly where it started, and going nowhere.
    fn still(&self) -> bool {
        self.pos == self.goal && self.vel == 0.0 && self.acc == 0.0
    }

    fn set_goal(&mut self, goal: i32) {
        self.goal = f64::from(goal);
    }

    fn stop(&mut self) {
        *self = Self {
            pos: self.goal,
            vel: 0.0,
            acc: 0.0,
            goal: self.goal,
        };
    }
}

/// Start and length of the span between two edges, in whole points. Length
/// is rounded on its own, so a span that only moves keeps it exactly; then
/// the edge standing still decides where the span goes.
fn span(low: &Edge, high: &Edge) -> (i32, i32) {
    let len = (high.pos - low.pos).round();
    let start = if high.still() && !low.still() {
        high.pos - len
    } else {
        low.pos.round()
    };
    (start as i32, len as i32)
}

#[derive(Clone, Copy, Debug)]
struct Motion {
    left: Edge,
    top: Edge,
    right: Edge,
    bottom: Edge,
    target: Rect,
    shown: Rect,
}

impl Motion {
    fn new(from: Rect, to: Rect) -> Self {
        Self {
            left: Edge::at(from.x, to.x),
            top: Edge::at(from.y, to.y),
            right: Edge::at(from.max_x(), to.max_x()),
            bottom: Edge::at(from.max_y(), to.max_y()),
            target: to,
            shown: from,
        }
    }

    fn edges(&mut self) -> [&mut Edge; 4] {
        [
            &mut self.left,
            &mut self.top,
            &mut self.right,
            &mut self.bottom,
        ]
    }

    fn aim(&mut self, to: Rect) {
        self.target = to;
        self.left.set_goal(to.x);
        self.top.set_goal(to.y);
        self.right.set_goal(to.max_x());
        self.bottom.set_goal(to.max_y());
    }

    /// Advances `t` seconds. Returns whether it is done.
    fn advance(&mut self, t: f64, w: f64) -> bool {
        for edge in self.edges() {
            if !edge.still() {
                edge.step(t, w);
            }
        }
        let done = self.edges().iter().all(|e| e.arrived());
        if done {
            for edge in self.edges() {
                edge.stop();
            }
        }
        done
    }

    fn frame(&self) -> Rect {
        let (x, w) = span(&self.left, &self.right);
        let (y, h) = span(&self.top, &self.bottom);
        Rect::new(x, y, w, h)
    }
}

/// Drives every window toward where it was sent.
#[derive(Debug)]
pub struct Mover {
    /// Seconds a motion takes; `0` makes moves instant.
    duration: f64,
    motions: HashMap<WinId, Motion>,
}

impl Mover {
    pub fn new(duration: f64) -> Self {
        Self {
            duration,
            motions: HashMap::new(),
        }
    }

    pub fn set_duration(&mut self, duration: f64) {
        self.duration = duration;
    }

    /// Sends window `id` to `target`. `current` is where it is now, unless
    /// it is in motion: then it carries on from where the motion has it.
    ///
    /// Returns the frame to apply at once when motion is off.
    pub fn send(&mut self, id: WinId, current: Rect, target: Rect) -> Option<Rect> {
        if self.duration <= 0.0 {
            self.motions.remove(&id);
            return (current != target).then_some(target);
        }
        if let Some(motion) = self.motions.get_mut(&id) {
            motion.aim(target);
        } else if current != target {
            self.motions.insert(id, Motion::new(current, target));
        }
        None
    }

    /// Stops window `id` where it is.
    pub fn halt(&mut self, id: WinId) {
        self.motions.remove(&id);
    }

    pub fn at_rest(&self) -> bool {
        self.motions.is_empty()
    }

    pub fn in_motion(&self, id: WinId) -> bool {
        self.motions.contains_key(&id)
    }

    /// Where `id` is going, if it is in motion.
    pub fn heading(&self, id: WinId) -> Option<Rect> {
        self.motions.get(&id).map(|m| m.target)
    }

    /// Advances everything by `dt` seconds. Returns the frames that changed.
    pub fn advance(&mut self, dt: f64) -> Vec<(WinId, Rect)> {
        let t = dt.clamp(0.0, MAX_STEP);
        let w = DURATION_POLES / self.duration.max(f64::EPSILON);
        let mut changed = Vec::with_capacity(self.motions.len());
        self.motions.retain(|&id, motion| {
            let done = motion.advance(t, w);
            let frame = if done { motion.target } else { motion.frame() };
            // The last frame goes out even if rounding got there sooner: it
            // is the one a window is told it stays at.
            if frame != motion.shown || done {
                motion.shown = frame;
                changed.push((id, frame));
            }
            !done
        });
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: f64 = 1.0 / 60.0;
    const FROM: Rect = Rect::new(0, 0, 100, 100);
    const FAR: Rect = Rect::new(1000, 0, 100, 100);

    fn run(a: &mut Mover, dt: f64) -> Vec<Rect> {
        let mut frames = Vec::new();
        for _ in 0..100_000 {
            frames.extend(a.advance(dt).into_iter().map(|(_, f)| f));
            if a.at_rest() {
                return frames;
            }
        }
        panic!("it never arrived");
    }

    fn sent(from: Rect, to: Rect) -> Vec<Rect> {
        let mut a = Mover::new(0.3);
        a.send(1, from, to);
        run(&mut a, FRAME)
    }

    #[test]
    fn it_lands_exactly() {
        let to = Rect::new(500, 40, 800, 600);
        assert_eq!(*sent(FROM, to).last().unwrap(), to);
    }

    #[test]
    fn it_takes_about_its_duration() {
        let n = sent(FROM, FAR).len();
        assert!((14..=26).contains(&n), "{n} frames for 18 at 60 fps");
    }

    #[test]
    fn the_constant_means_what_it_says() {
        let u: f64 = DURATION_POLES;
        let left = (1.0 + u + u * u / 2.0) * (-u).exp();
        assert!((left - 0.007).abs() < 0.0005, "{left}");
    }

    #[test]
    fn from_rest_it_never_overshoots_or_turns_back() {
        let xs: Vec<i32> = sent(FROM, FAR).iter().map(|f| f.x).collect();
        assert!(xs.windows(2).all(|p| p[1] >= p[0]), "{xs:?}");
        assert!(xs.iter().all(|x| *x <= 1000), "{xs:?}");
    }

    #[test]
    fn it_gathers_speed_and_settles_gently() {
        let xs: Vec<i32> = sent(FROM, FAR).iter().map(|f| f.x).collect();
        let steps: Vec<i32> = xs.windows(2).map(|p| p[1] - p[0]).collect();
        let peak = *steps.iter().max().unwrap();
        assert!(xs[0] < peak / 2, "starts softly: {xs:?}");
        assert!(steps[0] < steps[1], "{steps:?}");
        assert!(*steps.last().unwrap() <= 2, "{steps:?}");
    }

    #[test]
    fn speed_never_changes_abruptly() {
        let xs: Vec<i32> = sent(FROM, FAR).iter().map(|f| f.x).collect();
        let steps: Vec<i32> = xs.windows(2).map(|p| p[1] - p[0]).collect();
        let peak = *steps.iter().max().unwrap();
        for pair in steps.windows(2) {
            assert!((pair[1] - pair[0]).abs() <= peak / 2, "{steps:?}");
        }
    }

    #[test]
    fn a_new_goal_in_flight_bends_the_path() {
        let mut a = Mover::new(0.3);
        a.send(1, FROM, FAR);
        let mut last = 0;
        let mut step = 0;
        for _ in 0..7 {
            let x = a.advance(FRAME)[0].1.x;
            step = x - last;
            last = x;
        }
        // Further: it keeps going at the speed it had.
        a.send(1, FROM, Rect::new(2000, 0, 100, 100));
        let next = a.advance(FRAME)[0].1.x - last;
        assert!(
            next >= step * 9 / 10 && next <= step * 13 / 10,
            "{step} {next}"
        );
        // Back: it slows down before it turns.
        a.send(1, FROM, FROM);
        let at = a.motions[&1].left.pos;
        let after = a.advance(FRAME)[0].1.x;
        assert!(f64::from(after) >= at, "turned at once");
        assert_eq!(*run(&mut a, FRAME).last().unwrap(), FROM);
    }

    #[test]
    fn the_path_does_not_depend_on_the_frame_rate() {
        let at = |fps: f64| {
            let mut a = Mover::new(0.3);
            a.send(1, FROM, FAR);
            let mut x = 0;
            for _ in 0..(0.15 * fps).round() as usize {
                if let Some(f) = a.advance(1.0 / fps).last() {
                    x = f.1.x;
                }
            }
            x
        };
        assert!((at(60.0) - at(120.0)).abs() <= 1);
        assert!((at(60.0) - at(240.0)).abs() <= 1);
    }

    #[test]
    fn a_still_edge_never_moves_a_single_point() {
        // Shrinking from the left, growing from the top: right and bottom.
        let from = Rect::new(600, 200, 401, 301);
        let to = Rect::new(733, 77, 268, 424);
        for frame in sent(from, to) {
            assert_eq!(frame.max_x(), from.max_x(), "{frame:?}");
            assert_eq!(frame.max_y(), from.max_y(), "{frame:?}");
        }
        // Growing to the right: the left edge.
        let to = Rect::new(600, 200, 777, 301);
        for frame in sent(from, to) {
            assert_eq!(frame.x, from.x, "{frame:?}");
            assert_eq!(frame.y, from.y, "{frame:?}");
            assert_eq!(frame.h, from.h, "{frame:?}");
        }
    }

    #[test]
    fn a_window_only_moving_never_changes_size() {
        let from = Rect::new(13, 57, 411, 333);
        let to = Rect::new(977, 402, 411, 333);
        for fps in [59.0, 60.0, 120.0, 144.0] {
            let mut a = Mover::new(0.37);
            a.send(1, from, to);
            for frame in run(&mut a, 1.0 / fps) {
                assert_eq!((frame.w, frame.h), (411, 333), "{frame:?}");
            }
        }
    }

    #[test]
    fn a_stall_does_not_make_it_jump() {
        let mut a = Mover::new(0.3);
        a.send(1, FROM, FAR);
        let late = a.advance(5.0)[0].1.x;
        assert!(late < 900, "{late}");
        assert!(!a.at_rest());
    }

    #[test]
    fn nothing_is_sent_twice() {
        let frames = sent(FROM, Rect::new(3, 0, 100, 100));
        let (_, moving) = frames.split_last().unwrap();
        assert!(moving.windows(2).all(|p| p[0] != p[1]), "{frames:?}");
    }

    #[test]
    fn the_last_frame_always_goes_out() {
        let mut a = Mover::new(0.3);
        a.send(1, FROM, FAR);
        let mut last = None;
        while !a.at_rest() {
            last = a.advance(FRAME).pop().or(last);
            if a.at_rest() {
                assert_eq!(last, Some((1, FAR)));
            }
        }
    }

    #[test]
    fn already_there_is_no_motion() {
        let mut a = Mover::new(0.3);
        assert_eq!(a.send(1, FROM, FROM), None);
        assert!(a.at_rest());
    }

    #[test]
    fn duration_zero_goes_at_once() {
        let mut a = Mover::new(0.0);
        assert_eq!(a.send(1, FROM, FAR), Some(FAR));
        assert!(a.at_rest());
        assert_eq!(a.send(1, FAR, FAR), None);
    }

    #[test]
    fn a_shorter_duration_finishes_sooner() {
        let count = |d: f64| {
            let mut a = Mover::new(d);
            a.send(1, FROM, FAR);
            run(&mut a, FRAME).len()
        };
        assert!(count(0.2) < count(0.6));
    }

    #[test]
    fn cancel_leaves_it_where_it_is() {
        let mut a = Mover::new(0.3);
        a.send(1, FROM, FAR);
        a.advance(FRAME);
        a.halt(1);
        assert!(!a.in_motion(1));
        assert_eq!(a.heading(1), None);
        assert!(a.advance(FRAME).is_empty());
    }

    #[test]
    fn windows_move_independently() {
        let mut a = Mover::new(0.3);
        a.send(1, FROM, FAR);
        a.send(2, FAR, FROM);
        assert_eq!(a.advance(FRAME).len(), 2);
        a.halt(1);
        assert!(a.in_motion(2));
    }

    #[test]
    fn frames_snap_to_whole_refreshes() {
        let refresh = 1.0 / 120.0;
        assert_eq!(paced(refresh * 1.1, refresh), refresh);
        assert_eq!(paced(refresh * 0.9, refresh), refresh);
        assert_eq!(paced(refresh * 2.05, refresh), refresh * 2.0);
        // Halfway between two refreshes: the clock knows best.
        assert_eq!(paced(refresh * 2.5, refresh), refresh * 2.5);
        assert_eq!(paced(0.0, refresh), 0.0);
        assert_eq!(paced(0.01, 0.0), 0.01);
    }
}
