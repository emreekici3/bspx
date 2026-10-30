//! Basic integer geometry in screen points (macOS global coordinates:
//! origin at the top-left of the main display, y grows downwards).

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    /// The part of this rect a grid cell covers: `[rows, cols, x, y, w, h]`,
    /// as in yabai's `grid=`. Edges are rounded so neighbouring cells meet.
    pub fn grid_cell(&self, [rows, cols, x, y, w, h]: [u32; 6]) -> Rect {
        let cols = cols.max(1) as i64;
        let rows = rows.max(1) as i64;
        let col = |i: u32| self.x + (self.w as i64 * i as i64 / cols) as i32;
        let row = |i: u32| self.y + (self.h as i64 * i as i64 / rows) as i32;
        let (x0, y0) = (col(x), row(y));
        Rect::new(x0, y0, col(x + w) - x0, row(y + h) - y0)
    }

    /// Where a `w` x `h` window goes in `cell` (a part of `self`) when it
    /// cannot take the cell's size: against each edge of `self` the cell
    /// touches, centred in the cell along the others, and kept inside `self`.
    pub fn align_in(&self, cell: Rect, w: i32, h: i32) -> Rect {
        let along = |start: i32, size: i32, len: i32, lo: i32, hi: i32| {
            let (at_start, at_end) = (start <= lo, start + size >= hi);
            let pos = match (at_start, at_end) {
                (true, false) => start,
                (false, true) => start + size - len,
                _ => start + (size - len) / 2,
            };
            pos.min(hi - len).max(lo)
        };
        Rect::new(
            along(cell.x, cell.w, w, self.x, self.max_x()),
            along(cell.y, cell.h, h, self.y, self.max_y()),
            w,
            h,
        )
    }

    /// The edge of this rect nearest to a point inside it, relative to its
    /// size (so a tall window's top and bottom count as much as its sides).
    pub fn nearest_side(&self, x: f32, y: f32) -> Direction {
        let (cx, cy) = self.center();
        let dx = (x - cx) / self.w.max(1) as f32;
        let dy = (y - cy) / self.h.max(1) as f32;
        if dx.abs() >= dy.abs() {
            if dx < 0.0 {
                Direction::Left
            } else {
                Direction::Right
            }
        } else if dy < 0.0 {
            Direction::Up
        } else {
            Direction::Down
        }
    }

    /// The half of this rect on `side`.
    pub fn half(&self, side: Direction) -> Rect {
        let (hw, hh) = (self.w / 2, self.h / 2);
        match side {
            Direction::Left => Rect::new(self.x, self.y, hw, self.h),
            Direction::Right => Rect::new(self.x + hw, self.y, self.w - hw, self.h),
            Direction::Up => Rect::new(self.x, self.y, self.w, hh),
            Direction::Down => Rect::new(self.x, self.y + hh, self.w, self.h - hh),
        }
    }

    pub const fn max_x(&self) -> i32 {
        self.x + self.w
    }

    pub const fn max_y(&self) -> i32 {
        self.y + self.h
    }

    pub fn center(&self) -> (f32, f32) {
        (
            self.x as f32 + self.w as f32 / 2.0,
            self.y as f32 + self.h as f32 / 2.0,
        )
    }

    /// Shrinks the rect by the given insets. Never returns a negative size.
    pub fn inset(&self, p: &Padding) -> Rect {
        Rect {
            x: self.x + p.left,
            y: self.y + p.top,
            w: (self.w - p.left - p.right).max(1),
            h: (self.h - p.top - p.bottom).max(1),
        }
    }

    /// Whether the two rects overlap (touching edges do not count).
    pub fn intersects(&self, other: &Rect) -> bool {
        self.x < other.max_x()
            && other.x < self.max_x()
            && self.y < other.max_y()
            && other.y < self.max_y()
    }

    pub fn contains_point(&self, x: f32, y: f32) -> bool {
        x >= self.x as f32
            && x < self.max_x() as f32
            && y >= self.y as f32
            && y < self.max_y() as f32
    }
}

/// Edge insets.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Padding {
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
    pub left: i32,
}

impl Padding {
    pub const fn uniform(v: i32) -> Self {
        Self {
            top: v,
            right: v,
            bottom: v,
            left: v,
        }
    }
}

/// A cardinal direction, used by focus/swap/resize commands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Direction {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "left" | "west" | "h" => Some(Self::Left),
            "right" | "east" | "l" => Some(Self::Right),
            "up" | "north" | "k" => Some(Self::Up),
            "down" | "south" | "j" => Some(Self::Down),
            _ => None,
        }
    }
}

/// The screen edge the Dock is attached to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DockEdge {
    Bottom,
    Left,
    Right,
}

/// A Dock that is shown (not set to hide itself): its edge, and how many
/// points of that edge it takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dock {
    pub edge: DockEdge,
    pub thickness: i32,
}

/// The area windows may use on a display: below the menu bar, whose bottom
/// is taken from `visible` (the system's visible frame), and clear of `dock`.
/// With `dock` set to `None` (it hides itself, or sits on another display)
/// windows reach the display's other three edges.
pub fn usable_area(display: Rect, visible: Rect, dock: Option<Dock>) -> Rect {
    let top = visible.y.clamp(display.y, display.max_y() - 1);
    let mut r = Rect::new(display.x, top, display.w, display.max_y() - top);
    if let Some(d) = dock {
        let t = d.thickness.clamp(0, r.w.min(r.h) / 2);
        match d.edge {
            DockEdge::Bottom => r.h -= t,
            DockEdge::Left => {
                r.x += t;
                r.w -= t;
            }
            DockEdge::Right => r.w -= t,
        }
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    const DISPLAY: Rect = Rect::new(0, 0, 1512, 982);
    /// Menu bar 33pt, Dock 70pt at the bottom.
    const VISIBLE: Rect = Rect::new(0, 33, 1512, 879);

    #[test]
    fn sides_and_halves() {
        let r = Rect::new(0, 0, 400, 200);
        assert_eq!(r.nearest_side(10.0, 100.0), Direction::Left);
        assert_eq!(r.nearest_side(390.0, 90.0), Direction::Right);
        assert_eq!(r.nearest_side(200.0, 10.0), Direction::Up);
        assert_eq!(r.nearest_side(150.0, 190.0), Direction::Down);
        assert_eq!(r.half(Direction::Left), Rect::new(0, 0, 200, 200));
        assert_eq!(r.half(Direction::Right), Rect::new(200, 0, 200, 200));
        assert_eq!(r.half(Direction::Up), Rect::new(0, 0, 400, 100));
        assert_eq!(r.half(Direction::Down), Rect::new(0, 100, 400, 100));
    }

    #[test]
    fn grid_cells() {
        let area = Rect::new(10, 30, 1000, 600);
        assert_eq!(area.grid_cell([1, 1, 0, 0, 1, 1]), area);
        assert_eq!(
            area.grid_cell([1, 2, 1, 0, 1, 1]),
            Rect::new(510, 30, 500, 600)
        );
        assert_eq!(
            area.grid_cell([4, 4, 3, 0, 1, 1]),
            Rect::new(760, 30, 250, 150)
        );
        // Cells that do not divide evenly still meet without gaps.
        let odd = Rect::new(0, 0, 100, 100);
        let a = odd.grid_cell([1, 3, 0, 0, 1, 1]);
        let b = odd.grid_cell([1, 3, 1, 0, 1, 1]);
        let c = odd.grid_cell([1, 3, 2, 0, 1, 1]);
        assert_eq!((a.max_x(), b.max_x(), c.max_x()), (b.x, c.x, 100));
    }

    #[test]
    fn hidden_dock_frees_the_bottom() {
        assert_eq!(
            usable_area(DISPLAY, VISIBLE, None),
            Rect::new(0, 33, 1512, 949)
        );
    }

    #[test]
    fn shown_dock_is_kept_clear() {
        let bottom = Dock {
            edge: DockEdge::Bottom,
            thickness: 70,
        };
        assert_eq!(
            usable_area(DISPLAY, VISIBLE, Some(bottom)),
            Rect::new(0, 33, 1512, 879)
        );
        let left = Dock {
            edge: DockEdge::Left,
            thickness: 60,
        };
        assert_eq!(
            usable_area(DISPLAY, VISIBLE, Some(left)),
            Rect::new(60, 33, 1452, 949)
        );
        let right = Dock {
            edge: DockEdge::Right,
            thickness: 60,
        };
        assert_eq!(
            usable_area(DISPLAY, VISIBLE, Some(right)),
            Rect::new(0, 33, 1452, 949)
        );
    }

    #[test]
    fn stale_visible_frame_does_not_matter() {
        // A visible frame that still leaves room for a Dock that now hides.
        let stale = Rect::new(0, 33, 1512, 700);
        assert_eq!(usable_area(DISPLAY, stale, None).max_y(), 982);
    }

    #[test]
    fn absurd_dock_is_bounded() {
        let huge = Dock {
            edge: DockEdge::Bottom,
            thickness: 5000,
        };
        assert!(usable_area(DISPLAY, VISIBLE, Some(huge)).h > 0);
    }
}
