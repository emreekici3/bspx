//! The window system interface the manager drives.

use std::collections::{HashMap, HashSet};

use crate::bsp::WinId;
use crate::geometry::Rect;

#[cfg(any(target_os = "macos", bspx_check_macos))]
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
    /// A normal window bspx can move, though perhaps not resize (a
    /// fixed-size one such as Calculator). Every tileable window is movable.
    pub movable: bool,
}

pub trait Backend {
    /// All windows on screen: those on the active Space of the tiled display.
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
    /// Switches the tiled display to Space n (numbered from 1).
    fn switch_to_space(&mut self, _n: usize) -> Result<(), String> {
        Err("switching Spaces is not supported here".into())
    }
    /// Moves a window to another Space; returns whether it could. The user
    /// may be taken there along with it.
    fn move_to_space(&mut self, _id: WinId, _space: u64) -> bool {
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
    /// Moves and resizes a window.
    fn set_frame(&mut self, id: WinId, frame: Rect);
    /// Moves a window from `prev` to `frame`, touching only what changed: a
    /// pure move must not resize, which makes the app lay out its content
    /// again and is the costly part of every animation frame.
    fn update_frame(&mut self, id: WinId, frame: Rect, _prev: Rect) {
        self.set_frame(id, frame);
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
    /// Shows the drop-target highlight over `rect`, placed just above window
    /// `above`, or hides it with `None`.
    fn show_overlay(&mut self, _overlay: Option<Overlay>) {}
    /// Lets the platform UI (the overlay window) process its events. Called
    /// once per frame.
    fn pump(&mut self) {}
    /// Window events delivered since the last call (after `pump`), so the
    /// layout can react at once instead of at the next poll.
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
    /// `bspx ...` command lines from keyboard shortcuts since the last call.
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
    /// minimised or restored, or an app hidden or shown.
    Changed,
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
