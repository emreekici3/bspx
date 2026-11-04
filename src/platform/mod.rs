//! The window system interface the manager drives.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use crate::bsp::WinId;
use crate::geometry::Rect;

#[cfg(any(target_os = "macos", cberry_check_macos))]
pub mod macos;

#[derive(Clone, Debug, PartialEq)]
pub struct WindowInfo {
    pub id: WinId,
    pub frame: Rect,
    /// Owning application's name.
    pub app: String,
    /// A normal, resizable document window (not a dialog, sheet, panel,
    /// minimized or fullscreen window).
    pub tileable: bool,
    /// A normal window cberry can move, though perhaps not resize (a
    /// fixed-size one such as Calculator). Every tileable window is movable.
    pub movable: bool,
}

/// A display cberry tiles, as of the last look.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Display {
    /// The whole display.
    pub frame: Rect,
    /// The part windows are tiled in (menu bar and Dock excluded).
    pub area: Rect,
    /// The Space it shows.
    pub space: u64,
}

pub trait Backend {
    /// All windows on screen: those on the Spaces the displays show.
    fn windows(&mut self) -> Vec<WindowInfo>;
    /// The Space (virtual desktop) shown on the tiled display.
    fn active_space(&mut self) -> u64 {
        0
    }
    /// The Spaces of the tiled display, in Mission Control order (full-screen
    /// apps' Spaces left out).
    fn spaces(&mut self) -> Vec<u64> {
        vec![self.active_space()]
    }
    /// The Spaces of the display showing Space `shown`, in Mission Control
    /// order (full-screen apps' Spaces left out).
    fn display_spaces(&mut self, _shown: u64) -> Vec<u64> {
        self.spaces()
    }
    /// Switches the tiled display to Space n (numbered from 1).
    fn switch_to_space(&mut self, _n: usize) -> Result<(), String> {
        Err("switching Spaces is not supported here".into())
    }
    /// Shows Space `space` on its display.
    fn show_space(&mut self, space: u64) -> Result<(), String> {
        let n = self
            .spaces()
            .iter()
            .position(|s| *s == space)
            .ok_or("no such Space")?;
        self.switch_to_space(n + 1)
    }
    /// Moves a window to another Space; returns whether it could. The user
    /// may be taken there along with it.
    fn move_to_space(&mut self, _id: WinId, _space: u64) -> bool {
        false
    }
    /// Moves a window to another Space without touching the pointer or
    /// taking the user there; returns whether it could.
    fn place_on_space(&mut self, _id: WinId, _space: u64) -> bool {
        false
    }
    /// Every window that still exists, on any Space, minimised or hidden
    /// ones included. `None` if the backend cannot tell; then a window that
    /// is not on screen counts as closed.
    fn alive_windows(&mut self) -> Option<HashSet<WinId>> {
        None
    }
    /// The Space each window is on. Windows left out are on no Space
    /// (minimised, or their app is hidden).
    fn window_spaces(&mut self, ids: &[WinId]) -> HashMap<WinId, u64> {
        let space = self.active_space();
        ids.iter().map(|&id| (id, space)).collect()
    }
    /// The window with keyboard focus, if any.
    fn focused_window(&mut self) -> Option<WinId>;
    /// The usable area of the display (menu bar and Dock excluded).
    fn tiling_area(&mut self) -> Rect;
    /// The displays to tile, the main one (with the menu bar) first. Each
    /// shows a Space of its own; one that shares a Space with another (with
    /// "Displays have separate Spaces" off) is left out.
    fn displays(&mut self) -> Vec<Display> {
        let area = self.tiling_area();
        let space = self.active_space();
        vec![Display {
            frame: area,
            area,
            space,
        }]
    }
    /// Moves and resizes a window.
    fn set_frame(&mut self, id: WinId, frame: Rect);
    /// Moves a window from `prev` to `frame`, touching only what changed: a
    /// pure move must not resize, which makes the app lay out its content
    /// again and is the costly part of every animation frame.
    fn update_frame(&mut self, id: WinId, frame: Rect, _prev: Rect) {
        self.set_frame(id, frame);
    }
    /// Like [`update_frame`](Self::update_frame), for a frame in the middle
    /// of an animation: one that will be followed by more, so it may favour
    /// how the window looks over where it is safe to put it.
    fn animate_frame(&mut self, id: WinId, frame: Rect, prev: Rect) {
        self.update_frame(id, frame, prev);
    }
    /// Gives a window keyboard focus and raises it.
    fn focus(&mut self, id: WinId);
    /// Whether the left mouse button is held (the user may be dragging a window).
    fn mouse_down(&mut self) -> bool {
        false
    }
    /// The mouse pointer, in the same coordinates as window frames.
    fn cursor(&mut self) -> Option<(f32, f32)> {
        None
    }
    /// A window's current frame, read right now (cheap enough for every frame).
    fn window_frame(&mut self, _id: WinId) -> Option<Rect> {
        None
    }
    /// Where the window server has a window right now: quick and exact, even
    /// while its app is dragged around by the user.
    fn server_frame(&mut self, id: WinId) -> Option<Rect> {
        self.window_frame(id)
    }
    /// Shows the drop-target highlight over `rect`, placed just above window
    /// `above`, or hides it with `None`.
    fn show_overlay(&mut self, _overlay: Option<Overlay>) {}
    /// Shows the ring around the focused window, or hides it with `None`.
    fn show_border(&mut self, _border: Option<Border>) {}
    /// Lets the platform UI (the overlay window) process its events. Called
    /// once per frame.
    fn pump(&mut self) {}
    /// Waits for the next frame while something is moving: at most
    /// `remaining` by the clock, or until the display refreshes, for a
    /// backend that can tell.
    fn wait_frame(&mut self, remaining: Duration, _refresh: Duration) {
        std::thread::sleep(remaining);
    }
    /// Nothing is moving any more.
    fn stop_frames(&mut self) {}
    /// The display's highest refresh rate, in frames per second, to pace
    /// animations to; `None` if unknown.
    fn frame_rate(&mut self) -> Option<f64> {
        None
    }
    /// Whether the backend is still waiting for something it saw to settle
    /// (a new window whose Accessibility element is not there yet, an app
    /// still starting up) and wants another look soon, events or not.
    fn wants_another_look(&mut self) -> bool {
        false
    }
    /// Window events delivered since the last call (after `pump`): the only
    /// way cberry learns that something changed.
    fn take_events(&mut self) -> Vec<WindowEvent> {
        Vec::new()
    }
    /// Moves the mouse pointer.
    fn warp_cursor(&mut self, _x: f32, _y: f32) {}
    /// Whether a panel above the normal windows has the keyboard: Spotlight,
    /// a launcher, an app's floating panel. Focusing a window would close it.
    fn panel_has_focus(&mut self) -> bool {
        false
    }
    /// Whether an open menu or popover is at a point.
    fn covered_at(&mut self, _x: f32, _y: f32) -> bool {
        false
    }
    /// Sets the modifier key that turns a click anywhere on a window into a
    /// window gesture (`None` turns gestures off).
    fn set_mouse_modifier(&mut self, _modifier: Option<Modifier>) {}
    /// Whether a modifier key is held right now.
    fn modifier_held(&mut self, _modifier: Modifier) -> bool {
        false
    }
    /// `cberry ...` command lines from keyboard shortcuts since the last call.
    fn hotkey_commands(&mut self) -> Vec<String> {
        Vec::new()
    }
    /// Sets the keyboard shortcuts, one per line.
    fn set_hotkeys(&mut self, _text: &str) {}
    /// The button of the gesture in progress: pressed with the modifier held
    /// and not released yet. Those clicks never reach the app. A click too
    /// quick to be seen held is still reported once.
    fn mouse_gesture(&mut self) -> Option<MouseButton> {
        None
    }
}

/// A modifier key for mouse gestures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Modifier {
    Alt,
    Cmd,
    Ctrl,
    Shift,
    Fn,
}

impl Modifier {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "alt" | "option" | "opt" => Self::Alt,
            "cmd" | "command" => Self::Cmd,
            "ctrl" | "control" => Self::Ctrl,
            "shift" => Self::Shift,
            "fn" => Self::Fn,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Alt => "alt",
            Self::Cmd => "cmd",
            Self::Ctrl => "ctrl",
            Self::Shift => "shift",
            Self::Fn => "fn",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowEvent {
    /// The window was closed. Its frame may linger in the window list for a
    /// moment while macOS plays the close animation.
    Destroyed(WinId),
    /// Something that needs a fresh look at the window list: a window was
    /// opened, minimised or restored, an app launched, quit, hidden, shown
    /// or brought to the front, the Space or the displays changed.
    Changed,
    /// The window was moved or resized: by the user, by its app, or by cberry
    /// itself.
    Moved(WinId),
}

/// The ring around the focused window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Border {
    /// The window's frame; the ring goes outside it.
    pub rect: Rect,
    /// The window it goes around.
    pub window: WinId,
    /// RGBA, 0..=1.
    pub color: [f32; 4],
    pub width: i32,
    /// The roundness of the window's corners.
    pub radius: i32,
}

/// The translucent highlight shown over a drop target while a window is dragged.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Overlay {
    pub rect: Rect,
    /// The window the highlight sits on (it is stacked just above it).
    pub above: WinId,
    /// RGBA, 0..=1.
    pub color: [f32; 4],
}

/// Whether to move a window before resizing it when both change in one
/// animation frame. The window spends a moment between the two writes at
/// one of two in-between frames, and a slow app (Safari) shows it.
///
/// An edge that is moving anyway can lag a frame unnoticed; an edge that
/// should stand still must not draw back from where it is, or the window
/// shows a gap that flickers there every frame. So when one side of the
/// window is still and the other moves, the order keeps the still side
/// covered: shrinking from the left, move first (for a moment the window
/// reaches past its right edge); growing to the left, resize first.
///
/// Otherwise growing moves first and shrinking resizes first, so a window
/// never pushes against the screen edge on its way.
///
/// macOS lets a move take a window past the edge of the `display` but cuts
/// a resize short there (Safari asked for 688 points at x 838 on a
/// 1512-point screen gets 674, and keeps them once moved). So resizing
/// first is only done when the new size fits where the window still is.
pub fn position_first(frame: Rect, prev: Rect, display: Rect) -> bool {
    let grows = frame.w > prev.w || frame.h > prev.h;
    let resized_in_place = Rect::new(prev.x, prev.y, frame.w, frame.h);
    let fits = display.w == 0
        || (resized_in_place.max_x() <= display.max_x()
            && resized_in_place.max_y() <= display.max_y());
    // Per axis: Some(order) when only the near edge moves.
    let axis = |pos: i32, prev_pos: i32, len: i32, prev_len: i32| {
        (pos != prev_pos && pos + len == prev_pos + prev_len).then_some(len < prev_len)
    };
    let x = axis(frame.x, prev.x, frame.w, prev.w);
    let y = axis(frame.y, prev.y, frame.h, prev.h);
    let order = match (x, y) {
        (Some(a), Some(b)) if a != b => grows,
        (Some(order), _) | (None, Some(order)) => order,
        (None, None) => grows,
    };
    order || !fits
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Room to spare around every frame in these tests.
    const BIG: Rect = Rect {
        x: 0,
        y: 0,
        w: 4000,
        h: 4000,
    };

    #[test]
    fn a_still_right_edge_never_draws_back() {
        // The right window of two shrinks as the left one grows into it.
        let prev = Rect::new(600, 0, 400, 500);
        let shrunk = Rect::new(620, 0, 380, 500);
        assert!(position_first(shrunk, prev, BIG));
        // Moved first, it still reaches the old right edge.
        assert!(shrunk.x + prev.w >= prev.max_x());
        // The right window grows to the left.
        let grown = Rect::new(580, 0, 420, 500);
        assert!(!position_first(grown, prev, BIG));
        // Resized first, it still reaches the right edge.
        assert!(prev.x + grown.w >= grown.max_x());
    }

    #[test]
    fn a_still_bottom_edge_never_draws_back() {
        let prev = Rect::new(0, 300, 500, 300);
        assert!(position_first(Rect::new(0, 320, 500, 280), prev, BIG));
        assert!(!position_first(Rect::new(0, 280, 500, 320), prev, BIG));
    }

    #[test]
    fn without_a_still_far_edge_the_screen_edge_rule_holds() {
        let prev = Rect::new(100, 100, 400, 300);
        // Both edges move: growing moves first, shrinking resizes first.
        assert!(position_first(Rect::new(90, 100, 500, 300), prev, BIG));
        assert!(!position_first(Rect::new(110, 100, 300, 300), prev, BIG));
        // Only the far edge moves: a plain resize, either order is the same.
        assert!(position_first(Rect::new(100, 100, 450, 300), prev, BIG));
    }

    #[test]
    fn a_resize_that_the_screen_edge_would_cut_short_moves_first() {
        // Safari against the right edge of a 1512-point screen, growing to
        // the left by 22 points: resized in place it would need 1526.
        let screen = Rect::new(0, 0, 1512, 982);
        let prev = Rect::new(838, 42, 666, 932);
        let grown = Rect::new(816, 42, 688, 932);
        assert!(position_first(grown, prev, screen));
        // A step small enough to fit is still resized first.
        let nudged = Rect::new(832, 42, 672, 932);
        assert!(!position_first(nudged, prev, screen));
        // An unknown display does not hold anything back.
        assert!(!position_first(grown, prev, Rect::default()));
    }
}
