//! Binary space partitioning layout.
//!
//! Every leaf holds one window; every inner node splits its area in two,
//! either side by side ([`Split::Columns`]) or stacked ([`Split::Rows`]).
//! A new window always splits an existing leaf along that leaf's longer side,
//! so the screen is carved up into a binary tree of panes.

use std::collections::HashMap;

use crate::geometry::{Direction, Rect};

pub type WinId = u32;

/// The smallest width and height each window accepts, where known.
pub type MinSizes = HashMap<WinId, (i32, i32)>;

const MIN_RATIO: f32 = 0.05;
const MAX_RATIO: f32 = 0.95;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Split {
    /// `a` on the left, `b` on the right.
    Columns,
    /// `a` on top, `b` below.
    Rows,
}

impl Split {
    fn toggled(self) -> Self {
        match self {
            Split::Columns => Split::Rows,
            Split::Rows => Split::Columns,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Side {
    A,
    B,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    Leaf(WinId),
    Split {
        split: Split,
        /// Share of the area given to `a`, in `MIN_RATIO..=MAX_RATIO`.
        ratio: f32,
        a: Box<Node>,
        b: Box<Node>,
    },
}

impl Node {
    fn leaf_count(&self) -> usize {
        match self {
            Node::Leaf(_) => 1,
            Node::Split { a, b, .. } => a.leaf_count() + b.leaf_count(),
        }
    }

    /// The smallest area this subtree fits in: its windows' minimum sizes
    /// side by side or stacked, with the gaps between them.
    fn min_size(&self, mins: &MinSizes, gap: i32) -> (i32, i32) {
        match self {
            Node::Leaf(w) => mins.get(w).copied().unwrap_or((0, 0)),
            Node::Split { split, a, b, .. } => {
                let (aw, ah) = a.min_size(mins, gap);
                let (bw, bh) = b.min_size(mins, gap);
                match split {
                    Split::Columns => (aw + gap + bw, ah.max(bh)),
                    Split::Rows => (aw.max(bw), ah + gap + bh),
                }
            }
        }
    }

    /// Splits `area` between `a` and `b`, giving each at least its minimum
    /// size when the area allows. Returns the two areas and the share `a`
    /// really got, which differs from `ratio` when a minimum size won.
    fn split_area(&self, area: Rect, gap: i32, mins: &MinSizes) -> Option<(Rect, Rect, f32)> {
        let Node::Split { split, ratio, a, b } = self else {
            return None;
        };
        let (amin, bmin) = (a.min_size(mins, gap), b.min_size(mins, gap));
        let (amin, bmin, len) = match split {
            Split::Columns => (amin.0, bmin.0, area.w),
            Split::Rows => (amin.1, bmin.1, area.h),
        };
        let avail = (len - gap).max(2);
        let mut share = ((avail as f32) * ratio).round() as i32;
        if amin + bmin <= avail {
            share = share.clamp(amin.max(1), (avail - bmin).min(avail - 1));
        } else if amin + bmin > 0 {
            // Both cannot fit: share the shortfall in proportion.
            share = ((avail as f32) * amin as f32 / (amin + bmin) as f32).round() as i32;
        }
        let share = share.clamp(1, avail - 1);
        let (ra, rb) = split_rect(area, *split, share as f32 / avail as f32, gap);
        Some((ra, rb, share as f32 / avail as f32))
    }

    fn collect(&self, out: &mut Vec<WinId>) {
        match self {
            Node::Leaf(w) => out.push(*w),
            Node::Split { a, b, .. } => {
                a.collect(out);
                b.collect(out);
            }
        }
    }

    fn path_to(&self, win: WinId, path: &mut Vec<Side>) -> bool {
        match self {
            Node::Leaf(w) => *w == win,
            Node::Split { a, b, .. } => {
                path.push(Side::A);
                if a.path_to(win, path) {
                    return true;
                }
                *path.last_mut().unwrap() = Side::B;
                if b.path_to(win, path) {
                    return true;
                }
                path.pop();
                false
            }
        }
    }
}

/// Splits `area` into the two child areas of a split node, leaving `gap`
/// points between them.
pub fn split_rect(area: Rect, split: Split, ratio: f32, gap: i32) -> (Rect, Rect) {
    match split {
        Split::Columns => {
            let avail = (area.w - gap).max(2);
            let aw = ((avail as f32) * ratio).round() as i32;
            let aw = aw.clamp(1, avail - 1);
            (
                Rect::new(area.x, area.y, aw, area.h),
                Rect::new(area.x + aw + gap, area.y, avail - aw, area.h),
            )
        }
        Split::Rows => {
            let avail = (area.h - gap).max(2);
            let ah = ((avail as f32) * ratio).round() as i32;
            let ah = ah.clamp(1, avail - 1);
            (
                Rect::new(area.x, area.y, area.w, ah),
                Rect::new(area.x, area.y + ah + gap, area.w, avail - ah),
            )
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct BspTree {
    root: Option<Node>,
}

impl BspTree {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn root(&self) -> Option<&Node> {
        self.root.as_ref()
    }

    pub fn is_empty(&self) -> bool {
        self.root.is_none()
    }

    pub fn len(&self) -> usize {
        self.root.as_ref().map_or(0, Node::leaf_count)
    }

    pub fn contains(&self, win: WinId) -> bool {
        self.path(win).is_some()
    }

    /// Windows in tree order (left/top first).
    pub fn windows(&self) -> Vec<WinId> {
        let mut out = Vec::new();
        if let Some(root) = &self.root {
            root.collect(&mut out);
        }
        out
    }

    /// Computes the frame of every window inside `area`.
    pub fn layout(&self, area: Rect, gap: i32) -> Vec<(WinId, Rect)> {
        self.layout_with(area, gap, &MinSizes::new())
    }

    /// Like [`layout`](Self::layout), but no window is made smaller than
    /// its entry in `mins` while the area has room for it.
    pub fn layout_with(&self, area: Rect, gap: i32, mins: &MinSizes) -> Vec<(WinId, Rect)> {
        fn rec(node: &Node, area: Rect, gap: i32, mins: &MinSizes, out: &mut Vec<(WinId, Rect)>) {
            match node {
                Node::Leaf(w) => out.push((*w, area)),
                Node::Split { a, b, .. } => {
                    let (ra, rb, _) = node.split_area(area, gap, mins).expect("a split");
                    rec(a, ra, gap, mins, out);
                    rec(b, rb, gap, mins, out);
                }
            }
        }
        let mut out = Vec::with_capacity(self.len());
        if let Some(root) = &self.root {
            rec(root, area, gap, mins, &mut out);
        }
        out
    }

    /// Sets every split's ratio to the share it really gets with `mins`,
    /// so a ratio never hides past what a minimum size allows (otherwise
    /// growing a window back would take several presses before it moved).
    pub fn settle_ratios(&mut self, area: Rect, gap: i32, mins: &MinSizes) {
        fn rec(node: &mut Node, area: Rect, gap: i32, mins: &MinSizes) {
            let Some((ra, rb, real)) = node.split_area(area, gap, mins) else {
                return;
            };
            if let Node::Split { ratio, a, b, .. } = node {
                *ratio = real.clamp(MIN_RATIO, MAX_RATIO);
                rec(a, ra, gap, mins);
                rec(b, rb, gap, mins);
            }
        }
        if let Some(root) = &mut self.root {
            rec(root, area, gap, mins);
        }
    }

    /// Inserts `new` by splitting the leaf of `target` (or the last window
    /// when `target` is `None` or unknown). The split follows the longer side
    /// of the target's current frame; the new window takes the right/bottom half.
    /// Returns `false` if the window is already managed.
    pub fn insert(
        &mut self,
        new: WinId,
        target: Option<WinId>,
        area: Rect,
        gap: i32,
        ratio: f32,
    ) -> bool {
        if self.contains(new) {
            return false;
        }
        let Some(root) = &self.root else {
            self.root = Some(Node::Leaf(new));
            return true;
        };
        let target = target
            .filter(|t| self.contains(*t))
            .or_else(|| {
                let mut all = Vec::new();
                root.collect(&mut all);
                all.last().copied()
            })
            .expect("non-empty tree has a window");

        let frame = self
            .layout(area, gap)
            .into_iter()
            .find_map(|(w, r)| (w == target).then_some(r))
            .unwrap_or(area);
        let path = self.path(target).expect("target is in the tree");
        // Split the other way from the window's parent, so splits take
        // turns (side by side, then stacked) and no row of three side by
        // side windows forms. A lone window splits along its longer side.
        let mut parent = self.root.as_ref();
        for side in path.iter().take(path.len().saturating_sub(1)) {
            parent = match parent {
                Some(Node::Split { a, b, .. }) => Some(if *side == Side::A { a } else { b }),
                _ => None,
            };
        }
        let split = match parent {
            Some(Node::Split {
                split: Split::Columns,
                ..
            }) if !path.is_empty() => Split::Rows,
            Some(Node::Split {
                split: Split::Rows, ..
            }) if !path.is_empty() => Split::Columns,
            _ if frame.w >= frame.h => Split::Columns,
            _ => Split::Rows,
        };

        let node = self.node_at_mut(&path);
        *node = Node::Split {
            split,
            ratio: ratio.clamp(MIN_RATIO, MAX_RATIO),
            a: Box::new(Node::Leaf(target)),
            b: Box::new(Node::Leaf(new)),
        };
        true
    }

    /// Removes a window; its sibling takes over the parent's whole area.
    pub fn remove(&mut self, win: WinId) -> bool {
        fn rec(node: Node, win: WinId) -> (Option<Node>, bool) {
            match node {
                Node::Leaf(w) if w == win => (None, true),
                leaf @ Node::Leaf(_) => (Some(leaf), false),
                Node::Split { split, ratio, a, b } => {
                    let (new_a, removed) = rec(*a, win);
                    if removed {
                        let node = match new_a {
                            Some(a) => Node::Split {
                                split,
                                ratio,
                                a: Box::new(a),
                                b,
                            },
                            None => *b,
                        };
                        return (Some(node), true);
                    }
                    let a = new_a.expect("untouched subtree survives");
                    let (new_b, removed) = rec(*b, win);
                    let node = match new_b {
                        Some(b) => Node::Split {
                            split,
                            ratio,
                            a: Box::new(a),
                            b: Box::new(b),
                        },
                        None => a,
                    };
                    (Some(node), removed)
                }
            }
        }
        let Some(root) = self.root.take() else {
            return false;
        };
        let (root, removed) = rec(root, win);
        self.root = root;
        removed
    }

    /// Exchanges the positions of two windows.
    pub fn swap(&mut self, x: WinId, y: WinId) -> bool {
        if x == y {
            return false;
        }
        let (Some(px), Some(py)) = (self.path(x), self.path(y)) else {
            return false;
        };
        *self.node_at_mut(&px) = Node::Leaf(y);
        *self.node_at_mut(&py) = Node::Leaf(x);
        true
    }

    /// Moves a border of `win` by `delta` (fraction of the parent's size)
    /// in direction `dir`. Prefers the border on the `dir` side of the window
    /// (growing it); if the window touches the screen edge there, moves the
    /// opposite border instead (shrinking it).
    pub fn resize(&mut self, win: WinId, dir: Direction, delta: f32) -> bool {
        let Some(path) = self.path(win) else {
            return false;
        };
        let wanted = match dir {
            Direction::Left | Direction::Right => Split::Columns,
            Direction::Up | Direction::Down => Split::Rows,
        };
        // The side the window must be on for the split's border to be the
        // window's border facing `dir`.
        let facing = match dir {
            Direction::Right | Direction::Down => Side::A,
            Direction::Left | Direction::Up => Side::B,
        };

        let mut facing_depth = None;
        let mut any_depth = None;
        let mut node = self.root.as_ref().unwrap();
        for (depth, side) in path.iter().enumerate() {
            let Node::Split { split, a, b, .. } = node else {
                break;
            };
            if *split == wanted {
                any_depth = Some(depth);
                if *side == facing {
                    facing_depth = Some(depth);
                }
            }
            node = if *side == Side::A { a } else { b };
        }
        let Some(depth) = facing_depth.or(any_depth) else {
            return false;
        };
        let signed = match dir {
            Direction::Right | Direction::Down => delta,
            Direction::Left | Direction::Up => -delta,
        };
        if let Node::Split { ratio, .. } = self.node_at_mut(&path[..depth]) {
            *ratio = (*ratio + signed).clamp(MIN_RATIO, MAX_RATIO);
            true
        } else {
            false
        }
    }

    /// Places the border on the `dir` side of `win` at an absolute screen
    /// coordinate, as when the user drags that edge with the mouse.
    /// `coord` is the far edge of the split's first half: the window's own
    /// right/bottom edge for `Right`/`Down`, or its left/top edge minus the
    /// gap for `Left`/`Up`. Returns `false` if that edge is a screen edge.
    pub fn set_border(
        &mut self,
        win: WinId,
        dir: Direction,
        coord: i32,
        area: Rect,
        gap: i32,
    ) -> bool {
        let Some(path) = self.path(win) else {
            return false;
        };
        let wanted = match dir {
            Direction::Left | Direction::Right => Split::Columns,
            Direction::Up | Direction::Down => Split::Rows,
        };
        let facing = match dir {
            Direction::Right | Direction::Down => Side::A,
            Direction::Left | Direction::Up => Side::B,
        };

        let mut found = None;
        let mut node = self.root.as_ref().unwrap();
        let mut rect = area;
        for (depth, side) in path.iter().enumerate() {
            let Node::Split { split, ratio, a, b } = node else {
                break;
            };
            if *split == wanted && *side == facing {
                found = Some((depth, rect));
            }
            let (ra, rb) = split_rect(rect, *split, *ratio, gap);
            (node, rect) = if *side == Side::A { (a, ra) } else { (b, rb) };
        }
        let Some((depth, r)) = found else {
            return false;
        };
        let new_ratio = match wanted {
            Split::Columns => (coord - r.x) as f32 / (r.w - gap).max(1) as f32,
            Split::Rows => (coord - r.y) as f32 / (r.h - gap).max(1) as f32,
        };
        if let Node::Split { ratio, .. } = self.node_at_mut(&path[..depth]) {
            *ratio = new_ratio.clamp(MIN_RATIO, MAX_RATIO);
            true
        } else {
            false
        }
    }

    /// The area of the split that directly contains `win`: what the window
    /// and its sibling share. `None` for a lone window or an unknown one.
    pub fn parent_rect(&self, win: WinId, area: Rect, gap: i32, mins: &MinSizes) -> Option<Rect> {
        let path = self.path(win)?;
        let (_, parent) = path.split_last()?;
        let mut node = self.root.as_ref()?;
        let mut rect = area;
        for side in parent {
            let (ra, rb, _) = node.split_area(rect, gap, mins)?;
            let Node::Split { a, b, .. } = node else {
                return None;
            };
            (node, rect) = if *side == Side::A { (a, ra) } else { (b, rb) };
        }
        Some(rect)
    }

    /// Switches the split containing `win` between columns and rows.
    pub fn toggle_split(&mut self, win: WinId) -> bool {
        self.with_parent(win, |split, _ratio, _a, _b| *split = split.toggled())
    }

    /// Swaps the two halves of the split containing `win`.
    pub fn flip(&mut self, win: WinId) -> bool {
        self.with_parent(win, |_split, ratio, a, b| {
            std::mem::swap(a, b);
            *ratio = 1.0 - *ratio;
        })
    }

    /// Rotates the whole layout by 90° clockwise.
    pub fn rotate(&mut self) {
        fn rec(node: &mut Node) {
            if let Node::Split { split, ratio, a, b } = node {
                match split {
                    // Left goes to the top: order is kept.
                    Split::Columns => *split = Split::Rows,
                    // Top goes to the right: order is reversed.
                    Split::Rows => {
                        *split = Split::Columns;
                        std::mem::swap(a, b);
                        *ratio = 1.0 - *ratio;
                    }
                }
                rec(a);
                rec(b);
            }
        }
        if let Some(root) = &mut self.root {
            rec(root);
        }
    }

    /// Gives every window the same share of the screen.
    pub fn balance(&mut self) {
        fn rec(node: &mut Node) {
            if let Node::Split { ratio, a, b, .. } = node {
                let (na, nb) = (a.leaf_count() as f32, b.leaf_count() as f32);
                *ratio = na / (na + nb);
                rec(a);
                rec(b);
            }
        }
        if let Some(root) = &mut self.root {
            rec(root);
        }
    }

    fn with_parent(
        &mut self,
        win: WinId,
        f: impl FnOnce(&mut Split, &mut f32, &mut Box<Node>, &mut Box<Node>),
    ) -> bool {
        let Some(path) = self.path(win) else {
            return false;
        };
        let Some((_, parent)) = path.split_last() else {
            return false;
        };
        if let Node::Split { split, ratio, a, b } = self.node_at_mut(parent) {
            f(split, ratio, a, b);
            true
        } else {
            false
        }
    }

    fn path(&self, win: WinId) -> Option<Vec<Side>> {
        let root = self.root.as_ref()?;
        let mut path = Vec::new();
        root.path_to(win, &mut path).then_some(path)
    }

    fn node_at_mut(&mut self, path: &[Side]) -> &mut Node {
        let mut node = self.root.as_mut().expect("path into empty tree");
        for side in path {
            node = match node {
                Node::Split { a, b, .. } => {
                    if *side == Side::A {
                        a
                    } else {
                        b
                    }
                }
                Node::Leaf(_) => unreachable!("path runs past a leaf"),
            };
        }
        node
    }
}

/// Finds the window next to `from` in direction `dir`, using the frames
/// produced by [`BspTree::layout`]. Picks the closest window that overlaps
/// `from` on the perpendicular axis; ties go to the larger overlap.
pub fn neighbor(layout: &[(WinId, Rect)], from: WinId, dir: Direction) -> Option<WinId> {
    const TOL: i32 = 1;
    fn overlap(a0: i32, a1: i32, b0: i32, b1: i32) -> i32 {
        a1.min(b1) - a0.max(b0)
    }
    let f = layout.iter().find(|(w, _)| *w == from)?.1;
    layout
        .iter()
        .filter(|(w, _)| *w != from)
        .filter_map(|(w, r)| {
            let (ahead, dist, ov) = match dir {
                Direction::Right => (
                    r.x >= f.max_x() - TOL,
                    r.x - f.max_x(),
                    overlap(f.y, f.max_y(), r.y, r.max_y()),
                ),
                Direction::Left => (
                    r.max_x() <= f.x + TOL,
                    f.x - r.max_x(),
                    overlap(f.y, f.max_y(), r.y, r.max_y()),
                ),
                Direction::Down => (
                    r.y >= f.max_y() - TOL,
                    r.y - f.max_y(),
                    overlap(f.x, f.max_x(), r.x, r.max_x()),
                ),
                Direction::Up => (
                    r.max_y() <= f.y + TOL,
                    f.y - r.max_y(),
                    overlap(f.x, f.max_x(), r.x, r.max_x()),
                ),
            };
            (ahead && ov > 0).then_some((dist, -ov, *w))
        })
        .min()
        .map(|(_, _, w)| w)
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Rect = Rect::new(0, 0, 1000, 600);
    const GAP: i32 = 10;

    fn frame(t: &BspTree, w: WinId) -> Rect {
        t.layout(AREA, GAP)
            .into_iter()
            .find(|(x, _)| *x == w)
            .unwrap()
            .1
    }

    fn tree(ids: &[WinId]) -> BspTree {
        let mut t = BspTree::new();
        let mut last = None;
        for &id in ids {
            assert!(t.insert(id, last, AREA, GAP, 0.5));
            last = Some(id);
        }
        t
    }

    #[test]
    fn splits_take_turns_so_no_three_sit_side_by_side() {
        // 1 | 2, then a new window next to the wide 1: stacked under it,
        // not a third column, though 1 is wider than tall.
        let mut t = BspTree::new();
        t.insert(1, None, AREA, GAP, 0.5);
        t.insert(2, Some(1), AREA, GAP, 0.5);
        t.insert(3, Some(1), AREA, GAP, 0.5);
        let layout: std::collections::HashMap<_, _> = t.layout(AREA, GAP).into_iter().collect();
        assert_eq!(layout[&1].x, layout[&3].x);
        assert!(layout[&3].y > layout[&1].y);
        assert!(layout[&2].x > layout[&1].x);
    }

    #[test]
    fn first_window_fills_area() {
        let t = tree(&[1]);
        assert_eq!(t.layout(AREA, GAP), vec![(1, AREA)]);
    }

    #[test]
    fn second_window_splits_longer_side() {
        let t = tree(&[1, 2]);
        assert_eq!(frame(&t, 1), Rect::new(0, 0, 495, 600));
        assert_eq!(frame(&t, 2), Rect::new(505, 0, 495, 600));
    }

    #[test]
    fn third_window_splits_tall_leaf_into_rows() {
        let t = tree(&[1, 2, 3]);
        assert_eq!(frame(&t, 1), Rect::new(0, 0, 495, 600));
        assert_eq!(frame(&t, 2), Rect::new(505, 0, 495, 295));
        assert_eq!(frame(&t, 3), Rect::new(505, 305, 495, 295));
    }

    #[test]
    fn duplicate_insert_is_rejected() {
        let mut t = tree(&[1, 2]);
        assert!(!t.insert(2, None, AREA, GAP, 0.5));
        assert_eq!(t.len(), 2);
    }

    #[test]
    fn remove_promotes_sibling() {
        let mut t = tree(&[1, 2, 3]);
        assert!(t.remove(2));
        assert_eq!(frame(&t, 3), Rect::new(505, 0, 495, 600));
        assert!(t.remove(1));
        assert_eq!(t.layout(AREA, GAP), vec![(3, AREA)]);
        assert!(t.remove(3));
        assert!(t.is_empty());
        assert!(!t.remove(3));
    }

    #[test]
    fn neighbors() {
        let t = tree(&[1, 2, 3]);
        let l = t.layout(AREA, GAP);
        assert_eq!(neighbor(&l, 1, Direction::Right), Some(2));
        assert_eq!(neighbor(&l, 3, Direction::Left), Some(1));
        assert_eq!(neighbor(&l, 2, Direction::Down), Some(3));
        assert_eq!(neighbor(&l, 3, Direction::Up), Some(2));
        assert_eq!(neighbor(&l, 1, Direction::Left), None);
        assert_eq!(neighbor(&l, 1, Direction::Up), None);
    }

    #[test]
    fn swap_exchanges_frames() {
        let mut t = tree(&[1, 2, 3]);
        let (f1, f3) = (frame(&t, 1), frame(&t, 3));
        assert!(t.swap(1, 3));
        assert_eq!(frame(&t, 3), f1);
        assert_eq!(frame(&t, 1), f3);
    }

    #[test]
    fn resize_grows_toward_direction() {
        let mut t = tree(&[1, 2]);
        assert!(t.resize(1, Direction::Right, 0.1));
        assert_eq!(frame(&t, 1).w, 594);
        // Window 2 has no right border to move: its left border moves right instead.
        assert!(t.resize(2, Direction::Right, 0.1));
        assert_eq!(frame(&t, 1).w, 693);
        // No row split exists.
        assert!(!t.resize(1, Direction::Down, 0.1));
    }

    #[test]
    fn resize_picks_nearest_matching_border() {
        let mut t = tree(&[1, 2, 3]);
        // Window 3 sits in the bottom row of the right column: moving up hits the row split.
        assert!(t.resize(3, Direction::Up, 0.1));
        assert!(frame(&t, 3).h > 295);
        assert_eq!(frame(&t, 1), Rect::new(0, 0, 495, 600));
    }

    #[test]
    fn set_border_follows_dragged_edge() {
        let mut t = tree(&[1, 2, 3]);
        // Drag window 1's right edge to x = 600.
        assert!(t.set_border(1, Direction::Right, 600, AREA, GAP));
        assert_eq!(frame(&t, 1).max_x(), 600);
        assert_eq!(frame(&t, 2).x, 610);
        // Drag window 3's top edge to y = 200: the border between 2 and 3 moves.
        assert!(t.set_border(3, Direction::Up, 200 - GAP, AREA, GAP));
        assert_eq!(frame(&t, 3).y, 200);
        assert_eq!(frame(&t, 2).max_y(), 190);
        // Window 1's left edge is the screen edge.
        assert!(!t.set_border(1, Direction::Left, 50, AREA, GAP));
    }

    #[test]
    fn ratio_is_clamped() {
        let mut t = tree(&[1, 2]);
        for _ in 0..20 {
            t.resize(1, Direction::Right, 0.2);
        }
        assert!(frame(&t, 2).w >= 20);
    }

    #[test]
    fn toggle_split_and_flip() {
        let mut t = tree(&[1, 2]);
        assert!(t.toggle_split(2));
        assert_eq!(frame(&t, 1), Rect::new(0, 0, 1000, 295));
        assert!(t.flip(1));
        assert_eq!(frame(&t, 2), Rect::new(0, 0, 1000, 295));
        let mut single = tree(&[1]);
        assert!(!single.toggle_split(1));
    }

    #[test]
    fn rotate_clockwise() {
        let mut t = tree(&[1, 2]);
        t.rotate();
        assert_eq!(frame(&t, 1).y, 0);
        assert_eq!(frame(&t, 2).y, 305);
        t.rotate();
        // Top window moved to the right.
        assert_eq!(frame(&t, 1).x, 505);
        assert_eq!(frame(&t, 2).x, 0);
        t.rotate();
        t.rotate();
        assert_eq!(t.layout(AREA, GAP), tree(&[1, 2]).layout(AREA, GAP));
    }

    #[test]
    fn balance_equalizes_shares() {
        let mut t = tree(&[1, 2, 3]);
        t.toggle_split(3); // right column becomes two side-by-side windows
        t.balance();
        let widths: Vec<i32> = t.layout(AREA, GAP).iter().map(|(_, r)| r.w).collect();
        let (min, max) = (widths.iter().min().unwrap(), widths.iter().max().unwrap());
        // Off by at most the gap, which the outer split spends on one side.
        assert!(max - min <= GAP, "{widths:?}");
    }

    #[test]
    fn parent_rect_is_the_shared_area() {
        let t = tree(&[1, 2, 3]);
        assert_eq!(t.parent_rect(1, AREA, GAP, &MinSizes::new()), Some(AREA));
        // 2 and 3 share the right column.
        assert_eq!(
            t.parent_rect(3, AREA, GAP, &MinSizes::new()),
            Some(Rect::new(505, 0, 495, 600))
        );
        assert_eq!(t.parent_rect(9, AREA, GAP, &MinSizes::new()), None);
        assert_eq!(tree(&[1]).parent_rect(1, AREA, GAP, &MinSizes::new()), None);
    }

    #[test]
    fn minimum_sizes_are_kept() {
        let mut t = tree(&[1, 2, 3]);
        let mins = MinSizes::from([(1, (600, 0)), (3, (0, 400))]);
        let l = t.layout_with(AREA, GAP, &mins);
        let f = |w: WinId| l.iter().find(|(x, _)| *x == w).unwrap().1;
        assert_eq!(f(1).w, 600);
        assert_eq!(f(2).x, 610);
        assert_eq!(f(3).h, 400);
        assert_eq!(f(2).h, 190);
        // Without minimums nothing changes.
        assert_eq!(
            t.layout_with(AREA, GAP, &MinSizes::new()),
            t.layout(AREA, GAP)
        );

        // Settling moves the ratios to what the minimums allow.
        t.settle_ratios(AREA, GAP, &mins);
        assert_eq!(t.layout(AREA, GAP), l);
    }

    #[test]
    fn minimum_sizes_add_up_through_the_tree() {
        // 1 | (2 / 3): the right column is as wide as its widest window.
        let t = tree(&[1, 2, 3]);
        let mins = MinSizes::from([(3, (700, 0))]);
        let l = t.layout_with(AREA, GAP, &mins);
        assert_eq!(l[0].1.w, 1000 - GAP - 700);
        assert_eq!(l[1].1.w, 700);
    }

    #[test]
    fn impossible_minimums_share_the_space() {
        let t = tree(&[1, 2]);
        let mins = MinSizes::from([(1, (900, 0)), (2, (900, 0))]);
        let l = t.layout_with(AREA, GAP, &mins);
        assert_eq!(l[0].1.w, 495);
        assert_eq!(l[1].1.w, 495);
    }

    #[test]
    fn windows_in_order() {
        let mut t = tree(&[5, 7, 9]);
        assert_eq!(t.windows(), vec![5, 7, 9]);
        t.swap(5, 9);
        assert_eq!(t.windows(), vec![9, 7, 5]);
    }
}
