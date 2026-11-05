//! Binary space partitioning layout.
//!
//! Every leaf holds one window; every inner node splits its area in two,
//! either side by side ([`Split::Columns`]) or stacked ([`Split::Rows`]).
//! A new window always splits an existing leaf along that leaf's longer side,
//! so the screen is carved up into a binary tree of panes.

use crate::geometry::{Direction, Rect};

pub type WinId = u32;

const MIN_RATIO: f32 = 0.05;
const MAX_RATIO: f32 = 0.95;
/// How far windows may overlap and still count as side by side: some cannot
/// quite shrink to their slot.
const CUT_TOLERANCE: i32 = 8;

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

/// Where a moved border is, seen from a node next to it: at its start (left
/// or top) or at its end (right or bottom).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Edge {
    Start,
    End,
}

/// The width (`Columns`) or height (`Rows`) of `r`.
fn extent(r: Rect, split: Split) -> i32 {
    match split {
        Split::Columns => r.w,
        Split::Rows => r.h,
    }
}

/// Re-splits `node` (now in `area`, before in `old_area`, the moved border
/// at its `edge`) so that its windows away from the border keep their size:
/// see `BspTree::keep_far_sizes`.
fn absorb(
    node: &mut Node,
    old: &Node,
    area: Rect,
    old_area: Rect,
    border: Split,
    edge: Edge,
    gap: i32,
) {
    let (
        Node::Split { split, ratio, a, b },
        Node::Split {
            ratio: old_ratio,
            a: old_a,
            b: old_b,
            ..
        },
    ) = (node, old)
    else {
        return;
    };
    let (old_ra, old_rb) = split_rect(old_area, *split, *old_ratio, gap);
    if *split == border {
        // The half away from the border keeps its size.
        let avail = (extent(area, border) - gap).max(2);
        let first = match edge {
            Edge::End => extent(old_ra, border),
            Edge::Start => avail - extent(old_rb, border),
        };
        *ratio = (first as f32 / avail as f32).clamp(MIN_RATIO, MAX_RATIO);
        let (ra, rb) = split_rect(area, *split, *ratio, gap);
        match edge {
            Edge::End => absorb(b, old_b, rb, old_rb, border, edge, gap),
            Edge::Start => absorb(a, old_a, ra, old_ra, border, edge, gap),
        }
    } else {
        // Both halves are against the border.
        let (ra, rb) = split_rect(area, *split, *ratio, gap);
        absorb(a, old_a, ra, old_ra, border, edge, gap);
        absorb(b, old_b, rb, old_rb, border, edge, gap);
    }
}

/// The first place the windows part into two groups along one axis: how many
/// lie before it, and where the first group ends and the second starts.
fn cut(windows: &mut [(WinId, Rect)], split: Split) -> Option<(usize, i32, i32)> {
    let span = |r: &Rect| match split {
        Split::Columns => (r.x, r.x + r.w),
        Split::Rows => (r.y, r.y + r.h),
    };
    windows.sort_by_key(|(_, r)| span(r));
    let mut end = i32::MIN;
    for i in 1..windows.len() {
        end = end.max(span(&windows[i - 1].1).1);
        let start = span(&windows[i].1).0;
        if end <= start + CUT_TOLERANCE {
            return Some((i, end, start));
        }
    }
    None
}

/// The tree that lays `windows` out in `area` as they are now, if one does.
fn carve(windows: &mut [(WinId, Rect)], area: Rect, gap: i32) -> Option<Node> {
    if let [(id, _)] = windows {
        return Some(Node::Leaf(*id));
    }
    let (split, (at, end, start)) = cut(windows, Split::Columns)
        .map(|c| (Split::Columns, c))
        .or_else(|| cut(windows, Split::Rows).map(|c| (Split::Rows, c)))?;
    let (origin, size) = match split {
        Split::Columns => (area.x, area.w),
        Split::Rows => (area.y, area.h),
    };
    let first = (end + start - gap) as f32 / 2.0 - origin as f32;
    let ratio = (first / (size - gap).max(1) as f32).clamp(MIN_RATIO, MAX_RATIO);
    let (rect_a, rect_b) = split_rect(area, split, ratio, gap);
    let (before, after) = windows.split_at_mut(at);
    Some(Node::Split {
        split,
        ratio,
        a: Box::new(carve(before, rect_a, gap)?),
        b: Box::new(carve(after, rect_b, gap)?),
    })
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

    /// The tree of windows already laid out without overlaps, or `None` if
    /// nothing like a tree would keep them where they are.
    pub fn from_frames(windows: &[(WinId, Rect)], area: Rect, gap: i32) -> Option<Self> {
        if windows.is_empty() {
            return None;
        }
        let mut windows = windows.to_vec();
        Some(Self {
            root: Some(carve(&mut windows, area, gap)?),
        })
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

    /// The tree part of the way from `self` to `to`, which must have the
    /// same windows in the same splits: every ratio `t` of the way (0 is
    /// `self`, 1 is `to`). `None` if the two trees differ otherwise.
    pub fn interpolate(&self, to: &BspTree, t: f32) -> Option<BspTree> {
        fn rec(a: &Node, b: &Node, t: f32) -> Option<Node> {
            match (a, b) {
                (Node::Leaf(x), Node::Leaf(y)) if x == y => Some(Node::Leaf(*x)),
                (
                    Node::Split {
                        split: s1,
                        ratio: r1,
                        a: a1,
                        b: b1,
                    },
                    Node::Split {
                        split: s2,
                        ratio: r2,
                        a: a2,
                        b: b2,
                    },
                ) if s1 == s2 => Some(Node::Split {
                    split: *s1,
                    ratio: r1 + (r2 - r1) * t,
                    a: Box::new(rec(a1, a2, t)?),
                    b: Box::new(rec(b1, b2, t)?),
                }),
                _ => None,
            }
        }
        match (&self.root, &to.root) {
            (Some(a), Some(b)) => Some(BspTree {
                root: Some(rec(a, b, t)?),
            }),
            (None, None) => Some(BspTree::new()),
            _ => None,
        }
    }

    /// Computes the frame of every window inside `area`: each split gives
    /// its first half its ratio of the room, whatever the windows in it
    /// would rather have.
    pub fn layout(&self, area: Rect, gap: i32) -> Vec<(WinId, Rect)> {
        fn rec(node: &Node, area: Rect, gap: i32, out: &mut Vec<(WinId, Rect)>) {
            match node {
                Node::Leaf(w) => out.push((*w, area)),
                Node::Split { split, ratio, a, b } => {
                    let (ra, rb) = split_rect(area, *split, *ratio, gap);
                    rec(a, ra, gap, out);
                    rec(b, rb, gap, out);
                }
            }
        }
        let mut out = Vec::new();
        if let Some(root) = &self.root {
            rec(root, area, gap, &mut out);
        }
        out
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

    /// Puts `new` beside `target`, on its `side`: the two share the target's
    /// old slot equally. Returns `false` if `new` is already managed or
    /// `target` is not.
    pub fn insert_at(&mut self, new: WinId, target: WinId, side: Direction) -> bool {
        if new == target || self.contains(new) {
            return false;
        }
        let Some(path) = self.path(target) else {
            return false;
        };
        let split = match side {
            Direction::Left | Direction::Right => Split::Columns,
            Direction::Up | Direction::Down => Split::Rows,
        };
        let (a, b) = match side {
            Direction::Left | Direction::Up => (new, target),
            Direction::Right | Direction::Down => (target, new),
        };
        *self.node_at_mut(&path) = Node::Split {
            split,
            ratio: 0.5,
            a: Box::new(Node::Leaf(a)),
            b: Box::new(Node::Leaf(b)),
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

    /// Puts window `new` where `old` is; `old` leaves the tree.
    pub fn replace(&mut self, old: WinId, new: WinId) -> bool {
        if self.contains(new) {
            return false;
        }
        let Some(path) = self.path(old) else {
            return false;
        };
        *self.node_at_mut(&path) = Node::Leaf(new);
        true
    }

    /// Moves a border of `win` by `delta` (fraction of the parent's size)
    /// in direction `dir`: the arrow says where the border goes. The border
    /// moved is the window's left (or top) one, the nearest if there are
    /// several; a window against the left (top) of the screen has none, and
    /// its right (bottom) border moves. So a window on the right grows with
    /// left and shrinks with right, one on the left the other way round,
    /// and one in between like one on the right.
    ///
    /// Only the windows along the moved border change size: on both sides,
    /// the windows further away keep theirs (see `keep_far_sizes`). `area`
    /// and `gap` are what the tree is laid out in.
    pub fn resize(&mut self, win: WinId, dir: Direction, delta: f32, area: Rect, gap: i32) -> bool {
        let Some(path) = self.path(win) else {
            return false;
        };
        let old = self.clone();
        let wanted = match dir {
            Direction::Left | Direction::Right => Split::Columns,
            Direction::Up | Direction::Down => Split::Rows,
        };
        // The window is the second half of a split whose border is its left
        // (top) one.
        let facing = Side::B;

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
        } else {
            return false;
        }
        self.keep_far_sizes(&path[..depth], &old, area, gap);
        true
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
        let old = self.clone();
        if let Node::Split { ratio, .. } = self.node_at_mut(&path[..depth]) {
            *ratio = new_ratio.clamp(MIN_RATIO, MAX_RATIO);
        } else {
            return false;
        }
        self.keep_far_sizes(&path[..depth], &old, area, gap);
        true
    }

    /// After the split at `prefix` moved its border (`old` is the tree from
    /// before), only the windows touching that border change size. On each
    /// side, every split of the same kind keeps the size of its half away
    /// from the border; the half against it takes the change, down to the
    /// windows themselves. (A split the other way has both halves against
    /// the border: both change, as they must.)
    fn keep_far_sizes(&mut self, prefix: &[Side], old: &BspTree, area: Rect, gap: i32) {
        let Some(at) = old.area_at(prefix, area, gap) else {
            return;
        };
        let Some(was) = old.node_at(prefix) else {
            return;
        };
        if let (
            Node::Split { split, ratio, a, b },
            Node::Split {
                ratio: old_ratio,
                a: old_a,
                b: old_b,
                ..
            },
        ) = (self.node_at_mut(prefix), was)
        {
            let (ra, rb) = split_rect(at, *split, *ratio, gap);
            let (old_ra, old_rb) = split_rect(at, *split, *old_ratio, gap);
            absorb(a, old_a, ra, old_ra, *split, Edge::End, gap);
            absorb(b, old_b, rb, old_rb, *split, Edge::Start, gap);
        }
    }

    /// The area of the node at `path`.
    fn area_at(&self, path: &[Side], area: Rect, gap: i32) -> Option<Rect> {
        let mut node = self.root.as_ref()?;
        let mut rect = area;
        for side in path {
            let Node::Split { split, ratio, a, b } = node else {
                return None;
            };
            let (ra, rb) = split_rect(rect, *split, *ratio, gap);
            (node, rect) = if *side == Side::A { (a, ra) } else { (b, rb) };
        }
        Some(rect)
    }

    fn node_at(&self, path: &[Side]) -> Option<&Node> {
        let mut node = self.root.as_ref()?;
        for side in path {
            let Node::Split { a, b, .. } = node else {
                return None;
            };
            node = if *side == Side::A { a } else { b };
        }
        Some(node)
    }

    /// The area of the split that directly contains `win`: what the window
    /// and its sibling share. `None` for a lone window or an unknown one.
    pub fn parent_rect(&self, win: WinId, area: Rect, gap: i32) -> Option<Rect> {
        let path = self.path(win)?;
        let (_, parent) = path.split_last()?;
        let mut node = self.root.as_ref()?;
        let mut rect = area;
        for side in parent {
            let Node::Split { split, ratio, a, b } = node else {
                return None;
            };
            let (ra, rb) = split_rect(rect, *split, *ratio, gap);
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

    /// Sets every split back to `ratio`: the layout the windows had when
    /// they opened, before any resize.
    pub fn equalize(&mut self, ratio: f32) {
        fn rec(node: &mut Node, ratio: f32) {
            if let Node::Split { ratio: r, a, b, .. } = node {
                *r = ratio;
                rec(a, ratio);
                rec(b, ratio);
            }
        }
        if let Some(root) = &mut self.root {
            rec(root, ratio.clamp(MIN_RATIO, MAX_RATIO));
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
mod frames_tests {
    use super::*;

    const AREA: Rect = Rect::new(12, 44, 1488, 926);
    const GAP: i32 = 8;

    fn sized(tree: &BspTree) -> Vec<(WinId, Rect)> {
        let mut out = tree.layout(AREA, GAP);
        out.sort_by_key(|(w, _)| *w);
        out
    }

    #[test]
    fn a_layout_comes_back_from_its_frames() {
        // One on the left, two stacked on the right, not half and half.
        let mut tree = BspTree::new();
        tree.insert(1, None, AREA, GAP, 0.3);
        tree.insert(2, Some(1), AREA, GAP, 0.3);
        tree.insert(3, Some(2), AREA, GAP, 0.7);
        tree.insert(4, Some(1), AREA, GAP, 0.5);
        let frames = sized(&tree);
        for order in [[0, 1, 2, 3], [3, 2, 1, 0], [2, 0, 3, 1]] {
            let shuffled: Vec<_> = order.iter().map(|i| frames[*i]).collect();
            let back = BspTree::from_frames(&shuffled, AREA, GAP).unwrap();
            assert_eq!(sized(&back), frames);
        }
    }

    #[test]
    fn windows_that_overlap_make_no_tree() {
        let overlapping = [
            (1, Rect::new(100, 100, 400, 400)),
            (2, Rect::new(300, 200, 400, 400)),
        ];
        assert_eq!(BspTree::from_frames(&overlapping, AREA, GAP), None);
        assert_eq!(BspTree::from_frames(&[], AREA, GAP), None);
    }

    #[test]
    fn a_pinwheel_makes_no_tree() {
        let pinwheel = [
            (1, Rect::new(0, 0, 200, 100)),
            (2, Rect::new(200, 0, 100, 200)),
            (3, Rect::new(100, 200, 200, 100)),
            (4, Rect::new(0, 100, 100, 200)),
        ];
        assert_eq!(BspTree::from_frames(&pinwheel, AREA, GAP), None);
    }

    #[test]
    fn a_window_a_little_too_large_still_counts() {
        let frames = [
            (1, Rect::new(12, 44, 750, 926)),
            (2, Rect::new(758, 44, 742, 926)),
        ];
        assert!(BspTree::from_frames(&frames, AREA, GAP).is_some());
    }
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
        assert!(t.resize(1, Direction::Right, 0.1, AREA, GAP));
        assert_eq!(frame(&t, 1).w, 594);
        // Window 2 has no right border to move: its left border moves right instead.
        assert!(t.resize(2, Direction::Right, 0.1, AREA, GAP));
        assert_eq!(frame(&t, 1).w, 693);
        // No row split exists.
        assert!(!t.resize(1, Direction::Down, 0.1, AREA, GAP));
    }

    #[test]
    fn resize_picks_nearest_matching_border() {
        let mut t = tree(&[1, 2, 3]);
        // Window 3 sits in the bottom row of the right column: moving up hits the row split.
        assert!(t.resize(3, Direction::Up, 0.1, AREA, GAP));
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
            t.resize(1, Direction::Right, 0.2, AREA, GAP);
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
    fn equalize_puts_every_border_back_in_the_middle() {
        let fresh = tree(&[1, 2, 3]);
        let mut t = fresh.clone();
        t.resize(3, Direction::Left, 0.2, AREA, GAP);
        t.resize(3, Direction::Up, 0.2, AREA, GAP);
        assert_ne!(t.layout(AREA, GAP), fresh.layout(AREA, GAP));
        t.equalize(0.5);
        assert_eq!(t.layout(AREA, GAP), fresh.layout(AREA, GAP));
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
        assert_eq!(t.parent_rect(1, AREA, GAP), Some(AREA));
        // 2 and 3 share the right column.
        assert_eq!(
            t.parent_rect(3, AREA, GAP),
            Some(Rect::new(505, 0, 495, 600))
        );
        assert_eq!(t.parent_rect(9, AREA, GAP), None);
        assert_eq!(tree(&[1]).parent_rect(1, AREA, GAP), None);
    }

    #[test]
    fn windows_in_order() {
        let mut t = tree(&[5, 7, 9]);
        assert_eq!(t.windows(), vec![5, 7, 9]);
        t.swap(5, 9);
        assert_eq!(t.windows(), vec![9, 7, 5]);
    }
    #[test]
    fn interpolates_ratios_between_trees_of_one_shape() {
        let a = tree(&[1, 2, 3]);
        let mut b = a.clone();
        b.resize(1, Direction::Right, 0.2, AREA, GAP);
        let half = a.interpolate(&b, 0.5).unwrap();
        let w = |t: &BspTree| t.layout(AREA, GAP)[0].1.w;
        assert!(w(&a) < w(&half) && w(&half) < w(&b));
        assert_eq!(a.interpolate(&b, 0.0).unwrap(), a);
        let mut other = a.clone();
        other.swap(1, 2);
        assert!(a.interpolate(&other, 0.5).is_none(), "windows moved");
    }
    #[test]
    fn an_arrow_moves_a_border_its_way_left_or_top_border_first() {
        // Three side by side: 1 | 2 | 3.
        let mut three = tree(&[1, 2]);
        three.insert_at(3, 2, Direction::Right);
        let f = |t: &BspTree, w: WinId| {
            t.layout(AREA, GAP)
                .into_iter()
                .find(|(x, _)| *x == w)
                .unwrap()
                .1
        };
        // (window, arrow, grows): the border moved goes the arrow's way.
        for (win, dir, grows) in [
            (1, Direction::Right, true),
            (1, Direction::Left, false),
            (2, Direction::Left, true),
            (2, Direction::Right, false),
            (3, Direction::Left, true),
            (3, Direction::Right, false),
        ] {
            let mut t = three.clone();
            let before = f(&t, win);
            assert!(t.resize(win, dir, 0.1, AREA, GAP));
            let after = f(&t, win);
            assert_eq!(after.w > before.w, grows, "window {win} {dir:?}");
            // The edge that moved went the arrow's way.
            let moved = if after.x != before.x {
                after.x - before.x
            } else {
                after.max_x() - before.max_x()
            };
            match dir {
                Direction::Left => assert!(moved < 0, "window {win} {dir:?}"),
                _ => assert!(moved > 0, "window {win} {dir:?}"),
            }
        }
        // Stacked, the same with up and down: 1 / 2 / 3.
        let mut stack = BspTree::new();
        let tall = Rect::new(0, 0, 600, 1000);
        stack.insert(1, None, tall, GAP, 0.5);
        stack.insert(2, Some(1), tall, GAP, 0.5);
        stack.insert_at(3, 2, Direction::Down);
        let h = |t: &BspTree, w: WinId| {
            t.layout(tall, GAP)
                .into_iter()
                .find(|(x, _)| *x == w)
                .unwrap()
                .1
                .h
        };
        for (win, dir, grows) in [
            (1, Direction::Down, true),
            (2, Direction::Up, true),
            (2, Direction::Down, false),
            (3, Direction::Up, true),
        ] {
            let mut t = stack.clone();
            let before = h(&t, win);
            assert!(t.resize(win, dir, 0.1, tall, GAP));
            assert_eq!(h(&t, win) > before, grows, "window {win} {dir:?}");
        }
    }
    #[test]
    fn only_the_windows_along_a_moved_border_change_size() {
        // 1 | (2 | 3): the middle window's left border is the outer split.
        let mut three = tree(&[1, 2]);
        three.insert_at(3, 2, Direction::Right);
        let w = |t: &BspTree, win: WinId| {
            t.layout(AREA, GAP)
                .into_iter()
                .find(|(x, _)| *x == win)
                .unwrap()
                .1
        };
        // The middle window grows to the left: the right one stays as it is.
        let mut t = three.clone();
        let (one, two, right) = (w(&t, 1), w(&t, 2), w(&t, 3));
        t.resize(2, Direction::Left, 0.1, AREA, GAP);
        assert!(w(&t, 2).w > two.w);
        assert!(w(&t, 1).w < one.w);
        assert_eq!(
            w(&t, 3),
            right,
            "the window away from the border keeps its frame"
        );
        // The left window grows: only the middle one gives way.
        let mut t = three.clone();
        t.resize(1, Direction::Right, 0.1, AREA, GAP);
        assert!(w(&t, 1).w > one.w);
        assert!(w(&t, 2).w < two.w);
        assert_eq!(w(&t, 3), right);
        // Windows stacked against the border both change, as they must:
        // 1 | (2 / 3), window 1 grows.
        let mut t = tree(&[1, 2, 3]);
        let (two, three) = (w(&t, 2), w(&t, 3));
        t.resize(1, Direction::Right, 0.1, AREA, GAP);
        assert!(w(&t, 2).w < two.w && w(&t, 3).w < three.w);
    }
}
