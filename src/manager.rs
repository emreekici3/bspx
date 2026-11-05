//! Ties the BSP tree, the motion of windows and a window-system backend
//! together.

use std::collections::{HashMap, HashSet};

use log::{debug, info, warn};

use crate::bsp::{self, BspTree, WinId};
use crate::command::{Command, DisplayTarget, Zoom};
use crate::config::{Config, MouseAction, app_matches};
use crate::geometry::{Direction, Rect};
use crate::motion::Mover;
use crate::platform::{
    Backend, Border, Display, Modifier, MouseButton, Overlay, WindowEvent, WindowInfo,
};

/// The layout of a Space (virtual desktop) other than the current one: on
/// another display, or not on screen.
#[derive(Debug, Default)]
struct SpaceState {
    tree: BspTree,
    focused: Option<WinId>,
    zoomed: Option<(WinId, Zoom)>,
}

pub struct Manager<B: Backend> {
    backend: B,
    config: Config,
    /// Layout of the current Space: the one on the display with the focused
    /// window, which commands act on. `spaces` holds the others, those
    /// other displays show included.
    tree: BspTree,
    active_space: u64,
    /// The displays tiled, the main one first, from the last look.
    displays: Vec<Display>,
    spaces: HashMap<u64, SpaceState>,
    mover: Mover,
    /// Last frame each window was seen at or sent to.
    frames: HashMap<WinId, Rect>,
    apps: HashMap<WinId, String>,
    /// Windows the user took out of the layout.
    floating: HashSet<WinId>,
    /// Where each floating window `cberry zoom` grew was before.
    float_zooms: HashMap<WinId, Rect>,
    /// Windows reported closed that may still show up in the window list
    /// during their close animation; they must not be tiled again. Some
    /// apps (Electron ones like Claude or Slack) only hide a window closed
    /// with the red button and show the same window again when reopened,
    /// so a window is forgotten here as soon as it leaves the screen.
    closed: HashSet<WinId>,
    /// Last focused window that is part of the layout; new windows split it.
    focused: Option<WinId>,
    /// The smallest size each window was seen to accept: it was sent a
    /// smaller frame and stayed larger. Only this window's own refusals
    /// count, and they are forgotten when it closes. The layout ignores
    /// them; they only stop a command (a resize, a warp, a swap) from
    /// putting the window over another.
    min_sizes: MinSizes,
    /// The layout before the user's latest run of resizes, and when the
    /// last of them was. A window seen to refuse its size right after one
    /// stops that resize where it fits: the first time cberry learns a
    /// window's minimum is by going past it.
    resize_anchor: Option<ResizeRun>,
    /// The ring on screen, if any.
    shown_border: Option<Border>,
    /// The ring is following a window the user holds by its title bar.
    border_tracking: bool,
    /// That window at the press: its frame by the window server, and the
    /// one the ring knew.
    border_press: Option<(WinId, Rect, Rect)>,
    /// The drop highlight on screen, if any.
    shown_overlay: Option<Overlay>,
    /// The Space shown before the current one, for `space recent`.
    previous_space: Option<u64>,
    /// What the system last said had focus; see `sync_focus`.
    reported_focus: Option<WinId>,
    /// The window focused before the system's focus last moved, and when
    /// (`clock`): see `split_target`.
    focus_moved: Option<(WinId, f64)>,
    /// The window last sent to another Space, the Space, and when: going
    /// after it right away is meant to find it in focus.
    sent: Option<(WinId, u64, std::time::Instant)>,
    /// A window that must end up focused, until when, and when to try next:
    /// macOS hands focus around for a moment after a Space switch.
    holding_focus: Option<(WinId, std::time::Instant, std::time::Instant)>,
    /// The window `cberry zoom` grew over the others on this Space, if any.
    zoomed: Option<(WinId, Zoom)>,
    area: Rect,
    /// Left mouse button state at the previous frame.
    mouse_was_down: bool,
    /// What the user is doing with the mouse, while the button is held.
    drag: Drag,
    /// Window frames at the last poll before the mouse button went down,
    /// so a drag can be told apart from windows that never match their
    /// slot exactly (apps that round their size to a character grid).
    before_drag: HashMap<WinId, Rect>,
    /// Windows on screen, front to back, from the last poll.
    stack: Vec<WinId>,
    /// Those of them that are real document windows cberry can manage (the
    /// rest: dialogs, and the invisible helper windows many apps and tools
    /// keep on screen).
    manageable: HashSet<WinId>,
    /// Those of them cberry can move, though perhaps not resize.
    movable: HashSet<WinId>,
    /// The modifier last given to the backend.
    mouse_modifier: Option<Modifier>,
    /// The gesture button at the previous frame.
    gesture_was: Option<MouseButton>,
    /// Seconds since start, advanced by `tick`.
    clock: f64,
    /// Focus follows mouse: the pointer at the last frame, the window under
    /// it, and the window it entered and since when, until focused.
    last_cursor: Option<(f32, f32)>,
    under: Option<WinId>,
    hover: Option<(WinId, f64)>,
    /// Windows seen before (all that existed at the first poll, then every
    /// one that came on screen), so Space rules only move new windows.
    seen: HashSet<WinId>,
    synced: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Drag {
    None,
    /// The button went down over a tiled window; it may be moved or resized.
    Pressed(WinId),
    /// The window is being moved by its title bar. `target` is the window
    /// under the pointer, which it would swap with if dropped now, or split
    /// on `side` (a window on another display).
    Moving {
        win: WinId,
        target: Option<WinId>,
        side: Option<Direction>,
    },
    /// Something else (a resize, a click, a drag outside tiled windows).
    Other,
    /// A window moved or resized with the modifier held, grabbed anywhere.
    Gesture(Grab),
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Grab {
    win: WinId,
    button: MouseButton,
    action: MouseAction,
    /// Pointer and window frame when the button went down.
    start: (f32, f32),
    frame: Rect,
    /// Part of the layout (rather than a floating window).
    tiled: bool,
    /// For a resize, the edges that follow the pointer: the ones nearest to
    /// where the window was grabbed.
    horizontal: Direction,
    vertical: Direction,
    /// For a tiled window being moved, the window it would swap with.
    target: Option<WinId>,
    /// With the insert modifier held, the side of `target` it would go to
    /// instead, splitting that window.
    side: Option<Direction>,
    /// The pointer has gone further than a click's wobble.
    moved: bool,
}

/// How long the pointer must rest on a window before focus follows it, in
/// seconds, so crossing windows on the way somewhere does not focus them.
const FOCUS_FOLLOWS_MOUSE_DELAY: f64 = 0.1;

/// How soon after a window is sent to a Space going there brings it focus.
const FOLLOW_SENT: std::time::Duration = std::time::Duration::from_secs(2);
/// How long after that the window is kept in focus, and how often it is looked at.
const HOLD_FOCUS: std::time::Duration = std::time::Duration::from_millis(500);
const HOLD_FOCUS_RETRY: std::time::Duration = std::time::Duration::from_millis(30);

/// The smallest a window can be resized to by a mouse gesture.
const MIN_GESTURE_SIZE: i32 = 50;

/// Frame differences below this many points are noise (e.g. apps rounding
/// their size to a character grid), not a user's drag.
const DRAG_THRESHOLD: i32 = 4;

/// A window this many points larger than its slot is taken to have hit its
/// minimum size. Large enough that an app rounding its size up to whole
/// text cells is not mistaken for one.
const MIN_SIZE_THRESHOLD: i32 = 24;

/// The smallest width and height each window was seen to accept.
type MinSizes = HashMap<WinId, (i32, i32)>;

/// The user's latest run of resizes: the layout before the first, when the
/// last was, and, when they were all key presses for one window and one
/// direction, which and how far in all.
#[derive(Clone)]
struct ResizeRun {
    before: BspTree,
    at: f64,
    keys: Option<(WinId, Direction, f32)>,
}

/// Resizes this close together are one run (a key held down); a window
/// refusing its size this soon after the last one stops the run.
const RESIZE_RUN_SECS: f64 = 1.0;

impl<B: Backend> Manager<B> {
    pub fn new(backend: B, config: Config) -> Self {
        Self {
            mover: Mover::new(config.animation_duration),
            backend,
            config,
            tree: BspTree::new(),
            active_space: 0,
            displays: Vec::new(),
            spaces: HashMap::new(),
            frames: HashMap::new(),
            apps: HashMap::new(),
            floating: HashSet::new(),
            float_zooms: HashMap::new(),
            closed: HashSet::new(),
            focused: None,
            min_sizes: MinSizes::new(),
            resize_anchor: None,
            shown_border: None,
            border_tracking: false,
            border_press: None,
            shown_overlay: None,
            previous_space: None,
            reported_focus: None,
            focus_moved: None,
            sent: None,
            holding_focus: None,
            zoomed: None,
            area: Rect::default(),
            mouse_was_down: false,
            drag: Drag::None,
            before_drag: HashMap::new(),
            stack: Vec::new(),
            manageable: HashSet::new(),
            movable: HashSet::new(),
            mouse_modifier: None,
            gesture_was: None,
            clock: 0.0,
            last_cursor: None,
            under: None,
            hover: None,
            seen: HashSet::new(),
            synced: false,
        }
    }

    pub fn backend(&self) -> &B {
        &self.backend
    }

    /// `cberry ...` command lines from keyboard shortcuts.
    pub fn hotkey_commands(&mut self) -> Vec<String> {
        self.backend.hotkey_commands()
    }

    /// Whether the backend wants another look soon without an event: see
    /// [`Backend::wants_another_look`].
    pub fn wants_another_look(&mut self) -> bool {
        self.backend.wants_another_look()
    }

    pub fn tree(&self) -> &BspTree {
        &self.tree
    }

    /// The layout of a Space that is not on screen, if it has one.
    pub fn space_tree(&self, space: u64) -> Option<&BspTree> {
        if space == self.active_space {
            Some(&self.tree)
        } else {
            self.spaces.get(&space).map(|s| &s.tree)
        }
    }

    pub fn active_space(&self) -> u64 {
        self.active_space
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// How long an animation frame lasts: one refresh of the display (120
    /// frames a second on ProMotion, 60 elsewhere; 120 if unknown).
    pub fn frame_interval(&mut self) -> std::time::Duration {
        let rate = self
            .backend
            .frame_rate()
            .unwrap_or(120.0)
            .clamp(30.0, 240.0);
        std::time::Duration::from_secs_f64(1.0 / rate)
    }

    pub fn is_animating(&self) -> bool {
        !self.mover.at_rest()
    }

    /// Whether something moves by the user's hand: frames must not wait.
    pub fn is_held(&self) -> bool {
        self.border_tracking
    }

    /// Waits for the next frame of a motion. `remaining` is what is left of
    /// this one by the clock.
    pub fn wait_frame(&mut self, remaining: std::time::Duration) {
        let refresh = self.frame_interval();
        self.backend.wait_frame(remaining, refresh);
    }

    /// Nothing is moving any more.
    pub fn stop_frames(&mut self) {
        self.backend.stop_frames();
    }

    /// Applies new settings. Windows of apps that became floating leave the
    /// layout right away; those that may be tiled again join at the next poll.
    pub fn set_config(&mut self, config: Config) {
        if let Some(level) = config.log_level {
            log::set_max_level(level);
        }
        self.mover.set_duration(config.animation_duration);
        if config.hotkeys != self.config.hotkeys {
            self.backend.set_hotkeys(&config.hotkeys.join("\n"));
        }
        self.config = config;
        let floats = |apps: &HashMap<WinId, String>, config: &Config, id: &WinId| {
            let app = apps.get(id).map_or("", String::as_str);
            config.float_apps.iter().any(|a| app_matches(a, app))
        };
        for id in self.tree.windows() {
            if floats(&self.apps, &self.config, &id) {
                self.tree.remove(id);
                self.mover.halt(id);
                if self.focused == Some(id) {
                    self.focused = self.tree.windows().last().copied();
                }
            }
        }
        for space in self.spaces.values_mut() {
            for id in space.tree.windows() {
                if floats(&self.apps, &self.config, &id) {
                    space.tree.remove(id);
                    if space.focused == Some(id) {
                        space.focused = space.tree.windows().last().copied();
                    }
                }
            }
        }
        self.relayout();
    }

    /// Syncs with the window system: picks up opened and closed windows,
    /// Space switches and display changes. Call this periodically.
    pub fn refresh(&mut self) {
        self.sync(true);
    }

    /// `refresh`, optionally without learning minimum sizes (the frames
    /// right after a mouse resize are the user's, not the app's limits).
    fn sync(&mut self, learn: bool) {
        let mut dirty = false;

        let mut displays = self.backend.displays();
        if displays.is_empty() {
            let area = self.backend.tiling_area();
            let space = self.backend.active_space();
            displays.push(Display {
                frame: area,
                area,
                space,
            });
        }
        if displays != self.displays {
            debug!("displays: {displays:?}");
            // Spaces no longer on screen: their windows stop moving.
            for d in &self.displays {
                if !displays.iter().any(|n| n.space == d.space)
                    && let Some(state) = self.spaces.get(&d.space)
                {
                    for id in state.tree.windows() {
                        self.mover.halt(id);
                    }
                }
            }
            self.displays = displays;
            dirty = true;
        }
        let shown = self.shown_spaces();

        // The current display: the one with the focused window, else the
        // one that was current while it is still there, else the main one.
        let focused_now = self.backend.focused_window();
        let space = focused_now
            .and_then(|f| self.tiled_space_of(f))
            .filter(|s| shown.contains(s))
            .or_else(|| {
                shown
                    .contains(&self.active_space)
                    .then_some(self.active_space)
            })
            .unwrap_or(shown[0]);
        if space != self.active_space {
            if shown.contains(&self.active_space) {
                self.make_current(space);
            } else {
                self.switch_space(space);
                self.land_on_space(focused_now);
            }
            dirty = true;
        }
        let area = self.display_area(space);
        if area != self.area {
            self.area = area;
            dirty = true;
        }

        self.note_focus(focused_now);

        let windows = self.backend.windows();
        let on_screen: HashSet<WinId> = windows.iter().map(|w| w.id).collect();
        let alive = self
            .backend
            .alive_windows()
            .unwrap_or_else(|| on_screen.clone());

        // Ask where every window we know of really is, rather than trusting
        // what is on screen: right after a Space switch the screen still
        // shows the old Space's windows for a moment.
        let mut ids: Vec<WinId> = self.all_tiled().into_iter().map(|(_, id)| id).collect();
        let extra: Vec<WinId> = on_screen
            .iter()
            .copied()
            .filter(|id| !ids.contains(id))
            .collect();
        ids.extend(extra);
        let spaces = self.backend.window_spaces(&ids);
        // Right after a Space switch the screen still shows windows cberry
        // tiles on another Space. Only those count: a window on every Space
        // is always on screen and says nothing.
        let switching = self.all_tiled().iter().any(|(_, id)| {
            on_screen.contains(id) && spaces.get(id).is_some_and(|s| !shown.contains(s))
        }) || (!self.tree.is_empty()
            && !self.tree.windows().iter().any(|w| on_screen.contains(w)));
        let frames_now: HashMap<WinId, Rect> = windows.iter().map(|w| (w.id, w.frame)).collect();
        let hands_off = self.drag == Drag::None && !self.mouse_was_down;

        // Floating Spaces have no layout; their windows stay where they are.
        let float_spaces = self.float_space_ids();
        if float_spaces.contains(&space) && !self.tree.is_empty() {
            debug!("Space {space} floats: leaving its windows be");
            for id in self.tree.windows() {
                self.mover.halt(id);
            }
            self.tree = BspTree::new();
            self.zoomed = None;
            self.focused = None;
        }
        self.spaces.retain(|s, _| !float_spaces.contains(s));

        let mut moved = Vec::new();
        for (tree_space, id) in self.all_tiled() {
            let leave = if !alive.contains(&id) {
                Some("closed")
            } else {
                let on_display = shown.contains(&tree_space);
                match spaces.get(&id) {
                    None => Some("minimised or hidden"),
                    // Between two displays on screen, the layout decides
                    // where a window goes (macOS says it is on the other
                    // display's Space while it slides there); only one the
                    // user put there and left there has moved.
                    Some(&s)
                        if s != tree_space
                            && on_display
                            && shown.contains(&s)
                            && !(hands_off
                                && !self.mover.in_motion(id)
                                && frames_now.get(&id).is_some_and(|f| {
                                    self.display_of_space(s).is_some_and(|d| {
                                        let (x, y) = f.center();
                                        d.frame.contains_point(x, y)
                                    })
                                })) =>
                    {
                        None
                    }
                    Some(&s) if s != tree_space => {
                        moved.push((s, id));
                        Some("moved to another Space")
                    }
                    Some(_) if on_display && !on_screen.contains(&id) && !switching => {
                        Some("minimised or hidden")
                    }
                    Some(_) if on_display && !on_screen.contains(&id) => {
                        debug!("window {id} is off screen; kept while a Space switch settles");
                        None
                    }
                    Some(_) => None,
                }
            };
            if let Some(reason) = leave {
                debug!("window {id} leaves the layout of Space {tree_space}: {reason}");
                self.remove_from_space(tree_space, id);
                dirty |= shown.contains(&tree_space);
            }
        }
        // A window moved to another Space joins that Space's layout. One
        // moved to a Space on screen is picked up below.
        for (s, id) in moved {
            if !shown.contains(&s) && !float_spaces.contains(&s) {
                let (inner, gap, ratio) =
                    (self.inner_area(), self.config.gap, self.config.split_ratio);
                let state = self.spaces.entry(s).or_default();
                state.tree.insert(id, state.focused, inner, gap, ratio);
            }
        }

        self.floating.retain(|w| alive.contains(w));
        self.float_zooms.retain(|w, _| alive.contains(w));
        if !self.synced {
            self.synced = true;
            self.seen.extend(alive.iter().copied());
        }
        self.seen.retain(|w| alive.contains(w));
        self.closed.retain(|w| on_screen.contains(w));
        self.frames.retain(|w, _| on_screen.contains(w));
        self.apps.retain(|w, _| alive.contains(w));
        self.min_sizes.retain(|w, _| alive.contains(w));
        if self.focused.is_some_and(|f| !self.tree.contains(f)) {
            self.focused = None;
        }

        // Where the windows were sent, to see which ones would not go.
        let mut learned = false;
        let learn = learn && self.drag == Drag::None && !self.mouse_was_down;
        let expected: HashMap<WinId, Rect> = if learn {
            self.targets().into_iter().collect()
        } else {
            HashMap::new()
        };

        // Open windows in the order the backend lists them, but put the
        // focused one last so it ends up next to where the user is looking.
        self.stack = windows.iter().map(|w| w.id).collect();
        self.manageable = windows
            .iter()
            .filter(|w| w.tileable)
            .map(|w| w.id)
            .collect();
        self.movable = windows.iter().filter(|w| w.movable).map(|w| w.id).collect();
        let mut new = Vec::new();
        for info in windows {
            if !self.mover.in_motion(info.id) {
                self.frames.insert(info.id, info.frame);
                if let Some(&slot) = expected.get(&info.id) {
                    learned |= self.learn_min_size(info.id, slot, info.frame);
                }
            }
            self.apps.insert(info.id, info.app.clone());
            // A window counts as seen once cberry can manage it: a new one is
            // often listed a poll or two before its Accessibility element
            // turns up, and the rule must still apply then.
            if info.movable && !self.seen.contains(&info.id) {
                // A grid rule floats the window at its cell (a fixed-size
                // one too); a Space rule then may still send it away (it
                // keeps its place there).
                let placed = self.place_by_grid(&info);
                if placed || info.tileable {
                    self.seen.insert(info.id);
                }
                if (info.tileable && self.send_by_rule(info.id, &info.app)) || placed {
                    continue;
                }
            }
            let on = spaces.get(&info.id).copied();
            let wanted = info.tileable
                && on.is_some_and(|s| shown.contains(&s) && !float_spaces.contains(&s))
                && !self.floating.contains(&info.id)
                && !self.closed.contains(&info.id)
                && !self
                    .config
                    .float_apps
                    .iter()
                    .any(|a| app_matches(a, &info.app));
            if let Some(s) = on.filter(|_| wanted && !self.is_tiled_anywhere(info.id)) {
                new.push((info.id, s));
            }
        }
        new.sort_by_key(|(w, _)| Some(*w) == focused_now);
        for (id, s) in new {
            if s == self.active_space {
                let target = self.split_target(id);
                self.tile(id, target);
                if focused_now == Some(id) || self.focused.is_none() {
                    self.focused = Some(id);
                }
            } else {
                // On another display: next to the window last focused there.
                let (inner, gap, ratio) = (
                    self.inner_area_of(s),
                    self.config.gap,
                    self.config.split_ratio,
                );
                let state = self.spaces.entry(s).or_default();
                let target = state
                    .focused
                    .filter(|t| state.tree.contains(*t))
                    .or_else(|| state.tree.windows().last().copied());
                debug!("tiling window {id} next to {target:?} on another display");
                state.tree.insert(id, target, inner, gap, ratio);
                state.focused = Some(id);
                if focused_now == Some(id) {
                    self.make_current(s);
                    self.focused = Some(id);
                }
            }
            dirty = true;
        }

        // A window refused its size just after the user's resize: the resize
        // went past it. It stops where the window fits.
        if learned
            && let Some(run) = self.resize_anchor.clone()
            && self.clock - run.at <= RESIZE_RUN_SECS + 1.0
        {
            let now = self.tree.clone();
            let (area, gap) = (self.inner_area(), self.config.gap);
            let (tree, _) = match run.keys {
                // The same keys, pressed less far.
                Some((win, dir, total)) => self.limited_by(&run.before, &now, |t| {
                    let mut tree = run.before.clone();
                    tree.resize(win, dir, total * t, area, gap).then_some(tree)
                }),
                None => self.limited(&run.before, now),
            };
            if tree != self.tree {
                debug!("the last resize went past what a window accepts; it stops there");
                self.tree = tree;
                dirty = true;
            }
        }

        if dirty {
            self.relayout();
        }
    }

    /// Notes that the user is resizing: the layout before the first resize
    /// of a run is where a resize that turns out to go too far is measured
    /// from.
    /// `keys`: a key press resizing that window that way that much; `None`
    /// for the mouse.
    fn note_resize(&mut self, before: &BspTree, keys: Option<(WinId, Direction, f32)>) {
        let run = match self.resize_anchor.take() {
            Some(run) if self.clock - run.at <= RESIZE_RUN_SECS => {
                let keys = match (run.keys, keys) {
                    (Some((w, d, total)), Some((w2, d2, more))) if w == w2 && d == d2 => {
                        Some((w, d, total + more))
                    }
                    _ => None,
                };
                ResizeRun {
                    at: self.clock,
                    keys,
                    ..run
                }
            }
            _ => ResizeRun {
                before: before.clone(),
                at: self.clock,
                keys,
            },
        };
        self.resize_anchor = Some(run);
    }

    /// Tiles `id` by splitting `target` (or the last window), along the
    /// longer side of `target`'s slot, at the configured ratio.
    fn tile(&mut self, id: WinId, target: Option<WinId>) {
        let (inner, gap, ratio) = (self.inner_area(), self.config.gap, self.config.split_ratio);
        let target = target
            .filter(|t| self.tree.contains(*t))
            .or_else(|| self.tree.windows().last().copied());
        debug!("tiling window {id} next to {target:?}");
        self.tree.insert(id, target, inner, gap, ratio);
    }

    /// The ids of the Spaces set to float. Space numbers count on each
    /// display, so Space 2 set to float is the second of every display.
    fn float_space_ids(&mut self) -> HashSet<u64> {
        if self.config.float_spaces.is_empty() {
            return HashSet::new();
        }
        let numbers: Vec<usize> = self
            .config
            .float_spaces
            .iter()
            .filter_map(|s| self.config.space_number(s))
            .collect();
        let mut out = HashSet::new();
        for shown in self.shown_spaces() {
            let spaces = self.backend.display_spaces(shown);
            out.extend(numbers.iter().filter_map(|n| spaces.get(n - 1).copied()));
        }
        out
    }

    /// The Space number and id `space` (a number or a label) stands for, on
    /// the current display: Spaces are numbered from 1 on each display.
    fn resolve_space(&mut self, space: &str) -> Result<(usize, u64), String> {
        let current = self.current_space();
        if self.config.space_number(space).is_none() {
            let spaces = self.backend.display_spaces(current);
            let here = spaces.iter().position(|s| *s == current);
            let at = match (space, here) {
                ("next", Some(i)) => Some((i + 1) % spaces.len()),
                ("prev", Some(i)) => Some((i + spaces.len() - 1) % spaces.len()),
                ("recent", _) => {
                    let last = self
                        .previous_space
                        .ok_or("no Space was shown before this one")?;
                    Some(
                        spaces
                            .iter()
                            .position(|s| *s == last)
                            .ok_or("the last Space is not on this display")?,
                    )
                }
                _ => None,
            };
            if let Some(i) = at {
                return Ok((i + 1, spaces[i]));
            }
        }
        let n = self
            .config
            .space_number(space)
            .ok_or_else(|| format!("no Space is labelled '{space}'"))?;
        let id = *self
            .backend
            .display_spaces(current)
            .get(n - 1)
            .ok_or_else(|| format!("there is no Space {n} on this display"))?;
        Ok((n, id))
    }

    /// Floats a newly opened window at the grid cell its app's rule names.
    /// One that cannot be resized keeps its size and is put against the
    /// screen edges the cell touches (centred in it along the others).
    /// Returns whether it was placed.
    fn place_by_grid(&mut self, info: &WindowInfo) -> bool {
        let (id, app) = (info.id, info.app.as_str());
        let Some(grid) = self
            .config
            .grid_rules
            .iter()
            .find(|(a, _)| app_matches(a, app))
            .map(|(_, g)| *g)
        else {
            return false;
        };
        let area = self.inner_area();
        let cell = area.grid_cell(grid);
        let frame = if info.tileable {
            cell
        } else {
            area.align_in(cell, info.frame.w, info.frame.h)
        };
        self.floating.insert(id);
        self.mover.halt(id);
        self.backend.update_frame(id, frame, info.frame);
        self.frames.insert(id, frame);
        info!("window {id} of {app} floats at grid {grid:?}: {frame:?}");
        true
    }

    /// Sends a newly opened window to the Space its app's rule names.
    /// Returns whether it went.
    fn send_by_rule(&mut self, id: WinId, app: &str) -> bool {
        let Some(space) = self
            .config
            .space_rules
            .iter()
            .find(|(a, _)| app_matches(a, app))
            .map(|(_, s)| s.clone())
        else {
            return false;
        };
        let (n, sid) = match self.resolve_space(&space) {
            Ok(target) => target,
            Err(e) => {
                warn!("rule space {space} \"{app}\": {e}");
                return false;
            }
        };
        if sid == self.active_space {
            return false;
        }
        let moved = if self.config.rule_carry {
            self.backend.move_to_space(id, sid)
        } else {
            self.backend.place_on_space(id, sid)
        };
        if moved {
            info!("window {id} of {app} sent to Space {n}");
            true
        } else {
            warn!("could not send window {id} of {app} to Space {n}");
            false
        }
    }

    /// Compares where a settled window is with the slot it was sent to. One
    /// that stayed larger has hit its minimum size; one that has since gone
    /// below what was learned can shrink further after all.
    fn learn_min_size(&mut self, id: WinId, slot: Rect, actual: Rect) -> bool {
        let known = self.min_sizes.get(&id).copied().unwrap_or((0, 0));
        let learn = |slot: i32, actual: i32, known: i32| {
            let refused = actual > slot + MIN_SIZE_THRESHOLD && actual > known;
            let shrank_below = known > 0 && actual < known - DRAG_THRESHOLD;
            if refused || shrank_below {
                actual
            } else {
                known
            }
        };
        let min = (
            learn(slot.w, actual.w, known.0),
            learn(slot.h, actual.h, known.1),
        );
        if min == known {
            return false;
        }
        info!("window {id} accepts no less than {}x{}", min.0, min.1);
        self.min_sizes.insert(id, min);
        true
    }

    /// The first window `after` squeezes below the size it was seen to
    /// accept, where `before` did not squeeze it as far: a change the user
    /// asks for must not put that window over another.
    fn squeezed(&self, before: &BspTree, after: &BspTree) -> Option<WinId> {
        if self.min_sizes.is_empty() {
            return None;
        }
        let (area, gap) = (self.inner_area(), self.config.gap);
        let old: HashMap<WinId, Rect> = before.layout(area, gap).into_iter().collect();
        after.layout(area, gap).into_iter().find_map(|(id, r)| {
            let &(min_w, min_h) = self.min_sizes.get(&id)?;
            let was = old.get(&id);
            let narrower = r.w < min_w && was.is_none_or(|o| r.w < o.w);
            let shorter = r.h < min_h && was.is_none_or(|o| r.h < o.h);
            (narrower || shorter).then_some(id)
        })
    }

    /// `after`, a change of `before`'s ratios, cut short where it would
    /// squeeze a window below the size it was seen to accept: as far toward
    /// `after` as it can go. Returns the tree and the window that stopped
    /// it, if one did.
    fn limited(&self, before: &BspTree, after: BspTree) -> (BspTree, Option<WinId>) {
        let step = |t: f32| before.interpolate(&after, t);
        self.limited_by(before, &after, step)
    }

    /// Like `limited`, with `step(t)` the change made `t` of the way (0 to
    /// 1): the furthest step that squeezes no window below what it accepts.
    fn limited_by(
        &self,
        before: &BspTree,
        after: &BspTree,
        step: impl Fn(f32) -> Option<BspTree>,
    ) -> (BspTree, Option<WinId>) {
        let Some(stopper) = self.squeezed(before, after) else {
            return (after.clone(), None);
        };
        let (mut fits, mut not) = (0.0_f32, 1.0_f32);
        for _ in 0..24 {
            let t = (fits + not) / 2.0;
            match step(t) {
                Some(tree) if self.squeezed(before, &tree).is_none() => fits = t,
                Some(_) => not = t,
                // Not a change that can stop part-way: leave it be.
                None => return (after.clone(), Some(stopper)),
            }
        }
        let tree = step(fits).unwrap_or_else(|| before.clone());
        (tree, Some(stopper))
    }

    /// Reacts to window events, the only way cberry learns that something
    /// changed. A closed window leaves the layout at once, so its neighbours
    /// fill the gap (animated, or instantly with `animation 0`). Returns
    /// whether the window list should be read again now.
    pub fn handle_events(&mut self) -> bool {
        let mut refresh = false;
        let mut dirty = false;
        for event in self.backend.take_events() {
            match event {
                WindowEvent::Destroyed(id) => {
                    self.closed.insert(id);
                    self.floating.remove(&id);
                    if let Some((space, _)) = self.all_tiled().into_iter().find(|(_, w)| *w == id) {
                        debug!("window {id} closed");
                        self.remove_from_space(space, id);
                        dirty |= self.shown_spaces().contains(&space);
                        if self.focused == Some(id) {
                            self.focused = self.tree.windows().last().copied();
                        }
                    }
                    if self.drag != Drag::None {
                        self.drag = Drag::Other;
                        self.set_overlay(None);
                    }
                }
                WindowEvent::Changed => {
                    debug!("event: something changed");
                    refresh = true;
                }
                // While cberry animates, the moves are mostly its own: one
                // look once the animation ends covers them. While the mouse
                // is down, the user is dragging: the release takes a look.
                WindowEvent::Moved(_) => {
                    refresh |= self.mover.at_rest() && !self.backend.mouse_down();
                }
            }
        }
        if dirty {
            self.relayout();
        }
        refresh
    }

    /// A Space just came on screen: its last window gets the keyboard back,
    /// if the focus was left behind on a Space no longer shown.
    fn land_on_space(&mut self, focused_now: Option<WinId>) {
        let left_behind = focused_now.is_none_or(|f| {
            self.tiled_space_of(f)
                .is_some_and(|s| s != self.active_space)
        });
        if let Some(win) = self.focused.filter(|_| left_behind) {
            self.backend.focus(win);
        }
    }

    /// Puts the current layout away and brings out the one of `space`.
    fn switch_space(&mut self, space: u64) {
        debug!("Space {} -> {space}", self.active_space);
        self.previous_space = Some(self.active_space);
        for id in self.tree.windows() {
            self.mover.halt(id);
        }
        if std::mem::replace(&mut self.drag, Drag::None) != Drag::None {
            self.set_overlay(None);
        }
        let old = SpaceState {
            tree: std::mem::take(&mut self.tree),
            focused: self.focused.take(),
            zoomed: self.zoomed.take(),
        };
        if !old.tree.is_empty() {
            self.spaces.insert(self.active_space, old);
        }
        if let Some(state) = self.spaces.remove(&space) {
            self.tree = state.tree;
            self.focused = state.focused;
            self.zoomed = state.zoomed;
        }
        self.active_space = space;
    }

    /// Makes the display showing `space` the current one, the one commands
    /// act on. Both stay on screen, so nothing stops moving.
    fn make_current(&mut self, space: u64) {
        if space == self.active_space {
            return;
        }
        debug!("display of Space {space} is now the current one");
        let old = SpaceState {
            tree: std::mem::take(&mut self.tree),
            focused: self.focused.take(),
            zoomed: self.zoomed.take(),
        };
        if !old.tree.is_empty() {
            self.spaces.insert(self.active_space, old);
        }
        if let Some(state) = self.spaces.remove(&space) {
            self.tree = state.tree;
            self.focused = state.focused;
            self.zoomed = state.zoomed;
        }
        self.active_space = space;
        self.area = self.display_area(space);
    }

    /// The Space of the current display: the one with the focused window
    /// (before cberry has first looked, the main display's).
    fn current_space(&mut self) -> u64 {
        if self.displays.is_empty() {
            self.backend.active_space()
        } else {
            self.active_space
        }
    }

    /// The Spaces the displays show, the main display's first. Never empty
    /// once cberry has looked: before that, the current Space.
    fn shown_spaces(&self) -> Vec<u64> {
        if self.displays.is_empty() {
            return vec![self.active_space];
        }
        self.displays.iter().map(|d| d.space).collect()
    }

    fn display_of_space(&self, space: u64) -> Option<&Display> {
        self.displays.iter().find(|d| d.space == space)
    }

    /// The tiling area of the display showing `space`; the current one's
    /// for a Space not on screen (where it will be shown is not known).
    fn display_area(&self, space: u64) -> Rect {
        self.display_of_space(space)
            .or_else(|| self.display_of_space(self.active_space))
            .map_or(self.area, |d| d.area)
    }

    fn inner_area_of(&self, space: u64) -> Rect {
        self.display_area(space).inset(&self.config.padding)
    }

    /// The Space whose layout window `id` is in.
    fn tiled_space_of(&self, id: WinId) -> Option<u64> {
        if self.tree.contains(id) {
            return Some(self.active_space);
        }
        self.spaces
            .iter()
            .find(|(_, s)| s.tree.contains(id))
            .map(|(&space, _)| space)
    }

    /// The layout of `space`, if it has one.
    fn tree_of_mut(&mut self, space: u64) -> Option<&mut BspTree> {
        if space == self.active_space {
            Some(&mut self.tree)
        } else {
            self.spaces.get_mut(&space).map(|s| &mut s.tree)
        }
    }

    /// Swaps two tiled windows, on one display or across two. The display
    /// of `a` stays current, as `b`'s place now has `a`.
    fn exchange(&mut self, a: WinId, b: WinId) -> bool {
        let (Some(sa), Some(sb)) = (self.tiled_space_of(a), self.tiled_space_of(b)) else {
            return false;
        };
        if self.zoomed.is_some_and(|(w, _)| w == a || w == b) {
            self.zoomed = None;
        }
        if sa == sb {
            return self.tree_of_mut(sa).is_some_and(|t| t.swap(a, b));
        }
        for (space, old, new) in [(sa, a, b), (sb, b, a)] {
            if let Some(tree) = self.tree_of_mut(space) {
                tree.replace(old, new);
            }
            if let Some(state) = self.spaces.get_mut(&space) {
                if state.focused == Some(old) {
                    state.focused = Some(new);
                }
                if state.zoomed.is_some_and(|(w, _)| w == old) {
                    state.zoomed = None;
                }
            }
        }
        self.make_current(sb);
        self.focused = Some(a);
        info!("window {a} swapped with window {b} across displays");
        true
    }

    /// Moves tiled window `win` into the layout of the display showing
    /// `space`, next to `next_to` (on `side` of it, if given) or else to the
    /// window last focused there. That display becomes the current one.
    fn move_to_display(
        &mut self,
        win: WinId,
        space: u64,
        next_to: Option<WinId>,
        side: Option<Direction>,
    ) -> bool {
        let Some(from) = self.tiled_space_of(win) else {
            return false;
        };
        if from == space {
            return false;
        }
        if self.zoomed.is_some_and(|(w, _)| w == win) {
            self.zoomed = None;
        }
        self.remove_from_space(from, win);
        if self.focused == Some(win) {
            self.focused = self.tree.windows().last().copied();
        }
        self.make_current(space);
        let (inner, gap, ratio) = (self.inner_area(), self.config.gap, self.config.split_ratio);
        let target = next_to
            .filter(|t| self.tree.contains(*t))
            .or(self.focused.filter(|t| self.tree.contains(*t)))
            .or_else(|| self.tree.windows().last().copied());
        match (target, side) {
            (Some(t), Some(side)) => {
                self.tree.insert_at(win, t, side);
            }
            _ => {
                self.tree.insert(win, target, inner, gap, ratio);
            }
        }
        self.focused = Some(win);
        info!("window {win} moved to another display");
        true
    }

    /// The Space of the display next to the current one in direction `dir`.
    fn display_toward(&self, dir: Direction) -> Option<u64> {
        let frames: Vec<(WinId, Rect)> = self
            .displays
            .iter()
            .enumerate()
            .map(|(i, d)| (i as WinId, d.frame))
            .collect();
        let here = self
            .displays
            .iter()
            .position(|d| d.space == self.active_space)?;
        let next = bsp::neighbor(&frames, here as WinId, dir)?;
        self.displays.get(next as usize).map(|d| d.space)
    }

    /// The Space shown on the display `target` names: the next or previous
    /// one, or one by number, counting from 1 left to right (top to bottom
    /// where two are one above the other).
    fn resolve_display(&self, target: DisplayTarget) -> Result<u64, String> {
        let mut order: Vec<&Display> = self.displays.iter().collect();
        order.sort_by_key(|d| (d.frame.x, d.frame.y));
        let count = order.len();
        let here = order
            .iter()
            .position(|d| d.space == self.active_space)
            .unwrap_or(0);
        let i = match target {
            DisplayTarget::Next => (here + 1) % count,
            DisplayTarget::Prev => (here + count - 1) % count,
            DisplayTarget::Number(n) if n <= count => n - 1,
            DisplayTarget::Number(n) => return Err(format!("there is no display {n}")),
        };
        Ok(order[i].space)
    }

    /// The Space of the display at a point, if it is on another display
    /// than the current one.
    fn other_display_at(&self, (x, y): (f32, f32)) -> Option<u64> {
        self.displays
            .iter()
            .find(|d| d.frame.contains_point(x, y))
            .map(|d| d.space)
            .filter(|s| *s != self.active_space)
    }

    /// Makes current the display of window `id`, if it is tiled on another
    /// display on screen. Returns whether it is now in the current layout.
    fn bring_display_of(&mut self, id: WinId) -> bool {
        if let Some(space) = self.tiled_space_of(id)
            && space != self.active_space
            && self.shown_spaces().contains(&space)
        {
            self.make_current(space);
        }
        self.tree.contains(id)
    }

    /// The window a new window `id` splits: the focused one. But opening a
    /// window in an app that already has one (`open -na Terminal` from a
    /// shortcut, say) first brings that app to the front, and focus jumps
    /// to its old window just before the new one shows up. The window the
    /// user was in before that jump is the one meant.
    fn split_target(&self, id: WinId) -> Option<WinId> {
        const JUMP_SECS: f64 = 0.6;
        let app = self.apps.get(&id);
        let jumped = self.focus_moved.filter(|&(prev, at)| {
            self.clock - at < JUMP_SECS
                && self.tree.contains(prev)
                && app.is_some()
                && self.focused.and_then(|f| self.apps.get(&f)) == app
                && self.apps.get(&prev) != app
        });
        match jumped {
            Some((prev, _)) => Some(prev),
            None => self.focused,
        }
    }

    /// Asks the system which window has focus and follows it, but only
    /// when the answer changed: the system's answer can lag or get stuck
    /// (the Accessibility API is slow to notice a click), and a stale one
    /// must not undo a focus change cberry saw first. Returns the answer.
    fn sync_focus(&mut self) -> Option<WinId> {
        let now = self.backend.focused_window();
        self.note_focus(now);
        now
    }

    /// `sync_focus` with the system's answer `now` already asked for. Focus
    /// on a window of another display makes that display the current one.
    fn note_focus(&mut self, now: Option<WinId>) {
        if now != self.reported_focus {
            self.reported_focus = now;
            if let Some(f) = now.filter(|f| self.bring_display_of(*f)) {
                if let Some(prev) = self.focused.filter(|p| *p != f) {
                    self.focus_moved = Some((prev, self.clock));
                }
                self.focused = Some(f);
            }
        }
    }

    /// Every tiled window, with the Space whose layout it is in.
    fn all_tiled(&self) -> Vec<(u64, WinId)> {
        let mut out: Vec<(u64, WinId)> = self
            .tree
            .windows()
            .into_iter()
            .map(|id| (self.active_space, id))
            .collect();
        for (&space, state) in &self.spaces {
            out.extend(state.tree.windows().into_iter().map(|id| (space, id)));
        }
        out
    }

    fn is_tiled_anywhere(&self, id: WinId) -> bool {
        self.tree.contains(id) || self.spaces.values().any(|s| s.tree.contains(id))
    }

    fn remove_from_space(&mut self, space: u64, id: WinId) {
        self.mover.halt(id);
        if space == self.active_space {
            self.tree.remove(id);
        } else if let Some(state) = self.spaces.get_mut(&space) {
            state.tree.remove(id);
            if state.focused == Some(id) {
                state.focused = state.tree.windows().last().copied();
            }
            if state.tree.is_empty() {
                self.spaces.remove(&space);
            }
        }
    }

    /// Follows the mouse. Call this every frame: it is cheap while the button
    /// is up. Moving a tiled window by its title bar highlights the window
    /// under the pointer, and dropping it there swaps the two. Resizing a
    /// window moves the split borders when the button is released.
    pub fn track_pointer(&mut self) {
        if self.config.mouse_modifier != self.mouse_modifier {
            self.mouse_modifier = self.config.mouse_modifier;
            self.backend.set_mouse_modifier(self.mouse_modifier);
        }
        let gesture = self.backend.mouse_gesture();
        let pressed = gesture.filter(|_| self.gesture_was.is_none());
        self.gesture_was = gesture;
        if let Drag::Gesture(grab) = self.drag {
            if gesture == Some(grab.button) && self.grab_is_valid(&grab) {
                self.gesture_moved(grab);
            } else {
                self.gesture_ended(grab);
            }
            self.mouse_was_down = self.backend.mouse_down();
            return;
        }
        // A gesture's click is swallowed, but the button state can still
        // have been read as a plain click first; the gesture wins.
        let plain = matches!(self.drag, Drag::None | Drag::Pressed(_) | Drag::Other);
        if let Some(button) = pressed.filter(|_| plain) {
            self.drag = Drag::None;
            self.gesture_started(button);
            if matches!(self.drag, Drag::Gesture(_)) {
                self.mouse_was_down = self.backend.mouse_down();
                return;
            }
        }
        let down = self.backend.mouse_down();
        let was_down = std::mem::replace(&mut self.mouse_was_down, down);
        match (was_down, down) {
            (false, true) => self.press(),
            (true, true) => self.dragging(),
            (true, false) => self.release(),
            (false, false) => {
                if self.config.focus_follows_mouse && gesture.is_none() {
                    self.follow_mouse();
                }
            }
        }
    }

    /// Focus follows mouse: focuses the window the pointer came to rest on.
    /// Only a moving pointer counts, so a window sliding under a still one
    /// (opened, closed, swapped) does not take the focus.
    fn follow_mouse(&mut self) {
        let Some((x, y)) = self.backend.cursor() else {
            return;
        };
        let moved = self.last_cursor.is_some_and(|last| last != (x, y));
        self.last_cursor = Some((x, y));
        let under = self.window_under(x, y);
        if under != self.under {
            self.under = under;
            self.hover = under.filter(|_| moved).map(|w| (w, self.clock));
        }
        let Some((win, since)) = self.hover else {
            return;
        };
        if self.clock - since < FOCUS_FOLLOWS_MOUSE_DELAY {
            return;
        }
        self.hover = None;
        let tiled = self.tiled_space_of(win).is_some();
        if self.reported_focus == Some(win) {
            return;
        }
        // Focusing raises; a tiled window must not bury a floating one.
        if tiled {
            let frame = self.frames.get(&win).copied().unwrap_or_default();
            let buried = self
                .stack
                .iter()
                .take_while(|id| **id != win)
                .filter(|id| self.manageable.contains(id) && !self.is_tiled_anywhere(**id))
                .find(|id| self.frames.get(id).is_some_and(|f| f.intersects(&frame)));
            if let Some(float) = buried {
                debug!("focus follows mouse: window {win} would cover floating window {float}");
                return;
            }
        }
        // Spotlight and the like close when they lose the keyboard.
        if self.backend.panel_has_focus() {
            debug!("focus follows mouse: a panel has the keyboard; leaving it");
            return;
        }
        // Menus and popovers are not in the window list; focus would close them.
        if self.backend.covered_at(x, y) {
            debug!("focus follows mouse: window {win} is behind a menu");
            return;
        }
        debug!("focus follows mouse: window {win}");
        self.backend.focus(win);
        if tiled && self.bring_display_of(win) {
            self.focused = Some(win);
        }
    }

    /// The frontmost window cberry manages at a point, from the last poll.
    /// Helper windows that only look like windows are looked through.
    fn window_under(&self, x: f32, y: f32) -> Option<WinId> {
        self.stack
            .iter()
            .copied()
            .filter(|id| self.manageable.contains(id) || self.is_tiled_anywhere(*id))
            .find(|id| self.frames.get(id).is_some_and(|f| f.contains_point(x, y)))
    }

    /// Mouse follows focus: puts the pointer in the middle of `win`'s slot,
    /// unless it is already on it.
    fn warp_to(&mut self, win: WinId) {
        if !self.config.mouse_follows_focus {
            return;
        }
        let Some((_, rect)) = self.all_targets().into_iter().find(|(id, _)| *id == win) else {
            return;
        };
        if self
            .backend
            .cursor()
            .is_some_and(|(x, y)| rect.contains_point(x, y))
        {
            return;
        }
        let (x, y) = rect.center();
        self.backend.warp_cursor(x, y);
        // The pointer did not travel there: nothing for focus follows mouse.
        self.last_cursor = Some((x, y));
        self.under = Some(win);
        self.hover = None;
    }

    /// Lets the platform UI process its events; call once per frame.
    pub fn pump(&mut self) {
        self.hold_focus();
        self.update_border();
        self.backend.pump();
    }

    /// Takes focus back for the window it belongs to, for as long as macOS
    /// may still hand it elsewhere.
    fn hold_focus(&mut self) {
        let Some((win, until, next)) = self.holding_focus else {
            return;
        };
        let now = std::time::Instant::now();
        if now >= until {
            self.holding_focus = None;
        } else if now >= next {
            self.holding_focus = Some((win, until, now + HOLD_FOCUS_RETRY));
            if self.backend.focused_window() != Some(win) {
                self.backend.focus(win);
            }
        }
    }

    /// Where the ring goes this frame: around the focused window, wherever
    /// that is, moving with it.
    fn wanted_border(&mut self) -> Option<Border> {
        self.border_tracking = false;
        // Its window slides away with the Space; the ring would not.
        if !self.config.border || self.backend.space_swiping() {
            return None;
        }
        let id = self
            .reported_focus
            .filter(|id| self.stack.contains(id) && self.movable.contains(id))?;
        // A window dragged by its title bar tells nobody where it is going.
        let dragged = self.backend.mouse_down() && !matches!(self.drag, Drag::Gesture(_));
        let frame = self.frames.get(&id).copied()?;
        let live = dragged.then(|| self.backend.server_frame(id)).flatten();
        self.border_tracking = live.is_some();
        // The window server's idea of a frame can differ a little from the
        // one the window was last seen at, so only what changed since the
        // press counts: a click moves nothing.
        // `frames` catches up during a drag, so all counts from the press.
        let press = match (live, self.border_press) {
            (Some(_), Some(press)) if press.0 == id => Some(press),
            (Some(now), _) => Some((id, now, frame)),
            _ => None,
        };
        self.border_press = press;
        let rect = match live.zip(press) {
            Some((now, (_, start, frame))) => Rect::new(
                frame.x + now.x - start.x,
                frame.y + now.y - start.y,
                frame.w + now.w - start.w,
                frame.h + now.h - start.h,
            ),
            None => frame,
        };
        Some(Border {
            rect,
            window: id,
            color: self.config.border_color,
            width: self.config.border_width,
            radius: self
                .config
                .border_radius_for(self.apps.get(&id).map_or("", String::as_str)),
        })
    }

    fn update_border(&mut self) {
        let wanted = self.wanted_border();
        if wanted != self.shown_border {
            self.shown_border = wanted;
            self.backend.show_border(wanted);
        }
    }

    fn press(&mut self) {
        // Where everything was before the user touched it.
        self.before_drag.clone_from(&self.frames);
        let cursor = self.backend.cursor();
        // A click on another display makes it the current one.
        if let Some(space) = cursor.and_then(|c| self.other_display_at(c))
            && self.shown_spaces().contains(&space)
        {
            self.make_current(space);
        }
        let grabbed = cursor.and_then(|(x, y)| {
            let hit = |id: &WinId| self.frames.get(id).is_some_and(|f| f.contains_point(x, y));
            // A zoomed window is in front of the ones it covers.
            let zoomed = self.zoomed.map(|(w, _)| w).filter(hit);
            zoomed.or_else(|| {
                self.tree
                    .windows()
                    .into_iter()
                    .filter(|id| !self.mover.in_motion(*id))
                    .find(hit)
            })
        });
        if let Some(win) = grabbed {
            // A click focuses the window; know it now rather than when the
            // system gets round to saying so.
            self.focused = Some(win);
        }
        self.drag = grabbed.map_or(Drag::Other, Drag::Pressed);
    }

    fn dragging(&mut self) {
        let off = |a: i32, b: i32| (a - b).abs() > DRAG_THRESHOLD;
        match self.drag {
            Drag::Pressed(win) => {
                if !self.config.drag_swap {
                    return;
                }
                let (Some(now), Some(&before)) =
                    (self.backend.window_frame(win), self.before_drag.get(&win))
                else {
                    return;
                };
                if off(now.w, before.w) || off(now.h, before.h) {
                    self.drag = Drag::Other; // a resize
                } else if off(now.x, before.x) || off(now.y, before.y) {
                    debug!("window {win} is being moved");
                    self.drag = Drag::Moving {
                        win,
                        target: None,
                        side: None,
                    };
                    self.dragging();
                }
            }
            Drag::Moving { win, target, side } => {
                let Some(cursor) = self.backend.cursor() else {
                    return;
                };
                let (target, side) = self.show_drop_target(win, (target, side), cursor, false);
                self.drag = Drag::Moving { win, target, side };
            }
            Drag::None | Drag::Other | Drag::Gesture(_) => {}
        }
    }

    /// Whether the grabbed window can still be moved: a tiled one must still
    /// be in this Space's layout, any window must still be on screen.
    fn grab_is_valid(&self, grab: &Grab) -> bool {
        self.stack.contains(&grab.win) && (!grab.tiled || self.tree.contains(grab.win))
    }

    /// The button went down with the modifier held: grab the window under
    /// the pointer, wherever it was clicked.
    fn gesture_started(&mut self, button: MouseButton) {
        let Some((x, y)) = self.backend.cursor() else {
            return;
        };
        let Some((win, frame)) = self.stack.iter().find_map(|id| {
            let f = *self.frames.get(id)?;
            f.contains_point(x, y).then_some((*id, f))
        }) else {
            debug!("mouse gesture at {x:.0},{y:.0}: no window there");
            return;
        };
        let action = self.config.mouse_action(button);
        let tiled = self.bring_display_of(win);
        if tiled && self.zoomed.is_some_and(|(w, _)| w == win) && action == MouseAction::Resize {
            // Resizing works on the split borders, which a zoom hides.
            self.zoomed = None;
            self.relayout();
        }
        self.mover.halt(win);
        self.backend.focus(win);
        if tiled {
            self.focused = Some(win);
        }
        let (cx, cy) = frame.center();
        let horizontal = if x < cx {
            Direction::Left
        } else {
            Direction::Right
        };
        let vertical = if y < cy {
            Direction::Up
        } else {
            Direction::Down
        };
        debug!("window {win} grabbed to {}", action.name());
        self.drag = Drag::Gesture(Grab {
            win,
            button,
            action,
            start: (x, y),
            frame,
            tiled,
            horizontal,
            vertical,
            target: None,
            side: None,
            moved: false,
        });
    }

    fn gesture_moved(&mut self, mut grab: Grab) {
        let Some((x, y)) = self.backend.cursor() else {
            return;
        };
        let dx = (x - grab.start.0).round() as i32;
        let dy = (y - grab.start.1).round() as i32;
        if !grab.moved && dx.abs() <= DRAG_THRESHOLD && dy.abs() <= DRAG_THRESHOLD {
            // A click: nothing moves, nothing swaps.
            return;
        }
        grab.moved = true;
        let f = grab.frame;
        match grab.action {
            MouseAction::Move => {
                let frame = Rect::new(f.x + dx, f.y + dy, f.w, f.h);
                let prev = self.frames.get(&grab.win).copied().unwrap_or(f);
                if frame != prev {
                    self.backend.update_frame(grab.win, frame, prev);
                    self.frames.insert(grab.win, frame);
                    self.float_zooms.remove(&grab.win);
                }
                let insert = self
                    .config
                    .mouse_insert_modifier
                    .is_some_and(|k| self.backend.modifier_held(k));
                if grab.tiled && (self.config.drag_swap || insert) {
                    (grab.target, grab.side) =
                        self.show_drop_target(grab.win, (grab.target, grab.side), (x, y), insert);
                }
            }
            MouseAction::Resize => {
                let (mut left, mut top, mut right, mut bottom) = (f.x, f.y, f.max_x(), f.max_y());
                match grab.horizontal {
                    Direction::Left => left = (left + dx).min(right - MIN_GESTURE_SIZE),
                    _ => right = (right + dx).max(left + MIN_GESTURE_SIZE),
                }
                match grab.vertical {
                    Direction::Up => top = (top + dy).min(bottom - MIN_GESTURE_SIZE),
                    _ => bottom = (bottom + dy).max(top + MIN_GESTURE_SIZE),
                }
                let frame = Rect::new(left, top, right - left, bottom - top);
                if grab.tiled {
                    // The dragged edges become split borders; the layout
                    // follows, neighbours included.
                    let (area, gap) = (self.inner_area(), self.config.gap);
                    let unchanged = self.tree.clone();
                    self.note_resize(&unchanged, None);
                    let coord = |dir| match dir {
                        Direction::Left => frame.x - gap,
                        Direction::Up => frame.y - gap,
                        Direction::Right => frame.max_x(),
                        Direction::Down => frame.max_y(),
                    };
                    if dx != 0 {
                        self.tree.set_border(
                            grab.win,
                            grab.horizontal,
                            coord(grab.horizontal),
                            area,
                            gap,
                        );
                    }
                    if dy != 0 {
                        self.tree.set_border(
                            grab.win,
                            grab.vertical,
                            coord(grab.vertical),
                            area,
                            gap,
                        );
                    }
                    // No further than the windows there accept.
                    (self.tree, _) = self.limited(&unchanged, self.tree.clone());
                    self.relayout_now();
                } else {
                    let prev = self.frames.get(&grab.win).copied().unwrap_or(f);
                    if frame != prev {
                        self.backend.update_frame(grab.win, frame, prev);
                        self.frames.insert(grab.win, frame);
                        self.float_zooms.remove(&grab.win);
                    }
                }
            }
        }
        self.drag = Drag::Gesture(grab);
    }

    fn gesture_ended(&mut self, grab: Grab) {
        self.drag = Drag::None;
        let now = self.frames.get(&grab.win).copied().unwrap_or(grab.frame);
        let pointer = self.backend.cursor().unwrap_or(grab.start);
        debug!(
            "window {} let go: pointer moved {:.0},{:.0}, window now at {},{} {}x{}",
            grab.win,
            pointer.0 - grab.start.0,
            pointer.1 - grab.start.1,
            now.x,
            now.y,
            now.w,
            now.h
        );
        if grab.action == MouseAction::Move && grab.tiled {
            self.set_overlay(None);
            // Onto a window of another display: it goes there.
            let elsewhere = grab
                .target
                .and_then(|t| self.tiled_space_of(t).map(|s| (t, s)))
                .filter(|(_, s)| *s != self.active_space);
            if let Some((other, space)) = elsewhere {
                match grab.side {
                    Some(side) => {
                        self.move_to_display(grab.win, space, Some(other), Some(side));
                    }
                    None => {
                        self.exchange(grab.win, other);
                    }
                }
                self.relayout();
                return;
            }
            // Onto another display with no window under the pointer.
            if grab.target.is_none()
                && let Some(space) = self.other_display_at(pointer)
            {
                self.move_to_display(grab.win, space, None, None);
                self.relayout();
                return;
            }
            let target = grab.target.filter(|t| self.tree.contains(*t));
            if let Some(other) = target.filter(|_| self.tree.contains(grab.win)) {
                let mut tree = self.tree.clone();
                match grab.side {
                    Some(side) => {
                        tree.remove(grab.win);
                        tree.insert_at(grab.win, other, side);
                    }
                    None => {
                        tree.swap(grab.win, other);
                    }
                }
                if let Some(w) = self.squeezed(&self.tree, &tree) {
                    info!("window {w} would not fit; window {} goes back", grab.win);
                } else {
                    match grab.side {
                        Some(side) => info!("putting window {} {side:?} of {other}", grab.win),
                        None => info!("swapping window {} with {other}", grab.win),
                    }
                    if self
                        .zoomed
                        .is_some_and(|(w, _)| w == grab.win || w == other)
                    {
                        self.zoomed = None;
                    }
                    self.tree = tree;
                }
            }
            // Back into a slot, animated from where it was let go.
            self.relayout();
        }
    }

    /// Highlights the tiled window under the pointer that `win` would swap
    /// with if dropped now, and returns it. With `insert`, only the half of
    /// it `win` would take is highlighted, and that side is returned too.
    ///
    /// The highlight covers the window where it really is, which is not
    /// always its slot: some apps round their size to a text grid, others
    /// will not shrink to it. The pointer is matched against the same frames,
    /// so what is highlighted is what a drop hits.
    fn show_drop_target(
        &mut self,
        win: WinId,
        _current: (Option<WinId>, Option<Direction>),
        (x, y): (f32, f32),
        insert: bool,
    ) -> (Option<WinId>, Option<Direction>) {
        let under = self
            .all_targets()
            .into_iter()
            .filter(|(id, _)| *id != win)
            .map(|(id, slot)| (id, slot, self.drawn_frame(id, slot)))
            .filter(|(_, _, frame)| frame.contains_point(x, y))
            // Where frames overlap, the window whose slot it is wins.
            .max_by_key(|(_, slot, _)| slot.contains_point(x, y));
        // A window carried to another display joins it, splitting the
        // window there rather than trading places with it.
        let insert = insert || under.is_some_and(|(id, _, _)| !self.tree.contains(id));
        let side = under
            .filter(|_| insert)
            .map(|(_, _, frame)| frame.nearest_side(x, y));
        self.set_overlay(under.map(|(id, _, frame)| Overlay {
            rect: side.map_or(frame, |s| frame.half(s)),
            above: id,
            color: self.config.drag_color,
        }));
        (under.map(|(id, _, _)| id), side)
    }

    /// Where a tiled window is on screen: its last known frame, or its slot
    /// while it is still sliding there.
    fn drawn_frame(&self, id: WinId, slot: Rect) -> Rect {
        if self.mover.in_motion(id) {
            return slot;
        }
        self.frames.get(&id).copied().unwrap_or(slot)
    }

    /// Shows or hides the drop highlight, touching the screen only when it
    /// changes.
    fn set_overlay(&mut self, overlay: Option<Overlay>) {
        if overlay != self.shown_overlay {
            self.shown_overlay = overlay;
            self.backend.show_overlay(overlay);
        }
    }

    fn release(&mut self) {
        match std::mem::replace(&mut self.drag, Drag::None) {
            Drag::Moving { win, target, side } => {
                self.set_overlay(None);
                // Animate from where the user let go.
                if let Some(now) = self.backend.window_frame(win) {
                    self.frames.insert(win, now);
                }
                let cursor = self.backend.cursor();
                match target {
                    Some(other) if !self.tree.contains(other) => {
                        if let Some(space) = self.tiled_space_of(other) {
                            self.move_to_display(win, space, Some(other), side);
                        }
                    }
                    None if let Some(space) = cursor.and_then(|c| self.other_display_at(c)) => {
                        self.move_to_display(win, space, None, None);
                    }
                    Some(other) => {
                        let mut tree = self.tree.clone();
                        tree.swap(win, other);
                        if let Some(w) = self.squeezed(&self.tree, &tree) {
                            info!("window {w} would not fit; window {win} goes back");
                        } else {
                            info!("swapping window {win} with {other}");
                            self.tree = tree;
                        }
                    }
                    None => debug!("window {win} dropped on no window; returning it"),
                }
                self.relayout();
            }
            _ => {
                // A resize ends here: read where windows ended up, then
                // turn the dragged edges into split borders.
                self.sync(false);
                if self.reconcile_drag() {
                    self.relayout();
                }
            }
        }
    }

    /// Works out what the user did with the mouse, by comparing every tiled
    /// window with its frame from before the button went down. Only the
    /// window that changed the most counts: one window is dragged at a time.
    /// Returns whether the layout needs to be re-applied.
    fn reconcile_drag(&mut self) -> bool {
        use crate::geometry::Direction::{Down, Left, Right, Up};

        let off = |a: i32, b: i32| (a - b).abs() > DRAG_THRESHOLD;
        let dragged = self
            .tree
            .windows()
            .into_iter()
            .filter(|id| !self.mover.in_motion(*id))
            .filter_map(|id| {
                let before = *self.before_drag.get(&id)?;
                let after = *self.frames.get(&id)?;
                let change = (after.x - before.x).abs()
                    + (after.y - before.y).abs()
                    + (after.max_x() - before.max_x()).abs()
                    + (after.max_y() - before.max_y()).abs();
                (change > DRAG_THRESHOLD).then_some((change, id, before, after))
            })
            .max_by_key(|(change, ..)| *change);
        let Some((_, id, before, after)) = dragged else {
            return false;
        };

        if self.zoomed.is_some_and(|(w, _)| w == id) {
            // Its frame is not its slot, so its edges say nothing about the
            // borders; the user is done with the zoom.
            debug!("zoomed window {id} changed by mouse; unzooming it");
            self.zoomed = None;
            return true;
        }
        if !off(after.w, before.w) && !off(after.h, before.h) {
            debug!("window {id} moved by mouse; returning it to its slot");
            return true;
        }
        // Each edge the user dragged becomes a border position in the tree,
        // as far as the windows there accept.
        let (area, gap) = (self.inner_area(), self.config.gap);
        let unchanged = self.tree.clone();
        self.note_resize(&unchanged, None);
        if off(after.max_x(), before.max_x()) {
            self.tree.set_border(id, Right, after.max_x(), area, gap);
        }
        if off(after.x, before.x) {
            self.tree.set_border(id, Left, after.x - gap, area, gap);
        }
        if off(after.max_y(), before.max_y()) {
            self.tree.set_border(id, Down, after.max_y(), area, gap);
        }
        if off(after.y, before.y) {
            self.tree.set_border(id, Up, after.y - gap, area, gap);
        }
        (self.tree, _) = self.limited(&unchanged, self.tree.clone());
        debug!("window {id} resized by mouse");
        true
    }

    /// Advances animations by `dt` seconds and pushes the new frames.
    pub fn tick(&mut self, dt: f64) {
        self.clock += dt;
        let mut moves = self.mover.advance(dt);
        // Windows making room go first: a window that grows into the space
        // a neighbour has not left yet would cover it for a frame.
        moves.sort_by_key(|(id, frame)| {
            self.frames
                .get(id)
                .is_some_and(|prev| frame.w > prev.w || frame.h > prev.h)
        });
        for (id, frame) in moves {
            // The last frame takes the safe order; it is the one that stays.
            let more = self.mover.in_motion(id);
            match self.frames.insert(id, frame) {
                Some(prev) if more => self.backend.animate_frame(id, frame, prev),
                Some(prev) => self.backend.update_frame(id, frame, prev),
                None => self.backend.set_frame(id, frame),
            }
        }
    }

    fn inner_area(&self) -> Rect {
        self.area.inset(&self.config.padding)
    }

    /// The inner area of the display a window at `frame` is on, by its
    /// centre. A floating window may be on any display, whichever is current.
    fn inner_area_around(&self, frame: Rect) -> Rect {
        let (x, y) = (
            (frame.x + frame.w / 2) as f32,
            (frame.y + frame.h / 2) as f32,
        );
        self.displays
            .iter()
            .find(|d| d.frame.contains_point(x, y))
            .map_or(self.area, |d| d.area)
            .inset(&self.config.padding)
    }

    /// `cberry resize` for a floating window: the rule of tiled ones, with
    /// the display's edges for the screen's. `dir` moves its left (top)
    /// edge, or its right (bottom) one when it is against the left (top) of
    /// the display, by `amount` of the display.
    fn resize_floating(&mut self, win: WinId, dir: Direction, amount: f32) -> Result<(), String> {
        let now = self
            .heading_of(win)
            .ok_or("the focused window is not managed")?;
        if !self.manageable.contains(&win) {
            return Err("the focused window cannot be resized".into());
        }
        let area = self.inner_area_around(now);
        let step = |len: i32| (len as f32 * amount).round() as i32;
        let (mut left, mut top, mut right, mut bottom) = (now.x, now.y, now.max_x(), now.max_y());
        match dir {
            Direction::Left => {
                move_edge(&mut left, &mut right, area.x, area.max_x(), -step(area.w))
            }
            Direction::Right => {
                move_edge(&mut left, &mut right, area.x, area.max_x(), step(area.w))
            }
            Direction::Up => move_edge(&mut top, &mut bottom, area.y, area.max_y(), -step(area.h)),
            Direction::Down => move_edge(&mut top, &mut bottom, area.y, area.max_y(), step(area.h)),
        }
        let frame = Rect::new(left, top, right - left, bottom - top);
        if frame == now {
            return Err("the border is as far as it goes".into());
        }
        self.float_zooms.remove(&win);
        self.glide(win, frame);
        Ok(())
    }

    /// Where a window is, or will be once its animation ends: a key held
    /// down moves on from there, not from a frame in flight.
    fn heading_of(&self, win: WinId) -> Option<Rect> {
        self.mover
            .heading(win)
            .or_else(|| self.frames.get(&win).copied())
    }

    /// Sends a floating window to `frame`, animated as tiled windows are.
    fn glide(&mut self, win: WinId, frame: Rect) {
        let Some(&now) = self.frames.get(&win) else {
            return;
        };
        if let Some(frame) = self.mover.send(win, now, frame) {
            self.backend.update_frame(win, frame, now);
            self.frames.insert(win, frame);
        }
    }

    /// Where every tiled window belongs: its slot in the tree, or the zoomed
    /// window's larger area.
    fn targets(&self) -> Vec<(WinId, Rect)> {
        let mut targets = self.slots();
        if let Some(zoomed) = self.zoom_rect() {
            for t in &mut targets {
                if t.0 == zoomed.0 {
                    t.1 = zoomed.1;
                }
            }
        }
        targets
    }

    /// The zoomed window and the area it covers.
    fn zoom_rect(&self) -> Option<(WinId, Rect)> {
        zoom_rect(&self.tree, self.zoomed, self.inner_area(), self.config.gap)
    }

    /// `targets`, for the windows on every display.
    fn all_targets(&self) -> Vec<(WinId, Rect)> {
        let mut out = self.targets();
        let gap = self.config.gap;
        for d in &self.displays {
            if d.space == self.active_space {
                continue;
            }
            let Some(state) = self.spaces.get(&d.space) else {
                continue;
            };
            let inner = d.area.inset(&self.config.padding);
            let zoomed = zoom_rect(&state.tree, state.zoomed, inner, gap);
            out.extend(
                state
                    .tree
                    .layout(inner, gap)
                    .into_iter()
                    .map(|(id, r)| match zoomed {
                        Some((z, rect)) if z == id => (id, rect),
                        _ => (id, r),
                    }),
            );
        }
        out
    }

    /// `slots`, for the windows on every display: the neighbours of a window
    /// at the edge of its display are on the next one.
    fn all_slots(&self) -> Vec<(WinId, Rect)> {
        let mut out = self.slots();
        let gap = self.config.gap;
        for d in &self.displays {
            if d.space == self.active_space {
                continue;
            }
            if let Some(state) = self.spaces.get(&d.space) {
                out.extend(state.tree.layout(d.area.inset(&self.config.padding), gap));
            }
        }
        out
    }

    /// Frames as the tree lays them out, ignoring zoom: what the neighbours
    /// of a window are, for `focus` and `swap`.
    fn slots(&self) -> Vec<(WinId, Rect)> {
        self.tree.layout(self.inner_area(), self.config.gap)
    }

    /// Sends every tiled window toward its slot in the tree.
    /// Puts every tiled window in its slot at once, without animating:
    /// while the user drags a border, the windows must keep up with it.
    fn relayout_now(&mut self) {
        for (id, target) in self.targets() {
            self.mover.halt(id);
            let prev = self.frames.get(&id).copied().unwrap_or(target);
            if prev != target {
                self.backend.update_frame(id, target, prev);
                self.frames.insert(id, target);
            }
        }
    }

    fn relayout(&mut self) {
        // A zoomed window that left the layout (closed, floated, moved to
        // another Space) takes its zoom with it.
        if self.zoomed.is_some_and(|(w, _)| !self.tree.contains(w)) {
            self.zoomed = None;
        }
        // A window the user is moving stays under the pointer.
        let held = match self.drag {
            Drag::Gesture(g) if g.action == MouseAction::Move => Some(g.win),
            _ => None,
        };
        for (id, target) in self.all_targets() {
            if Some(id) == held {
                continue;
            }
            let current = self.frames.get(&id).copied().unwrap_or(target);
            if let Some(frame) = self.mover.send(id, current, target) {
                self.backend.update_frame(id, frame, current);
                self.frames.insert(id, frame);
            }
        }
    }

    /// Runs a command. `Reload` and `Quit` are the caller's business.
    pub fn execute(&mut self, cmd: &Command) -> Result<String, String> {
        // The user may have switched windows since the last poll.
        self.sync_focus();
        let focused_result = self
            .focused
            .ok_or_else(|| "no tiled window is focused".to_string());
        let focused = || focused_result.clone();

        match *cmd {
            Command::Focus(dir) => {
                let from = focused()?;
                // At the edge of a display, the next display's windows.
                let to = bsp::neighbor(&self.all_slots(), from, dir)
                    .ok_or("no window in that direction")?;
                self.backend.focus(to);
                self.bring_display_of(to);
                self.focused = Some(to);
                self.warp_to(to);
            }
            Command::Swap(dir) => {
                let from = focused()?;
                let to = bsp::neighbor(&self.all_slots(), from, dir)
                    .ok_or("no window in that direction")?;
                if !self.tree.contains(to) {
                    self.exchange(from, to);
                    self.relayout();
                    self.warp_to(from);
                    return Ok(String::new());
                }
                let mut tree = self.tree.clone();
                tree.swap(from, to);
                if let Some(w) = self.squeezed(&self.tree, &tree) {
                    return Err(format!("window {w} would not fit there"));
                }
                self.tree = tree;
                self.warp_to(from);
            }
            Command::Warp(dir) => {
                let from = focused()?;
                // Across the edge of the display: into the next display's
                // layout, next to its window there, or alone on it.
                let next = bsp::neighbor(&self.all_slots(), from, dir);
                let across = match next {
                    Some(to) => self.tiled_space_of(to).filter(|s| *s != self.active_space),
                    None => self.display_toward(dir),
                };
                if let Some(space) = across {
                    if !self.move_to_display(from, space, next, None) {
                        return Err("no window in that direction".into());
                    }
                    self.relayout();
                    self.warp_to(from);
                    return Ok(String::new());
                }
                let to = next.ok_or("no window in that direction")?;
                let (inner, gap, ratio) =
                    (self.inner_area(), self.config.gap, self.config.split_ratio);
                let before: HashMap<WinId, Rect> = self.slots().into_iter().collect();
                let mut tree = self.tree.clone();
                tree.remove(from);
                // The neighbour splits the way it would for a new window
                // (a tall one into top and bottom), and the window keeps
                // the side it came from across that split: from the bottom
                // left, it goes to the bottom.
                tree.insert(from, Some(to), inner, gap, ratio);
                let after: HashMap<WinId, Rect> = tree.layout(inner, gap).into_iter().collect();
                let (fa, ta) = (after[&from], after[&to]);
                let (fc, tc) = (before[&from].center(), before[&to].center());
                let side_by_side = fa.y == ta.y;
                let wants_first = if side_by_side {
                    fc.0 < tc.0
                } else {
                    fc.1 < tc.1
                };
                let is_first = if side_by_side {
                    fa.x < ta.x
                } else {
                    fa.y < ta.y
                };
                if wants_first != is_first {
                    tree.swap(from, to);
                }
                // Already where it would go: the two trade places instead.
                if tree.layout(inner, gap) == self.tree.layout(inner, gap) {
                    tree.swap(from, to);
                }
                if let Some(w) = self.squeezed(&self.tree, &tree) {
                    return Err(format!("window {w} would not fit there"));
                }
                if self.zoomed.is_some_and(|(w, _)| w == from || w == to) {
                    self.zoomed = None;
                }
                info!("window {from} warped {dir:?} into window {to}'s place");
                self.tree = tree;
                self.warp_to(from);
            }
            Command::Resize(dir, amount) => {
                let amount = amount.unwrap_or(self.config.resize_step);
                let floating = self
                    .backend
                    .focused_window()
                    .filter(|w| !self.is_tiled_anywhere(*w));
                if let Some(win) = floating {
                    self.resize_floating(win, dir, amount)?;
                    return Ok(String::new());
                }
                let win = focused()?;
                let before = self.slots();
                let unchanged = self.tree.clone();
                self.note_resize(&unchanged, Some((win, dir, amount)));
                let (area, gap) = (self.inner_area(), self.config.gap);
                if !self.tree.resize(win, dir, amount, area, gap) {
                    return Err("nothing to resize in that direction".into());
                }
                // As far as the windows there accept: the same resize, by a
                // smaller amount (so the windows away from the border still
                // keep their size).
                let step = |t: f32| {
                    let mut tree = unchanged.clone();
                    tree.resize(win, dir, amount * t, area, gap).then_some(tree)
                };
                let after = self.tree.clone();
                let (tree, stopper) = self.limited_by(&unchanged, &after, step);
                self.tree = tree;
                if self.slots() == before {
                    self.tree = unchanged;
                    return Err(match stopper {
                        Some(w) => format!("window {w} will not go smaller"),
                        None => "the border is as far as it goes".into(),
                    });
                }
            }
            Command::Rotate => self.tree.rotate(),
            Command::Flip => {
                if !self.tree.flip(focused()?) {
                    return Err("the window is not split".into());
                }
            }
            Command::ToggleSplit => {
                if !self.tree.toggle_split(focused()?) {
                    return Err("the window is not split".into());
                }
            }
            Command::Balance => self.tree.equalize(self.config.split_ratio),
            Command::BalanceArea => self.tree.balance(),
            Command::Zoom(zoom) => {
                // A floating window has no split: it covers the screen, and
                // goes back to where it was.
                let float = self
                    .backend
                    .focused_window()
                    .filter(|w| self.movable.contains(w) && !self.is_tiled_anywhere(*w));
                if let Some(win) = float {
                    match self.float_zooms.remove(&win) {
                        Some(before) => self.glide(win, before),
                        None => {
                            let now = self
                                .heading_of(win)
                                .ok_or("the focused window is not managed")?;
                            self.float_zooms.insert(win, now);
                            self.glide(win, self.inner_area_around(now));
                        }
                    }
                    return Ok(String::new());
                }
                let win = focused()?;
                if self.zoomed == Some((win, zoom)) {
                    self.zoomed = None;
                } else {
                    if zoom == Zoom::Parent
                        && self
                            .tree
                            .parent_rect(win, self.inner_area(), self.config.gap)
                            .is_none()
                    {
                        return Err("the window is not split".into());
                    }
                    self.zoomed = Some((win, zoom));
                    // Over the others, which stay where they are.
                    self.backend.focus(win);
                }
            }
            Command::ToggleFloat => {
                let win = self
                    .backend
                    .focused_window()
                    .or(self.focused)
                    .ok_or("no window is focused")?;
                if self.tree.remove(win) {
                    self.floating.insert(win);
                    self.mover.halt(win);
                    self.focused = self.tree.windows().last().copied();
                    info!("window {win} floating");
                } else {
                    if self.float_space_ids().contains(&self.active_space) {
                        return Err(
                            "this Space floats; tile it with `cberry layout <space> bsp`".into(),
                        );
                    }
                    self.tile(win, self.focused);
                    self.floating.remove(&win);
                    self.focused = Some(win);
                    info!("window {win} tiled");
                }
            }
            Command::ToggleFloatAt(grid) => {
                let win = self
                    .backend
                    .focused_window()
                    .or(self.focused)
                    .ok_or("no window is focused")?;
                return if self.is_tiled_anywhere(win) {
                    self.execute(&Command::Grid(grid))
                } else {
                    self.execute(&Command::ToggleFloat)
                };
            }
            Command::Grid(grid) => {
                let win = self
                    .backend
                    .focused_window()
                    .or(self.focused)
                    .ok_or("no window is focused")?;
                let now = self
                    .heading_of(win)
                    .ok_or("the focused window is not managed")?;
                let area = self.inner_area_around(now);
                let cell = area.grid_cell(grid);
                // One that cannot be resized keeps its size, as with a rule.
                let frame = if self.manageable.contains(&win) {
                    cell
                } else {
                    area.align_in(cell, now.w, now.h)
                };
                if self.tree.remove(win) {
                    self.focused = self.tree.windows().last().copied();
                }
                self.floating.insert(win);
                self.float_zooms.remove(&win);
                self.glide(win, frame);
                info!("window {win} floats at grid {grid:?}: {frame:?}");
            }
            Command::RaiseFloating => {
                let focused_now = self.backend.focused_window();
                let mut floats: Vec<WinId> = self
                    .frames
                    .keys()
                    .copied()
                    .filter(|id| !self.is_tiled_anywhere(*id))
                    .filter(|id| {
                        self.floating.contains(id)
                            || self.apps.get(id).is_some_and(|app| {
                                self.config.float_apps.iter().any(|a| app_matches(a, app))
                            })
                    })
                    .collect();
                if floats.is_empty() {
                    return Err("no floating windows on this Space".into());
                }
                // Stable order; the focused one last, so it ends on top and
                // keeps the keyboard.
                floats.sort_unstable();
                floats.sort_by_key(|id| Some(*id) == focused_now);
                for &id in &floats {
                    self.backend.focus(id);
                }
                return Ok(String::new());
            }
            Command::Retile => {
                // Also a way out if a minimum size was learned wrongly.
                self.min_sizes.clear();
                self.frames.clear();
                self.refresh();
                self.relayout();
            }
            Command::Query => return Ok(self.describe()),
            Command::QueryJson => return Ok(self.describe_json()),
            Command::QuerySpaces => return Ok(self.describe_spaces()),
            Command::DaemonVersion => return Ok(format!("{}\n", env!("CARGO_PKG_VERSION"))),
            Command::Bind(None) => {
                let mut out = String::new();
                for line in &self.config.hotkeys {
                    out.push_str(line);
                    out.push('\n');
                }
                return Ok(out);
            }
            Command::Bind(Some(ref line)) => {
                let mut lines = self.config.hotkeys.clone();
                lines.push(line.clone());
                crate::hotkeys::check(line)?;
                let mut config = self.config.clone();
                config.hotkeys = lines;
                self.set_config(config);
                return Ok(String::new());
            }
            Command::Config { ref key, ref args } => {
                if args.is_empty() {
                    return self.config.get(key).map(|v| v + "\n");
                }
                let mut config = self.config.clone();
                let args: Vec<&str> = args.iter().map(String::as_str).collect();
                config.set(key, &args)?;
                self.set_config(config);
                return Ok(String::new());
            }
            Command::Rule { float, ref app } => {
                let mut config = self.config.clone();
                config.set_float_app(app, float);
                self.set_config(config);
                return Ok(String::new());
            }
            Command::Label(None) => {
                let current = self.current_space();
                let spaces = self.backend.display_spaces(current);
                let float_spaces = self.float_space_ids();
                let mut out = String::new();
                for (i, sid) in spaces.iter().enumerate() {
                    let n = i + 1;
                    let label = self.config.labels.get(&n).map_or("-", String::as_str);
                    let floats = if float_spaces.contains(sid) {
                        "\tfloat"
                    } else {
                        ""
                    };
                    let here = if *sid == current { "\tcurrent" } else { "" };
                    out.push_str(&format!("{n}\t{label}{floats}{here}\n"));
                }
                return Ok(out);
            }
            Command::Label(Some((n, ref name))) => {
                self.config.set_label(n, name);
                return Ok(String::new());
            }
            Command::SpaceRule { ref space, ref app } => {
                self.config.set_space_rule(app, space);
                return Ok(String::new());
            }
            Command::GridRule { grid, ref app } => {
                self.config.set_grid_rule(app, grid);
                return Ok(String::new());
            }
            Command::BorderRadiusRule { radius, ref app } => {
                self.config.set_border_radius_rule(app, radius);
                return Ok(String::new());
            }
            Command::Send(ref space) => {
                let win = self
                    .backend
                    .focused_window()
                    .ok_or("no window is focused")?;
                let (n, sid) = self.resolve_space(space)?;
                if sid == self.active_space {
                    return Err(format!("the window is already on Space {n}"));
                }
                if !self.backend.move_to_space(win, sid) {
                    return Err(format!("macOS would not move the window to Space {n}"));
                }
                info!("window {win} sent to Space {n}");
                self.sent = Some((win, sid, std::time::Instant::now()));
                // Its neighbours fill the gap; it joins that Space's layout.
                self.refresh();
                if self.active_space == sid {
                    // Taken there along with it.
                    self.backend.focus(win);
                } else {
                    if self.focused.is_none() {
                        self.focused = self.tree.windows().first().copied();
                    }
                    if let Some(next) = self.focused {
                        self.backend.focus(next);
                    }
                }
                return Ok(String::new());
            }
            Command::SendDisplay(target) => {
                let win = focused()?;
                let space = self.resolve_display(target)?;
                if space == self.active_space {
                    return Err("the window is already on that display".into());
                }
                self.move_to_display(win, space, None, None);
                self.relayout();
                self.warp_to(win);
                return Ok(String::new());
            }
            Command::Layout { ref space, float } => {
                let (n, _) = self.resolve_space(space)?;
                self.config.set_float_space(space, float);
                info!("Space {n} {}", if float { "floats" } else { "is tiled" });
                // Lets go of the windows, or tiles them, right away.
                self.refresh();
            }
            Command::Space(ref space) => {
                let (_, sid) = self.resolve_space(space)?;
                if sid != self.current_space() {
                    self.backend.show_space(sid)?;
                }
                if let Some((win, _, _)) = self
                    .sent
                    .take()
                    .filter(|(_, to, at)| *to == sid && at.elapsed() < FOLLOW_SENT)
                {
                    self.focused = Some(win);
                    self.backend.focus(win);
                    let now = std::time::Instant::now();
                    self.holding_focus = Some((win, now + HOLD_FOCUS, now + HOLD_FOCUS_RETRY));
                }
                return Ok(String::new());
            }
            Command::Reload | Command::Quit => return Err("handled by the daemon".into()),
        }
        self.relayout();
        Ok(String::new())
    }

    /// The windows as a JSON array, in the order of [`Self::describe`].
    pub fn describe_json(&self) -> String {
        let mut ids: Vec<WinId> = self.frames.keys().copied().collect();
        ids.sort_unstable();
        let windows: Vec<String> = ids
            .into_iter()
            .map(|id| {
                let f = self.mover.heading(id).unwrap_or(self.frames[&id]);
                let app = self.apps.get(&id).map_or("?", String::as_str);
                format!(
                    "{{\"id\":{id},\"app\":{},\"x\":{},\"y\":{},\"w\":{},\"h\":{},\
                     \"state\":\"{}\",\"focused\":{}}}",
                    json_string(app),
                    f.x,
                    f.y,
                    f.w,
                    f.h,
                    self.window_state(id),
                    self.focused == Some(id)
                )
            })
            .collect();
        format!("[{}]\n", windows.join(","))
    }

    /// The Spaces of the current display as a JSON array, in Mission Control
    /// order. `windows` counts the tiled ones.
    fn describe_spaces(&mut self) -> String {
        let current = self.current_space();
        let float_spaces = self.float_space_ids();
        let spaces: Vec<String> = self
            .backend
            .display_spaces(current)
            .into_iter()
            .enumerate()
            .map(|(i, id)| {
                let index = i + 1;
                let label = self
                    .config
                    .labels
                    .get(&index)
                    .map_or("null".to_string(), |l| json_string(l));
                let windows = if id == self.active_space {
                    self.tree.windows().len()
                } else {
                    self.spaces.get(&id).map_or(0, |s| s.tree.windows().len())
                };
                format!(
                    "{{\"index\":{index},\"label\":{label},\"focused\":{},\"float\":{},\
                     \"windows\":{windows}}}",
                    id == current,
                    float_spaces.contains(&id)
                )
            })
            .collect();
        format!("[{}]\n", spaces.join(","))
    }

    /// `zoomed`, `tiled` or `floating`.
    fn window_state(&self, id: WinId) -> &'static str {
        if self.zoomed.is_some_and(|(w, _)| w == id) {
            "zoomed"
        } else if self.is_tiled_anywhere(id) {
            "tiled"
        } else {
            "floating"
        }
    }

    /// One line per window: `id app x,y wxh [tiled|floating] [focused]`.
    pub fn describe(&self) -> String {
        let mut ids: Vec<WinId> = self.frames.keys().copied().collect();
        ids.sort_unstable();
        let mut out = String::new();
        for id in ids {
            let f = self.mover.heading(id).unwrap_or(self.frames[&id]);
            let state = self.window_state(id);
            let focus = if self.focused == Some(id) {
                " focused"
            } else {
                ""
            };
            let app = self.apps.get(&id).map_or("?", String::as_str);
            // Which `cberry rule float` floats the window, so a rule that
            // does or does not match is easy to see.
            let rule = if state == "floating" {
                self.config
                    .float_apps
                    .iter()
                    .find(|r| app_matches(r, app))
                    .map(|r| format!("\trule: {r}"))
                    .unwrap_or_default()
            } else {
                String::new()
            };
            out.push_str(&format!(
                "{id}\t{app}\t{},{} {}x{}\t{state}{focus}{rule}\n",
                f.x, f.y, f.w, f.h
            ));
        }
        out
    }
}

/// `text` as a JSON string.
fn json_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The zoomed window of a layout and the area it covers.
fn zoom_rect(
    tree: &BspTree,
    zoomed: Option<(WinId, Zoom)>,
    area: Rect,
    gap: i32,
) -> Option<(WinId, Rect)> {
    let (win, zoom) = zoomed?;
    let rect = match zoom {
        Zoom::Fullscreen => tree.contains(win).then_some(area)?,
        Zoom::Parent => tree.parent_rect(win, area, gap)?,
    };
    Some((win, rect))
}

/// Moves the near edge of the span `start..end` by `delta`, or its far edge
/// when it is against `lo`; within `lo..hi` and no smaller than a gesture
/// leaves a window (a span already outside stays where it is on that side).
fn move_edge(start: &mut i32, end: &mut i32, lo: i32, hi: i32, delta: i32) {
    if *start <= lo {
        *end = (*end + delta).clamp((*start + MIN_GESTURE_SIZE).min(*end), hi.max(*end));
    } else {
        *start = (*start + delta).clamp(lo.min(*start), (*end - MIN_GESTURE_SIZE).max(*start));
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::geometry::{Direction, Padding};
    use crate::platform::WindowInfo;

    #[derive(Default)]
    pub struct MockBackend {
        pub windows: Vec<WindowInfo>,
        pub focused: Option<WinId>,
        pub area: Rect,
        pub focus_calls: Vec<WinId>,
        pub mouse: bool,
        pub cursor: (f32, f32),
        pub overlay: Option<Overlay>,
        pub overlay_changes: usize,
        /// The Space each window is on (new windows open on the active one).
        pub space_of: HashMap<WinId, u64>,
        pub active: u64,
        pub minimised: HashSet<WinId>,
        /// Simulates the switch animation: the screen still shows this Space.
        pub screen_space: Option<u64>,
        pub events: Vec<WindowEvent>,
        /// Windows that refuse to get smaller than this, like real apps.
        pub min: HashMap<WinId, (i32, i32)>,
        /// The gesture modifier cberry asked for.
        pub modifier: Option<Modifier>,
        /// Modifier keys held down.
        pub held: Vec<Modifier>,
        /// The button held with the modifier, reported only while the
        /// modifier is set, like the real event tap.
        pub gesture: Option<MouseButton>,
        pub warps: Vec<(f32, f32)>,
        /// The Spaces of the display, in order; empty means just `active`.
        pub spaces: Vec<u64>,
        /// An open menu, in front of every window.
        pub menu: Option<Rect>,
        /// Spotlight (or the like) is open.
        pub panel: bool,
        /// Every window a frame was written to, in order.
        pub writes: Vec<WinId>,
        /// Whether each write was a frame in the middle of an animation.
        pub in_flight: Vec<bool>,
        /// Windows shown on every Space, as "Assign to: All Desktops".
        pub sticky: HashSet<WinId>,
        /// Windows fading out as they close: still listed, not on screen.
        pub fading: HashSet<WinId>,
        /// The display's refresh rate, if known.
        pub frame_rate: Option<f64>,
        /// Displays with their Spaces; empty means one, `area` showing
        /// `active`.
        pub displays: Vec<Display>,
        /// The Spaces of each display, in order; empty means `spaces`.
        pub display_spaces: Vec<Vec<u64>>,
        /// Windows only reach another Space with the user along, by the
        /// title bar, as on recent macOS.
        pub carries: bool,
        pub border: Option<Border>,
        pub border_calls: usize,
        pub swiping: bool,
        pub server_frames: HashMap<WinId, Rect>,
    }

    impl MockBackend {
        pub fn open(&mut self, id: WinId, app: &str) {
            self.windows.push(WindowInfo {
                id,
                frame: Rect::new(100, 100, 300, 200),
                app: app.into(),
                tileable: true,
                movable: true,
            });
            self.space_of.insert(id, self.active);
            self.focused = Some(id);
        }
        pub fn close(&mut self, id: WinId) {
            self.windows.retain(|w| w.id != id);
        }
        pub fn frame(&self, id: WinId) -> Rect {
            self.windows.iter().find(|w| w.id == id).unwrap().frame
        }
    }

    impl Backend for MockBackend {
        fn windows(&mut self) -> Vec<WindowInfo> {
            let shown: Vec<u64> = if self.displays.is_empty() {
                vec![self.screen_space.unwrap_or(self.active)]
            } else {
                self.displays.iter().map(|d| d.space).collect()
            };
            self.windows
                .iter()
                .filter(|w| {
                    shown.contains(&self.space_of.get(&w.id).copied().unwrap_or(0))
                        || self.sticky.contains(&w.id)
                })
                .filter(|w| !self.fading.contains(&w.id))
                .filter(|w| !self.minimised.contains(&w.id))
                .cloned()
                .collect()
        }
        fn active_space(&mut self) -> u64 {
            self.active
        }
        fn take_events(&mut self) -> Vec<WindowEvent> {
            std::mem::take(&mut self.events)
        }
        fn alive_windows(&mut self) -> Option<HashSet<WinId>> {
            Some(self.windows.iter().map(|w| w.id).collect())
        }
        fn window_spaces(&mut self, ids: &[WinId]) -> HashMap<WinId, u64> {
            ids.iter()
                .filter(|id| self.windows.iter().any(|w| w.id == **id))
                .filter(|id| !self.minimised.contains(id))
                .map(|&id| (id, self.space_of.get(&id).copied().unwrap_or(0)))
                .collect()
        }
        fn focused_window(&mut self) -> Option<WinId> {
            self.focused
        }
        fn tiling_area(&mut self) -> Rect {
            self.area
        }
        fn displays(&mut self) -> Vec<Display> {
            if self.displays.is_empty() {
                vec![Display {
                    frame: self.area,
                    area: self.area,
                    space: self.active,
                }]
            } else {
                self.displays.clone()
            }
        }
        fn set_frame(&mut self, id: WinId, frame: Rect) {
            let (min_w, min_h) = self.min.get(&id).copied().unwrap_or((0, 0));
            self.writes.push(id);
            self.in_flight.push(false);
            if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
                w.frame = Rect::new(frame.x, frame.y, frame.w.max(min_w), frame.h.max(min_h));
            }
        }
        fn mouse_down(&mut self) -> bool {
            self.mouse
        }
        fn set_mouse_modifier(&mut self, modifier: Option<Modifier>) {
            self.modifier = modifier;
        }
        fn modifier_held(&mut self, modifier: Modifier) -> bool {
            self.held.contains(&modifier)
        }
        fn spaces(&mut self) -> Vec<u64> {
            if self.spaces.is_empty() {
                vec![self.active]
            } else {
                self.spaces.clone()
            }
        }
        fn switch_to_space(&mut self, n: usize) -> Result<(), String> {
            let space = *self.spaces().get(n - 1).ok_or("no such Space")?;
            self.active = space;
            Ok(())
        }
        fn display_spaces(&mut self, shown: u64) -> Vec<u64> {
            self.display_spaces
                .iter()
                .find(|l| l.contains(&shown))
                .cloned()
                .unwrap_or_else(|| self.spaces())
        }
        fn show_space(&mut self, space: u64) -> Result<(), String> {
            let Some(i) = self.display_spaces.iter().position(|l| l.contains(&space)) else {
                let n = self
                    .spaces()
                    .iter()
                    .position(|s| *s == space)
                    .ok_or("no such Space")?;
                return self.switch_to_space(n + 1);
            };
            self.displays[i].space = space;
            if i == 0 {
                self.active = space;
            }
            Ok(())
        }
        fn move_to_space(&mut self, id: WinId, space: u64) -> bool {
            let known = self.spaces().contains(&space)
                || self.display_spaces.iter().any(|l| l.contains(&space));
            if !known {
                return false;
            }
            self.space_of.insert(id, space);
            if self.carries {
                self.active = space;
            }
            true
        }
        fn place_on_space(&mut self, id: WinId, space: u64) -> bool {
            !self.carries && self.move_to_space(id, space)
        }
        fn warp_cursor(&mut self, x: f32, y: f32) {
            self.cursor = (x, y);
            self.warps.push((x, y));
        }
        fn covered_at(&mut self, x: f32, y: f32) -> bool {
            self.menu.is_some_and(|m| m.contains_point(x, y))
        }
        fn panel_has_focus(&mut self) -> bool {
            self.panel
        }
        fn mouse_gesture(&mut self) -> Option<MouseButton> {
            self.gesture.filter(|_| self.modifier.is_some())
        }
        fn cursor(&mut self) -> Option<(f32, f32)> {
            Some(self.cursor)
        }
        fn window_frame(&mut self, id: WinId) -> Option<Rect> {
            self.windows.iter().find(|w| w.id == id).map(|w| w.frame)
        }
        fn server_frame(&mut self, id: WinId) -> Option<Rect> {
            self.server_frames.get(&id).copied()
        }
        fn space_swiping(&mut self) -> bool {
            self.swiping
        }
        fn show_border(&mut self, border: Option<Border>) {
            self.border = border;
            self.border_calls += 1;
        }
        fn show_overlay(&mut self, overlay: Option<Overlay>) {
            self.overlay = overlay;
            self.overlay_changes += 1;
        }
        fn frame_rate(&mut self) -> Option<f64> {
            self.frame_rate
        }
        fn animate_frame(&mut self, id: WinId, frame: Rect, _prev: Rect) {
            self.set_frame(id, frame);
            *self.in_flight.last_mut().unwrap() = true;
        }
        fn focus(&mut self, id: WinId) {
            self.focus_calls.push(id);
            self.focused = Some(id);
        }
    }

    fn manager(animation_duration: f64) -> Manager<MockBackend> {
        let backend = MockBackend {
            area: Rect::new(0, 25, 1000, 600),
            ..Default::default()
        };
        let config = Config {
            gap: 10,
            padding: Padding::default(),
            animation_duration,
            ..Config::default()
        };
        Manager::new(backend, config)
    }

    fn settle(m: &mut Manager<MockBackend>) {
        for _ in 0..1000 {
            m.tick(1.0 / 60.0);
            if !m.is_animating() {
                return;
            }
        }
        panic!("animations never settled");
    }

    #[test]
    fn new_windows_are_tiled_and_animated() {
        let mut m = manager(0.42);
        m.backend.open(1, "Terminal");
        m.refresh();
        assert!(m.is_animating());
        m.tick(1.0 / 60.0);
        // Partway there after one frame.
        assert_ne!(m.backend().frame(1), Rect::new(0, 25, 1000, 600));
        settle(&mut m);
        assert_eq!(m.backend().frame(1), Rect::new(0, 25, 1000, 600));

        m.backend.open(2, "Safari");
        m.refresh();
        settle(&mut m);
        assert_eq!(m.backend().frame(1), Rect::new(0, 25, 495, 600));
        assert_eq!(m.backend().frame(2), Rect::new(505, 25, 495, 600));
    }

    #[test]
    fn closing_gives_space_back() {
        let mut m = manager(0.0);
        m.backend.open(1, "a");
        m.backend.open(2, "b");
        m.refresh();
        m.backend.close(2);
        m.refresh();
        assert_eq!(m.backend().frame(1), Rect::new(0, 25, 1000, 600));
        assert_eq!(m.tree().len(), 1);
    }

    #[test]
    fn new_window_splits_focused_one() {
        let mut m = manager(0.0);
        for id in 1..=3 {
            m.backend.open(id, "a");
            m.refresh();
        }
        // 1 | (2 / 3). Focus 1, then open 4: it must split 1, not 3.
        m.backend.focused = Some(1);
        m.refresh();
        m.backend.open(4, "a");
        m.refresh();
        assert_eq!(m.backend().frame(1).h, 295);
        assert_eq!(m.backend().frame(4).x, 0);
        assert_eq!(m.backend().frame(3), Rect::new(505, 330, 495, 295));
    }

    /// Opens `n` windows of `app`, each refusing to be smaller than `min`,
    /// letting cberry see each one settle.
    fn open_many(m: &mut Manager<MockBackend>, first: WinId, n: u32, app: &str, min: (i32, i32)) {
        for id in first..first + n {
            m.backend.open(id, app);
            m.backend.min.insert(id, min);
            for _ in 0..3 {
                m.refresh();
                settle(m);
            }
        }
    }

    /// No two tiled windows overlap, and every window is on screen.
    fn assert_tidy(m: &Manager<MockBackend>) {
        let area = Rect::new(0, 25, 1000, 600);
        let tiled = m.tree().windows();
        for w in &m.backend().windows {
            let f = w.frame;
            assert!(
                f.x >= area.x
                    && f.y >= area.y
                    && f.max_x() <= area.max_x()
                    && f.max_y() <= area.max_y(),
                "window {} off screen: {f:?}",
                w.id
            );
        }
        for (id, slot) in m.targets() {
            assert_eq!(m.backend().frame(id), slot, "window {id} fills its slot");
        }
        for (i, a) in tiled.iter().enumerate() {
            for b in &tiled[i + 1..] {
                let (fa, fb) = (m.backend().frame(*a), m.backend().frame(*b));
                let apart = fa.max_x() <= fb.x
                    || fb.max_x() <= fa.x
                    || fa.max_y() <= fb.y
                    || fb.max_y() <= fa.y;
                assert!(apart, "windows {a} and {b} overlap: {fa:?} {fb:?}");
            }
        }
    }

    #[test]
    fn new_windows_stay_in_the_part_of_the_screen_they_split() {
        let mut m = manager(0.0);
        // One window on the left, two on the right: the bottom right one
        // has a quarter of the screen.
        open_many(&mut m, 1, 1, "Safari", (0, 0));
        open_many(&mut m, 2, 1, "Notes", (0, 0));
        open_many(&mut m, 3, 1, "Notes", (0, 0));
        let quarter = m.backend().frame(3);
        assert_eq!(quarter, Rect::new(505, 330, 495, 295));
        let (safari, notes) = (m.backend().frame(1), m.backend().frame(2));
        // Terminals opened from there split only that quarter.
        for id in 4..7 {
            open_many(&mut m, id, 1, "Terminal", (200, 100));
        }
        assert_eq!(m.backend().frame(1), safari, "the left side is untouched");
        assert_eq!(m.backend().frame(2), notes, "the top right is untouched");
        let tiled = m.tree().windows();
        assert_eq!(tiled.len(), 6, "all tiled: {tiled:?}");
        // Their slots, that is: a Terminal too big for its slot sticks out.
        for (id, f) in m.slots().into_iter().filter(|(w, _)| *w > 2) {
            assert!(
                f.x >= quarter.x
                    && f.y >= quarter.y
                    && f.max_x() <= quarter.max_x()
                    && f.max_y() <= quarter.max_y(),
                "window {id} left the quarter: {f:?}"
            );
        }
    }

    #[test]
    fn a_window_rounding_its_size_up_a_little_stays_tiled() {
        let mut m = manager(0.0);
        open_many(&mut m, 1, 1, "Safari", (0, 0));
        open_many(&mut m, 2, 1, "Terminal", (0, 0));
        // Halves of 295 points: the Terminals keep 297, as a text grid
        // rounds. Less than half the 10 point gap.
        m.backend.min.insert(2, (0, 297));
        open_many(&mut m, 3, 1, "Terminal", (0, 297));
        assert!(m.tree().contains(3));
        let (two, three) = (m.backend().frame(2), m.backend().frame(3));
        assert_eq!(two.h, 297);
        assert!(two.max_y() < three.y, "a gap is left: {two:?} {three:?}");
        // And the next Terminal is not taken to be too big for a half.
        open_many(&mut m, 4, 1, "Terminal", (0, 0));
        assert!(m.tree().contains(4));
    }

    #[test]
    fn float_apps_are_skipped() {
        let mut m = manager(0.0);
        m.backend.open(1, "Calculator");
        m.refresh();
        assert!(m.tree().is_empty());
        assert_eq!(m.backend().frame(1), Rect::new(100, 100, 300, 200));
    }

    #[test]
    fn float_rules_match_app_names_with_invisible_characters() {
        // WhatsApp reports its name with a left-to-right mark in front.
        let mut m = manager(0.0);
        let mut config = Config::default();
        config.set_float_app("WhatsApp", true);
        m.set_config(config);
        m.backend.open(1, "\u{200e}WhatsApp");
        m.refresh();
        assert!(m.tree().is_empty());
    }

    #[test]
    fn regex_float_rules_float_matching_windows() {
        let mut m = manager(0.0);
        m.backend.open(1, "Webex");
        m.backend.open(2, "Safari");
        m.refresh();
        assert_eq!(m.tree().len(), 2);
        // A rule added at runtime floats the windows already tiled.
        m.execute(&Command::Rule {
            float: true,
            app: "/Webex/".into(),
        })
        .unwrap();
        assert!(!m.tree().contains(1));
        assert!(m.tree().contains(2));
        // And windows opened later.
        m.backend.open(3, "Cisco Webex Meetings");
        m.refresh();
        assert!(!m.tree().contains(3));
        let q = m.describe();
        assert!(
            q.contains("Cisco Webex Meetings\t") && q.contains("rule: /Webex/"),
            "{q}"
        );
        assert!(
            !q.lines()
                .any(|l| l.contains("Safari") && l.contains("rule:")),
            "{q}"
        );
        // Taking the rule back tiles them again.
        m.execute(&Command::Rule {
            float: false,
            app: "/Webex/".into(),
        })
        .unwrap();
        m.refresh();
        assert!(m.tree().contains(1) && m.tree().contains(3));
    }

    #[test]
    fn plain_rules_with_regex_characters_match_exactly() {
        let mut m = manager(0.0);
        let mut config = Config::default();
        config.set_float_app("Code (Insiders)", true);
        m.set_config(config);
        m.backend.open(1, "Code (Insiders)");
        m.backend.open(2, "Code Insiders");
        m.refresh();
        assert!(!m.tree().contains(1));
        assert!(m.tree().contains(2));
    }

    #[test]
    fn commands() {
        let mut m = manager(0.0);
        m.backend.open(1, "a");
        m.backend.open(2, "b");
        m.refresh();
        m.backend.focused = Some(1);

        m.execute(&Command::Focus(Direction::Right)).unwrap();
        assert_eq!(m.backend().focus_calls, vec![2]);
        assert!(m.execute(&Command::Focus(Direction::Right)).is_err());

        m.execute(&Command::Swap(Direction::Left)).unwrap();
        assert_eq!(m.backend().frame(2).x, 0);

        m.execute(&Command::Resize(Direction::Right, Some(0.1)))
            .unwrap();
        assert_eq!(m.backend().frame(2).w, 594);

        m.execute(&Command::Balance).unwrap();
        assert_eq!(m.backend().frame(2).w, 495);

        m.execute(&Command::ToggleFloat).unwrap();
        assert_eq!(m.tree().len(), 1);
        assert_eq!(m.backend().frame(1), Rect::new(0, 25, 1000, 600));
        // Floating windows stay floating across polls.
        m.refresh();
        assert_eq!(m.tree().len(), 1);
        m.backend.focused = Some(2);
        m.execute(&Command::ToggleFloat).unwrap();
        assert_eq!(m.tree().len(), 2);

        let q = m.describe();
        assert!(q.contains("1\ta\t") && q.contains("2\tb\t"), "{q}");
    }

    #[test]
    fn zoom_covers_the_screen_and_toggles_back() {
        let mut m = manager(0.0);
        for id in [1, 2, 3] {
            m.backend.open(id, "a");
            m.refresh();
        }
        let before: Vec<Rect> = [1, 2, 3].map(|id| m.backend().frame(id)).into();
        let full = m.inner_area();

        m.backend.focused = Some(3);
        m.execute(&Command::Zoom(Zoom::Fullscreen)).unwrap();
        assert_eq!(m.backend().frame(3), full);
        // The others are left alone, and the zoomed one is brought forward.
        assert_eq!(m.backend().frame(1), before[0]);
        assert_eq!(m.backend().frame(2), before[1]);
        assert_eq!(m.backend().focus_calls.last(), Some(&3));
        // Polls keep it zoomed.
        m.refresh();
        assert_eq!(m.backend().frame(3), full);
        assert!(m.describe().contains("zoomed"));

        m.execute(&Command::Zoom(Zoom::Fullscreen)).unwrap();
        assert_eq!(m.backend().frame(3), before[2]);
    }

    #[test]
    fn zoom_parent_covers_the_split() {
        let mut m = manager(0.0);
        for id in [1, 2, 3] {
            m.backend.open(id, "a");
            m.refresh();
        }
        let (two, three) = (m.backend().frame(2), m.backend().frame(3));
        m.backend.focused = Some(3);
        m.execute(&Command::Zoom(Zoom::Parent)).unwrap();
        assert_eq!(
            m.backend().frame(3),
            Rect::new(two.x, two.y, two.w, three.max_y() - two.y)
        );
        // Switching to fullscreen replaces the parent zoom.
        m.execute(&Command::Zoom(Zoom::Fullscreen)).unwrap();
        assert_eq!(m.backend().frame(3), m.inner_area());
    }

    #[test]
    fn resizing_the_zoomed_window_by_mouse_unzooms_it() {
        let mut m = manager(0.0);
        m.backend.open(1, "a");
        m.refresh();
        m.backend.open(2, "a");
        m.refresh();
        let slots = m.slots();
        m.backend.focused = Some(2);
        m.execute(&Command::Zoom(Zoom::Fullscreen)).unwrap();
        let full = m.inner_area();
        drag(&mut m, 2, Rect::new(full.x, full.y, full.w - 200, full.h));
        assert!(m.zoomed.is_none());
        assert_eq!(m.slots(), slots, "the tree must not change");
        assert_eq!(m.backend().frame(2), slots[1].1);
    }

    #[test]
    fn a_click_focuses_even_when_the_system_lags() {
        let mut m = manager(0.0);
        m.backend.open(1, "a");
        m.refresh();
        m.backend.open(2, "a");
        m.refresh();
        m.backend.focused = Some(1);
        m.refresh();
        // Click window 2; the system keeps saying 1 has focus.
        m.backend.cursor = m.backend().frame(2).center();
        m.backend.mouse = true;
        m.track_pointer();
        m.backend.mouse = false;
        m.track_pointer();
        m.refresh();
        m.execute(&Command::Zoom(Zoom::Fullscreen)).unwrap();
        assert_eq!(m.backend().frame(2), m.inner_area());
        assert_ne!(m.backend().frame(1), m.inner_area());
        // A real change reported by the system still counts.
        m.execute(&Command::Zoom(Zoom::Fullscreen)).unwrap();
        m.backend.focused = None;
        m.refresh();
        m.backend.focused = Some(1);
        m.execute(&Command::Zoom(Zoom::Fullscreen)).unwrap();
        assert_eq!(m.backend().frame(1), m.inner_area());
    }

    #[test]
    fn a_click_on_a_zoomed_window_focuses_it_not_the_one_behind() {
        let mut m = manager(0.0);
        m.backend.open(1, "a");
        m.refresh();
        m.backend.open(2, "a");
        m.refresh();
        m.backend.focused = Some(2);
        m.execute(&Command::Zoom(Zoom::Fullscreen)).unwrap();
        // Over window 1's slot, but window 2 covers it.
        m.backend.cursor = m.slots()[0].1.center();
        m.backend.mouse = true;
        m.track_pointer();
        m.backend.mouse = false;
        m.track_pointer();
        m.execute(&Command::Zoom(Zoom::Fullscreen)).unwrap();
        assert!(m.zoomed.is_none(), "the zoom toggles off for window 2");
    }

    #[test]
    fn closing_the_zoomed_window_ends_the_zoom() {
        let mut m = manager(0.0);
        m.backend.open(1, "a");
        m.refresh();
        m.backend.open(2, "a");
        m.refresh();
        m.backend.focused = Some(2);
        m.execute(&Command::Zoom(Zoom::Fullscreen)).unwrap();
        m.backend.close(2);
        m.refresh();
        assert_eq!(m.backend().frame(1), m.inner_area());
        assert!(m.zoomed.is_none());
        // A lone window has no split to zoom into.
        m.backend.focused = Some(1);
        assert!(m.execute(&Command::Zoom(Zoom::Parent)).is_err());
    }

    #[test]
    fn retarget_during_animation_is_smooth() {
        let mut m = manager(0.42);
        m.backend.open(1, "a");
        m.refresh();
        settle(&mut m);
        m.backend.open(2, "b");
        m.refresh();
        for _ in 0..3 {
            m.tick(1.0 / 60.0);
        }
        let mid = m.backend().frame(1);
        m.backend.close(2);
        m.refresh();
        m.tick(1.0 / 60.0);
        let next = m.backend().frame(1);
        // It brakes first, then turns: no jump either way.
        assert!((next.w - mid.w).abs() < 60, "{mid:?} -> {next:?}");
        for _ in 0..12 {
            m.tick(1.0 / 60.0);
        }
        assert!(m.backend().frame(1).w > mid.w, "turned around");
        settle(&mut m);
        assert_eq!(m.backend().frame(1), Rect::new(0, 25, 1000, 600));
    }

    /// Simulates the user dragging window `id` to `frame` with the mouse,
    /// grabbing it at its centre and letting go at the centre of `frame`.
    fn drag(m: &mut Manager<MockBackend>, id: WinId, frame: Rect) {
        m.backend.cursor = m.backend.frame(id).center();
        m.backend.mouse = true;
        m.track_pointer();
        m.backend.set_frame(id, frame);
        m.backend.cursor = frame.center();
        m.track_pointer();
        m.refresh(); // a poll mid-drag must not fight the user
        m.track_pointer();
        assert_eq!(m.backend().frame(id), frame);
        m.backend.mouse = false;
        m.track_pointer();
        settle(m);
    }

    /// Three windows: Terminal (1) over Finder (3) on the left, Claude (2) on the right.
    fn three_windows() -> Manager<MockBackend> {
        let mut m = manager(0.42);
        m.backend.open(1, "Terminal");
        m.backend.open(2, "Claude");
        m.refresh();
        settle(&mut m);
        m.backend.focused = Some(1);
        m.refresh();
        m.backend.open(3, "Finder");
        m.refresh();
        settle(&mut m);
        m
    }

    #[test]
    fn dragging_onto_a_window_swaps_with_it() {
        let mut m = three_windows();
        let (claude, finder) = (m.backend().frame(2), m.backend().frame(3));
        assert_eq!(finder, Rect::new(0, 330, 495, 295));

        // Grab Claude by its title bar and carry it over Finder.
        m.backend.cursor = (700.0, 35.0);
        m.backend.mouse = true;
        m.track_pointer();
        m.backend.set_frame(2, Rect::new(300, 300, 495, 600));
        m.backend.cursor = (200.0, 400.0);
        m.track_pointer();
        // Finder is highlighted, in its slot, with the configured colour.
        let overlay = m.backend().overlay.expect("highlight over Finder");
        assert_eq!(overlay.rect, finder);
        assert_eq!(overlay.above, 3);
        assert_eq!(overlay.color, m.config().drag_color);

        // Moving within the same target does not redraw the highlight.
        let changes = m.backend().overlay_changes;
        m.backend.cursor = (210.0, 410.0);
        m.track_pointer();
        assert_eq!(m.backend().overlay_changes, changes);

        // Over Terminal instead.
        m.backend.cursor = (200.0, 100.0);
        m.track_pointer();
        assert_eq!(m.backend().overlay.unwrap().above, 1);
        m.backend.cursor = (200.0, 400.0);
        m.track_pointer();

        // Drop: the highlight goes away and the two swap, animated.
        m.backend.mouse = false;
        m.track_pointer();
        assert_eq!(m.backend().overlay, None);
        assert!(m.is_animating());
        m.tick(1.0 / 60.0);
        let mid = m.backend().frame(2);
        assert!(
            mid.x < 300 && mid.x > 0,
            "slides from the drop point: {mid:?}"
        );
        settle(&mut m);
        assert_eq!(m.backend().frame(2), finder);
        assert_eq!(m.backend().frame(3), claude);
        assert_eq!(m.backend().frame(1), Rect::new(0, 25, 495, 295));
    }

    #[test]
    fn dropping_on_its_own_slot_returns_the_window() {
        let mut m = three_windows();
        let before = m.tree().clone();
        let claude = m.backend().frame(2);
        m.backend.cursor = (700.0, 35.0);
        m.backend.mouse = true;
        m.track_pointer();
        m.backend.set_frame(2, Rect::new(540, 60, 495, 600));
        m.backend.cursor = (740.0, 70.0);
        m.track_pointer();
        assert_eq!(m.backend().overlay, None);
        m.backend.mouse = false;
        m.track_pointer();
        settle(&mut m);
        assert_eq!(m.backend().frame(2), claude);
        assert_eq!(m.tree(), &before);
    }

    #[test]
    fn drag_swap_can_be_turned_off() {
        let mut m = three_windows();
        m.execute(&Command::parse("config drag_swap off").unwrap())
            .unwrap();
        let claude = m.backend().frame(2);
        m.backend.cursor = (700.0, 35.0);
        m.backend.mouse = true;
        m.track_pointer();
        m.backend.set_frame(2, Rect::new(300, 300, 495, 600));
        m.backend.cursor = (200.0, 400.0);
        m.track_pointer();
        assert_eq!(m.backend().overlay_changes, 0);
        m.backend.mouse = false;
        m.track_pointer();
        settle(&mut m);
        assert_eq!(m.backend().frame(2), claude, "snaps back, no swap");
    }

    #[test]
    fn resizing_never_shows_the_highlight() {
        let mut m = three_windows();
        // Drag Claude's left edge over Finder's area.
        drag(&mut m, 2, Rect::new(300, 25, 700, 600));
        assert_eq!(m.backend().overlay_changes, 0);
        assert_eq!(m.backend().frame(3).w, 290);
    }

    #[test]
    fn mouse_resize_moves_the_border() {
        let mut m = manager(0.42);
        m.backend.open(1, "a");
        m.backend.open(2, "b");
        m.refresh();
        settle(&mut m);
        // Drag window 1's right edge from 495 to 700.
        drag(&mut m, 1, Rect::new(0, 25, 700, 600));
        assert_eq!(m.backend().frame(1), Rect::new(0, 25, 700, 600));
        assert_eq!(m.backend().frame(2), Rect::new(710, 25, 290, 600));
        // Drag window 2's left edge back to 400.
        drag(&mut m, 2, Rect::new(400, 25, 600, 600));
        assert_eq!(m.backend().frame(1).w, 390);
        assert_eq!(m.backend().frame(2), Rect::new(400, 25, 600, 600));
    }

    #[test]
    fn mouse_resize_with_a_size_rounding_neighbour() {
        // Three windows: 1 on the left, 2 and 3 stacked on the right.
        let mut m = manager(0.42);
        for id in 1..=3 {
            m.backend.open(id, "a");
            m.refresh();
            settle(&mut m);
        }
        // Window 1 is a terminal that rounds its size to whole text rows,
        // so it never exactly matches its slot.
        m.backend.set_frame(1, Rect::new(0, 25, 495, 588));
        m.refresh();
        // The user drags the left edge of window 3 to x = 600.
        drag(&mut m, 3, Rect::new(600, 330, 400, 295));
        // The border between the columns follows the drag...
        assert_eq!(m.backend().frame(3).x, 600);
        assert_eq!(m.backend().frame(2).x, 600);
        assert_eq!(m.backend().frame(1).w, 590);
        // ...and the drag does not move the row border, which was not touched.
        assert_eq!(m.backend().frame(2).h, 295);
    }

    #[test]
    fn mouse_resize_single_window_next_to_a_stack() {
        // Two windows stacked on the left (1 over 3), one on the right (2).
        let mut m = manager(0.42);
        m.backend.open(1, "a");
        m.backend.open(2, "b");
        m.refresh();
        settle(&mut m);
        m.backend.focused = Some(1);
        m.refresh();
        m.backend.open(3, "c");
        m.refresh();
        settle(&mut m);
        assert_eq!(m.backend().frame(3), Rect::new(0, 330, 495, 295));

        // Shrink the right window by dragging its left edge to x = 700...
        drag(&mut m, 2, Rect::new(700, 25, 300, 600));
        assert_eq!(m.backend().frame(2), Rect::new(700, 25, 300, 600));
        assert_eq!(m.backend().frame(1).w, 690);
        assert_eq!(m.backend().frame(3).w, 690);
        // ...then grow it back to x = 300.
        drag(&mut m, 2, Rect::new(300, 25, 700, 600));
        assert_eq!(m.backend().frame(2), Rect::new(300, 25, 700, 600));
        assert_eq!(m.backend().frame(1).w, 290);
        assert_eq!(m.backend().frame(3).w, 290);
        // The stacked windows keep their heights.
        assert_eq!(m.backend().frame(1).h, 295);
    }

    #[test]
    fn mouse_move_snaps_back() {
        let mut m = manager(0.42);
        m.backend.open(1, "a");
        m.backend.open(2, "b");
        m.refresh();
        settle(&mut m);
        drag(&mut m, 1, Rect::new(200, 200, 495, 600));
        assert_eq!(m.backend().frame(1), Rect::new(0, 25, 495, 600));
    }

    /// Holds `button` with the modifier at `from`, drags through `path`,
    /// then lets go.
    fn gesture(
        m: &mut Manager<MockBackend>,
        button: MouseButton,
        from: (f32, f32),
        path: &[(f32, f32)],
    ) {
        m.backend.cursor = from;
        m.backend.gesture = Some(button);
        m.backend.mouse = button == MouseButton::Left;
        m.track_pointer();
        for &p in path {
            m.backend.cursor = p;
            m.track_pointer();
        }
        m.backend.gesture = None;
        m.backend.mouse = false;
        m.track_pointer();
    }

    /// One window on the left, two stacked on the right, with
    /// `mouse_insert_modifier cmd`.
    fn left_one_right_two() -> Manager<MockBackend> {
        let mut m = manager(0.0);
        let mut config = m.config().clone();
        config.mouse_insert_modifier = Some(Modifier::Cmd);
        m.set_config(config);
        m.backend.open(1, "Terminal");
        m.refresh();
        m.backend.open(2, "Safari");
        m.refresh();
        m.backend.focused = Some(2);
        m.backend.open(3, "Notes");
        m.refresh();
        m.track_pointer();
        assert_eq!(m.backend().frame(1), Rect::new(0, 25, 495, 600));
        assert_eq!(m.backend().frame(2), Rect::new(505, 25, 495, 295));
        assert_eq!(m.backend().frame(3), Rect::new(505, 330, 495, 295));
        m
    }

    #[test]
    fn insert_drag_splits_the_window_it_is_dropped_on() {
        let mut m = left_one_right_two();
        m.backend.held = vec![Modifier::Cmd];
        // Grab the top right window, drop it on the bottom half of the left one.
        m.backend.cursor = (700.0, 150.0);
        m.backend.gesture = Some(MouseButton::Left);
        m.backend.mouse = true;
        m.track_pointer();
        m.backend.cursor = (200.0, 520.0);
        m.track_pointer();
        let overlay = m.backend().overlay.unwrap();
        assert_eq!(overlay.above, 1);
        assert_eq!(overlay.rect, Rect::new(0, 325, 495, 300), "the bottom half");
        m.backend.gesture = None;
        m.backend.mouse = false;
        m.track_pointer();
        assert_eq!(m.backend().overlay, None);
        assert_eq!(m.backend().frame(1), Rect::new(0, 25, 495, 295));
        assert_eq!(m.backend().frame(2), Rect::new(0, 330, 495, 295));
        assert_eq!(m.backend().frame(3), Rect::new(505, 25, 495, 600));
    }

    #[test]
    fn highlight_covers_a_window_that_did_not_fill_its_slot() {
        let mut m = left_one_right_two();
        // Like Terminal rounding its width to whole characters.
        let terminal = Rect::new(0, 25, 481, 593);
        m.backend.set_frame(1, terminal);
        m.refresh();
        assert_eq!(m.backend().frame(1), terminal);

        // Moved by its title bar.
        m.backend.cursor = (700.0, 35.0);
        m.backend.mouse = true;
        m.track_pointer();
        m.backend.set_frame(2, Rect::new(150, 300, 495, 295));
        m.backend.cursor = (200.0, 400.0);
        m.track_pointer();
        assert_eq!(m.backend().overlay.unwrap().rect, terminal);
        // Off its right edge, in what is left of its slot: no target.
        m.backend.cursor = (488.0, 400.0);
        m.track_pointer();
        assert_eq!(m.backend().overlay, None);
        m.backend.mouse = false;
        m.track_pointer();
        settle(&mut m);
        // Laid out again, the app rounds its size again.
        m.backend.set_frame(1, terminal);
        m.refresh();

        // Moved with the modifier, swapping.
        m.backend.cursor = (700.0, 150.0);
        m.backend.gesture = Some(MouseButton::Left);
        m.backend.mouse = true;
        m.track_pointer();
        m.backend.cursor = (200.0, 400.0);
        m.track_pointer();
        assert_eq!(m.backend().overlay.unwrap().rect, terminal);
        // And splitting it: the bottom half of where it really is.
        m.backend.held = vec![Modifier::Cmd];
        m.backend.cursor = (200.0, 520.0);
        m.track_pointer();
        assert_eq!(
            m.backend().overlay.unwrap().rect,
            Rect::new(0, 321, 481, 297)
        );
        m.backend.gesture = None;
        m.backend.mouse = false;
        m.track_pointer();
        assert_eq!(m.backend().overlay, None);
    }

    #[test]
    fn highlight_follows_a_window_that_changes_under_it() {
        let mut m = left_one_right_two();
        m.backend.cursor = (700.0, 150.0);
        m.backend.gesture = Some(MouseButton::Left);
        m.backend.mouse = true;
        m.track_pointer();
        m.backend.cursor = (200.0, 400.0);
        m.track_pointer();
        assert_eq!(
            m.backend().overlay.unwrap().rect,
            Rect::new(0, 25, 495, 600)
        );
        let changes = m.backend().overlay_changes;
        // Holding still: nothing is redrawn.
        m.track_pointer();
        assert_eq!(m.backend().overlay_changes, changes);
        // The app settles a little narrower while the pointer rests on it.
        m.backend.set_frame(1, Rect::new(0, 25, 481, 593));
        m.refresh();
        m.track_pointer();
        assert_eq!(
            m.backend().overlay.unwrap().rect,
            Rect::new(0, 25, 481, 593)
        );
        m.backend.gesture = None;
        m.backend.mouse = false;
        m.track_pointer();
    }

    #[test]
    fn warp_moves_a_window_into_its_neighbours_place() {
        let mut m = left_one_right_two();
        // The bottom right window goes left: the tall left window splits
        // into top and bottom, and the window takes the bottom, where it
        // came from. The top right one takes the whole right side.
        m.backend.focused = Some(3);
        m.execute(&Command::Warp(Direction::Left)).unwrap();
        assert_eq!(m.backend().frame(1), Rect::new(0, 25, 495, 295));
        assert_eq!(m.backend().frame(3), Rect::new(0, 330, 495, 295));
        assert_eq!(m.backend().frame(2), Rect::new(505, 25, 495, 600));
        // Nothing further left.
        assert!(m.execute(&Command::Warp(Direction::Left)).is_err());
        // And back.
        m.execute(&Command::Warp(Direction::Right)).unwrap();
        assert_eq!(m.backend().frame(1), Rect::new(0, 25, 495, 600));
        assert_eq!(m.backend().frame(2), Rect::new(505, 25, 495, 295));
        assert_eq!(m.backend().frame(3), Rect::new(505, 330, 495, 295));
    }

    #[test]
    fn warp_the_bottom_left_window_right() {
        // Two on the left, one on the right.
        let mut m = manager(0.0);
        open_many(&mut m, 1, 1, "Safari", (0, 0));
        open_many(&mut m, 2, 1, "Notes", (0, 0));
        m.backend.focused = Some(1);
        m.refresh();
        open_many(&mut m, 3, 1, "Terminal", (0, 0));
        assert_eq!(m.backend().frame(3), Rect::new(0, 330, 495, 295));
        m.backend.focused = Some(3);
        m.execute(&Command::Warp(Direction::Right)).unwrap();
        // The right window splits top and bottom; nothing side by side.
        assert_eq!(m.backend().frame(1), Rect::new(0, 25, 495, 600));
        assert_eq!(m.backend().frame(2), Rect::new(505, 25, 495, 295));
        assert_eq!(m.backend().frame(3), Rect::new(505, 330, 495, 295));
    }

    #[test]
    fn balance_undoes_resizes_both_ways() {
        let mut m = left_one_right_two();
        let fresh: Vec<Rect> = (1..=3).map(|id| m.backend().frame(id)).collect();
        // The bottom right window made wider and taller.
        m.backend.focused = Some(3);
        m.execute(&Command::Resize(Direction::Left, Some(0.2)))
            .unwrap();
        m.execute(&Command::Resize(Direction::Up, Some(0.2)))
            .unwrap();
        assert_ne!(m.backend().frame(3), fresh[2]);
        m.execute(&Command::Balance).unwrap();
        let now: Vec<Rect> = (1..=3).map(|id| m.backend().frame(id)).collect();
        assert_eq!(now, fresh);
        // `balance area` gives the left window a third instead.
        m.execute(&Command::BalanceArea).unwrap();
        assert_eq!(m.backend().frame(1).w, 330);
    }

    #[test]
    fn moving_a_window_just_opened_does_not_float_it() {
        // Two on the left, one on the right; the last one just opened.
        let mut m = manager(0.0);
        open_many(&mut m, 1, 1, "Safari", (0, 0));
        open_many(&mut m, 2, 1, "Notes", (0, 0));
        m.backend.focused = Some(1);
        m.refresh();
        open_many(&mut m, 3, 1, "Terminal", (0, 0));
        assert_eq!(m.tree().len(), 3);
        // Swapped, warped and resized right away: all stay tiled.
        m.backend.focused = Some(3);
        m.execute(&Command::Swap(Direction::Right)).unwrap();
        m.refresh();
        m.execute(&Command::Warp(Direction::Left)).unwrap();
        m.refresh();
        m.execute(&Command::Resize(Direction::Right, Some(0.1)))
            .unwrap();
        m.refresh();
        settle(&mut m);
        assert_eq!(m.tree().len(), 3, "{:?}", m.tree().windows());
        assert_tidy(&m);
    }

    #[test]
    fn warp_down_onto_the_window_below() {
        let mut m = left_one_right_two();
        m.backend.focused = Some(2);
        m.execute(&Command::Warp(Direction::Down)).unwrap();
        assert_eq!(m.backend().frame(3), Rect::new(505, 25, 495, 295));
        assert_eq!(m.backend().frame(2), Rect::new(505, 330, 495, 295));
        assert_eq!(m.backend().frame(1), Rect::new(0, 25, 495, 600));
    }

    #[test]
    fn insert_drag_onto_the_top_half_goes_above() {
        let mut m = left_one_right_two();
        m.backend.held = vec![Modifier::Cmd];
        gesture(&mut m, MouseButton::Left, (700.0, 150.0), &[(200.0, 100.0)]);
        assert_eq!(m.backend().frame(2), Rect::new(0, 25, 495, 295));
        assert_eq!(m.backend().frame(1), Rect::new(0, 330, 495, 295));
        assert_eq!(m.backend().frame(3), Rect::new(505, 25, 495, 600));
    }

    #[test]
    fn insert_drag_onto_a_side_goes_beside() {
        let mut m = left_one_right_two();
        m.backend.held = vec![Modifier::Cmd];
        // The bottom right window onto the left edge of the left one.
        gesture(&mut m, MouseButton::Left, (700.0, 500.0), &[(30.0, 325.0)]);
        assert_eq!(m.tree().windows(), vec![3, 1, 2]);
        assert_eq!(m.backend().frame(3).x, 0);
        assert_eq!(m.backend().frame(2), Rect::new(505, 25, 495, 600));
    }

    #[test]
    fn drag_without_the_insert_modifier_still_swaps() {
        let mut m = left_one_right_two();
        gesture(&mut m, MouseButton::Left, (700.0, 150.0), &[(200.0, 520.0)]);
        assert_eq!(m.backend().frame(2), Rect::new(0, 25, 495, 600));
        assert_eq!(m.backend().frame(1), Rect::new(505, 25, 495, 295));
    }

    #[test]
    fn modifier_drag_moves_a_tiled_window_from_anywhere_and_swaps() {
        let mut m = manager(0.0);
        m.backend.open(1, "Terminal");
        m.backend.open(2, "Safari");
        m.refresh();
        m.track_pointer();
        assert_eq!(m.backend().modifier, Some(Modifier::Alt));

        m.backend.cursor = (100.0, 300.0); // the middle of the window
        m.backend.gesture = Some(MouseButton::Left);
        m.backend.mouse = true;
        m.track_pointer();
        assert_eq!(m.backend().focus_calls.last(), Some(&1));
        m.backend.cursor = (150.0, 320.0);
        m.track_pointer();
        assert_eq!(
            m.backend().frame(1),
            Rect::new(50, 45, 495, 600),
            "follows the pointer"
        );
        assert_eq!(m.backend().overlay, None, "over its own slot");
        m.backend.cursor = (700.0, 300.0);
        m.track_pointer();
        assert_eq!(m.backend().frame(1).x, 600);
        assert_eq!(m.backend().overlay.map(|o| o.above), Some(2));
        // A poll in the middle must not pull the window back.
        m.refresh();
        assert_eq!(m.backend().frame(1).x, 600);

        m.backend.gesture = None;
        m.backend.mouse = false;
        m.track_pointer();
        assert_eq!(m.tree().windows(), vec![2, 1]);
        assert_eq!(m.backend().overlay, None);
        assert_eq!(m.backend().frame(1), Rect::new(505, 25, 495, 600));
        assert_eq!(m.backend().frame(2), Rect::new(0, 25, 495, 600));
    }

    #[test]
    fn modifier_drag_dropped_nowhere_returns_the_window() {
        let mut m = manager(0.42);
        m.backend.open(1, "Terminal");
        m.backend.open(2, "Safari");
        m.refresh();
        settle(&mut m);
        gesture(&mut m, MouseButton::Left, (100.0, 300.0), &[(130.0, 330.0)]);
        assert!(m.is_animating(), "slides back");
        settle(&mut m);
        assert_eq!(m.tree().windows(), vec![1, 2]);
        assert_eq!(m.backend().frame(1), Rect::new(0, 25, 495, 600));
    }

    #[test]
    fn modifier_right_drag_resizes_from_the_nearest_edge() {
        let mut m = manager(0.0);
        m.backend.open(1, "Terminal");
        m.backend.open(2, "Safari");
        m.refresh();
        // Grabbed in its right half: the right edge follows.
        gesture(
            &mut m,
            MouseButton::Right,
            (400.0, 500.0),
            &[(450.0, 500.0), (500.0, 500.0)],
        );
        assert_eq!(m.backend().frame(1), Rect::new(0, 25, 595, 600));
        assert_eq!(m.backend().frame(2), Rect::new(605, 25, 395, 600));
        // Window 2 grabbed in its left half moves the same border back.
        gesture(
            &mut m,
            MouseButton::Right,
            (650.0, 300.0),
            &[(550.0, 300.0)],
        );
        assert_eq!(m.backend().frame(1), Rect::new(0, 25, 495, 600));
        assert_eq!(m.backend().frame(2), Rect::new(505, 25, 495, 600));
        assert_eq!(
            m.backend().overlay_changes,
            0,
            "a resize shows no highlight"
        );
    }

    #[test]
    fn modifier_resize_follows_the_pointer_without_lag() {
        let mut m = manager(0.42);
        m.backend.open(1, "Terminal");
        m.backend.open(2, "Safari");
        m.refresh();
        settle(&mut m);
        m.backend.cursor = (400.0, 500.0);
        m.backend.gesture = Some(MouseButton::Right);
        m.track_pointer();
        m.backend.cursor = (500.0, 500.0);
        m.track_pointer();
        assert!(!m.is_animating(), "no easing while the border is held");
        assert_eq!(m.backend().frame(1), Rect::new(0, 25, 595, 600));
        assert_eq!(m.backend().frame(2), Rect::new(605, 25, 395, 600));
        m.backend.gesture = None;
        m.track_pointer();
    }

    #[test]
    fn modifier_drags_move_and_resize_floating_windows_freely() {
        let mut m = manager(0.0);
        m.backend.open(2, "Calculator"); // floats; in front of the terminal
        m.backend.open(1, "Terminal");
        m.refresh();
        assert_eq!(m.tree().windows(), vec![1]);
        gesture(&mut m, MouseButton::Left, (150.0, 150.0), &[(250.0, 200.0)]);
        assert_eq!(m.backend().frame(2), Rect::new(200, 150, 300, 200));
        assert_eq!(
            m.backend().frame(1),
            Rect::new(0, 25, 1000, 600),
            "untouched"
        );
        assert_eq!(m.backend().overlay_changes, 0);
        m.refresh();
        gesture(
            &mut m,
            MouseButton::Right,
            (490.0, 340.0),
            &[(540.0, 370.0)],
        );
        assert_eq!(m.backend().frame(2), Rect::new(200, 150, 350, 230));
        // Grabbed top-left, and never smaller than the minimum.
        m.refresh();
        gesture(
            &mut m,
            MouseButton::Right,
            (210.0, 160.0),
            &[(900.0, 900.0)],
        );
        assert_eq!(m.backend().frame(2), Rect::new(500, 330, 50, 50));
    }

    #[test]
    fn modifier_click_on_a_window_still_animating_does_not_swap() {
        let mut m = manager(0.42);
        m.backend.open(1, "Terminal");
        m.backend.open(2, "Safari");
        m.refresh();
        settle(&mut m);
        m.execute(&Command::Balance).unwrap();
        m.backend.focused = Some(1);
        m.refresh();
        m.execute(&Command::Swap(Direction::Right)).unwrap();
        m.tick(1.0 / 60.0);
        // Window 1 is on its way right; the pointer is over the slot it
        // left, which is window 2's now.
        let f = m.backend().frame(1);
        let at = (f.x as f32 + 10.0, 300.0);
        gesture(&mut m, MouseButton::Left, at, &[(at.0 + 2.0, 301.0)]);
        assert_eq!(m.tree().windows(), vec![2, 1], "no second swap");
        settle(&mut m);
        assert_eq!(m.backend().frame(1), Rect::new(505, 25, 495, 600));
    }

    #[test]
    fn modifier_click_focuses_without_moving() {
        let mut m = manager(0.0);
        m.backend.open(1, "Terminal");
        m.backend.open(2, "Safari");
        m.refresh();
        gesture(&mut m, MouseButton::Left, (700.0, 300.0), &[]);
        assert_eq!(m.backend().focus_calls.last(), Some(&2));
        assert_eq!(m.backend().frame(1), Rect::new(0, 25, 495, 600));
        assert_eq!(m.backend().frame(2), Rect::new(505, 25, 495, 600));
    }

    #[test]
    fn gesture_wins_over_a_click_seen_a_frame_earlier() {
        let mut m = manager(0.0);
        m.backend.open(1, "Terminal");
        m.backend.open(2, "Safari");
        m.refresh();
        m.track_pointer();
        // The button state arrives a frame before the gesture does.
        m.backend.cursor = (100.0, 300.0);
        m.backend.mouse = true;
        m.track_pointer();
        gesture(&mut m, MouseButton::Left, (100.0, 300.0), &[(700.0, 300.0)]);
        assert_eq!(m.tree().windows(), vec![2, 1]);
    }

    #[test]
    fn mouse_modifier_off_disables_gestures() {
        let mut m = manager(0.0);
        m.backend.open(1, "Terminal");
        m.backend.open(2, "Safari");
        m.refresh();
        m.execute(&Command::parse("config mouse_modifier off").unwrap())
            .unwrap();
        m.track_pointer();
        assert_eq!(m.backend().modifier, None);
        gesture(&mut m, MouseButton::Left, (100.0, 300.0), &[(700.0, 300.0)]);
        assert_eq!(m.tree().windows(), vec![1, 2]);
        assert_eq!(m.backend().frame(1), Rect::new(0, 25, 495, 600));
    }

    #[test]
    fn closing_the_grabbed_window_ends_the_gesture() {
        let mut m = manager(0.0);
        m.backend.open(1, "Terminal");
        m.backend.open(2, "Safari");
        m.refresh();
        m.backend.cursor = (100.0, 300.0);
        m.backend.gesture = Some(MouseButton::Left);
        m.track_pointer();
        m.backend.cursor = (700.0, 300.0);
        m.track_pointer();
        assert!(m.backend().overlay.is_some());
        m.backend.close(1);
        m.refresh();
        m.track_pointer();
        assert_eq!(m.backend().overlay, None);
        assert_eq!(m.tree().windows(), vec![2]);
        m.backend.gesture = None;
        m.track_pointer();
        assert_eq!(m.backend().frame(2), Rect::new(0, 25, 1000, 600));
    }

    fn ffm() -> Manager<MockBackend> {
        let mut m = manager(0.0);
        m.config.focus_follows_mouse = true;
        m
    }

    /// Moves the pointer to `to` and lets it rest there for `secs`.
    fn hover(m: &mut Manager<MockBackend>, to: (f32, f32), secs: f64) {
        m.backend.cursor = to;
        m.track_pointer();
        let mut t = 0.0;
        while t < secs {
            m.tick(0.02);
            m.track_pointer();
            t += 0.02;
        }
    }

    #[test]
    fn mouse_follows_focus_warps_to_the_focused_window() {
        let mut m = manager(0.0);
        m.backend.open(1, "Terminal");
        m.backend.open(2, "Safari");
        m.refresh();
        m.backend.focused = Some(1);
        m.refresh();
        m.backend.cursor = (100.0, 300.0);
        m.execute(&Command::Focus(Direction::Right)).unwrap();
        assert!(m.backend().warps.is_empty(), "off by default");

        m.execute(&Command::parse("config mouse_follows_focus on").unwrap())
            .unwrap();
        m.backend.cursor = (700.0, 300.0);
        m.execute(&Command::Focus(Direction::Left)).unwrap();
        assert_eq!(m.backend().warps, vec![(247.5, 325.0)]);
        // Swapping takes the pointer along with the window.
        m.execute(&Command::Swap(Direction::Right)).unwrap();
        assert_eq!(m.backend().warps.last(), Some(&(752.5, 325.0)));
        // Already on the window: the pointer stays put.
        m.backend.cursor = (100.0, 100.0);
        m.execute(&Command::Focus(Direction::Left)).unwrap();
        assert_eq!(m.backend().warps.len(), 2);
    }

    #[test]
    fn focus_follows_mouse_after_a_short_rest() {
        let mut m = ffm();
        m.backend.open(1, "Terminal");
        m.backend.open(2, "Safari");
        m.refresh();
        m.backend.focused = Some(1);
        m.refresh();
        hover(&mut m, (100.0, 300.0), 0.2);
        assert!(m.backend().focus_calls.is_empty(), "already focused");
        hover(&mut m, (700.0, 300.0), 0.05);
        assert!(m.backend().focus_calls.is_empty(), "only passing by");
        hover(&mut m, (710.0, 300.0), 0.1);
        assert_eq!(m.backend().focus_calls, vec![2]);
        hover(&mut m, (720.0, 310.0), 0.3);
        assert_eq!(m.backend().focus_calls, vec![2], "once per visit");
    }

    #[test]
    fn focus_follows_mouse_ignores_windows_moving_under_a_still_pointer() {
        let mut m = ffm();
        m.backend.open(1, "Terminal");
        m.backend.open(2, "Safari");
        m.refresh();
        hover(&mut m, (700.0, 300.0), 0.2);
        m.backend.focus_calls.clear();
        m.backend.focused = Some(2);
        m.refresh();
        m.execute(&Command::Swap(Direction::Left)).unwrap();
        m.refresh();
        hover(&mut m, (700.0, 300.0), 0.3);
        assert!(m.backend().focus_calls.is_empty());
    }

    #[test]
    fn focus_follows_mouse_keeps_floating_windows_and_menus_visible() {
        let mut m = ffm();
        m.backend.open(3, "Calculator"); // floating, in front
        m.backend.open(1, "Terminal");
        m.backend.open(2, "Safari");
        m.refresh();
        m.backend.focused = Some(3);
        m.refresh();
        // Window 1 lies partly under the calculator: raising it would hide it.
        hover(&mut m, (450.0, 500.0), 0.3);
        assert!(m.backend().focus_calls.is_empty());
        // A menu open over window 2.
        m.backend.menu = Some(Rect::new(600, 200, 200, 300));
        hover(&mut m, (700.0, 300.0), 0.3);
        assert!(m.backend().focus_calls.is_empty());
        m.backend.menu = None;
        hover(&mut m, (650.0, 550.0), 0.3);
        assert!(m.backend().focus_calls.is_empty(), "same visit");
        hover(&mut m, (100.0, 150.0), 0.3); // the calculator
        hover(&mut m, (700.0, 300.0), 0.3);
        assert_eq!(m.backend().focus_calls.last(), Some(&2));
    }

    #[test]
    fn focus_follows_mouse_looks_through_helper_windows() {
        let mut m = ffm();
        // An invisible window some tool keeps over everything.
        m.backend.windows.push(WindowInfo {
            id: 50,
            frame: Rect::new(0, 0, 1000, 700),
            app: "borders".into(),
            tileable: false,
            movable: false,
        });
        m.backend.open(1, "Terminal");
        m.backend.open(2, "Safari");
        m.refresh();
        m.backend.focused = Some(1);
        m.refresh();
        assert_eq!(m.tree().windows(), vec![1, 2]);
        hover(&mut m, (100.0, 300.0), 0.1);
        hover(&mut m, (700.0, 300.0), 0.3);
        assert_eq!(m.backend().focus_calls, vec![2]);
    }

    #[test]
    fn focus_follows_mouse_leaves_spotlight_open() {
        let mut m = ffm();
        m.backend.open(1, "Terminal");
        m.backend.open(2, "Safari");
        m.refresh();
        m.backend.focused = Some(1);
        m.refresh();
        hover(&mut m, (100.0, 300.0), 0.1);
        m.backend.panel = true;
        hover(&mut m, (700.0, 300.0), 0.3);
        assert!(m.backend().focus_calls.is_empty());
        // Closed: the next window the pointer goes to gets focus again.
        m.backend.panel = false;
        hover(&mut m, (100.0, 300.0), 0.3);
        hover(&mut m, (700.0, 300.0), 0.3);
        assert_eq!(m.backend().focus_calls, vec![2]);
    }

    #[test]
    fn focus_follows_mouse_waits_while_a_button_is_down() {
        let mut m = ffm();
        m.backend.open(1, "Terminal");
        m.backend.open(2, "Safari");
        m.refresh();
        m.backend.focused = Some(1);
        m.refresh();
        hover(&mut m, (100.0, 300.0), 0.1);
        m.backend.mouse = true;
        hover(&mut m, (700.0, 300.0), 0.3);
        assert!(m.backend().focus_calls.is_empty());
        m.backend.mouse = false;
    }

    /// The main display (Space 10) and one to its right (Space 20).
    fn two_displays() -> Manager<MockBackend> {
        let mut m = manager(0.0);
        m.backend.active = 10;
        m.backend.displays = vec![
            Display {
                frame: Rect::new(0, 0, 1000, 625),
                area: Rect::new(0, 25, 1000, 600),
                space: 10,
            },
            Display {
                frame: Rect::new(1000, 0, 800, 625),
                area: Rect::new(1000, 25, 800, 600),
                space: 20,
            },
        ];
        m
    }

    /// Opens window `id` on the display showing `space`, focused.
    fn open_on(m: &mut Manager<MockBackend>, id: WinId, space: u64) {
        m.backend.open(id, "App");
        m.backend.space_of.insert(id, space);
        let x = if space == 20 { 1100 } else { 100 };
        m.backend.windows.last_mut().unwrap().frame = Rect::new(x, 100, 300, 200);
        m.refresh();
    }

    const MAIN: Rect = Rect::new(0, 25, 1000, 600);
    const SECOND: Rect = Rect::new(1000, 25, 800, 600);

    #[test]
    fn each_display_tiles_its_own_windows() {
        let mut m = two_displays();
        open_on(&mut m, 1, 10);
        open_on(&mut m, 2, 10);
        open_on(&mut m, 3, 20);
        assert_eq!(m.backend().frame(1), Rect::new(0, 25, 495, 600));
        assert_eq!(m.backend().frame(2), Rect::new(505, 25, 495, 600));
        assert_eq!(m.backend().frame(3), SECOND);
        // A new window splits the window focused on its own display.
        open_on(&mut m, 4, 20);
        assert_eq!(m.backend().frame(3), Rect::new(1000, 25, 395, 600));
        assert_eq!(m.backend().frame(4), Rect::new(1405, 25, 395, 600));
        assert_eq!(m.backend().frame(1), Rect::new(0, 25, 495, 600));
        assert_eq!(m.backend().frame(2), Rect::new(505, 25, 495, 600));
    }

    #[test]
    fn commands_act_on_the_display_of_the_focused_window() {
        let mut m = two_displays();
        open_on(&mut m, 1, 10);
        open_on(&mut m, 2, 10);
        open_on(&mut m, 3, 20);
        open_on(&mut m, 4, 20);
        m.backend.focused = Some(3);
        m.refresh();
        assert_eq!(m.active_space(), 20);
        m.execute(&Command::Rotate).unwrap();
        m.refresh();
        assert_eq!(m.backend().frame(3), Rect::new(1000, 25, 800, 295));
        assert_eq!(m.backend().frame(1), Rect::new(0, 25, 495, 600));
        m.backend.focused = Some(1);
        m.refresh();
        assert_eq!(m.active_space(), 10);
    }

    #[test]
    fn focus_crosses_to_the_next_display_at_the_edge() {
        let mut m = two_displays();
        open_on(&mut m, 1, 10);
        open_on(&mut m, 2, 10);
        open_on(&mut m, 3, 20);
        m.backend.focused = Some(2);
        m.refresh();
        m.execute(&Command::Focus(Direction::Right)).unwrap();
        assert_eq!(m.backend().focused, Some(3));
        assert_eq!(m.active_space(), 20);
        m.execute(&Command::Focus(Direction::Left)).unwrap();
        assert_eq!(m.backend().focused, Some(2));
        assert_eq!(m.active_space(), 10);
        assert!(m.execute(&Command::Focus(Direction::Left)).is_ok());
        assert_eq!(m.backend().focused, Some(1));
        assert!(m.execute(&Command::Focus(Direction::Left)).is_err());
    }

    #[test]
    fn warp_takes_a_window_to_an_empty_display() {
        let mut m = two_displays();
        open_on(&mut m, 1, 10);
        open_on(&mut m, 2, 10);
        m.backend.focused = Some(2);
        m.refresh();
        m.execute(&Command::Warp(Direction::Right)).unwrap();
        m.refresh();
        assert_eq!(m.backend().frame(2), SECOND);
        assert_eq!(m.backend().frame(1), MAIN);
        // macOS has not said it moved yet: the layout still holds it.
        m.refresh();
        assert_eq!(m.backend().frame(2), SECOND);
        // And back, next to the window there.
        m.backend.space_of.insert(2, 20);
        m.execute(&Command::Warp(Direction::Left)).unwrap();
        m.refresh();
        assert_eq!(m.backend().frame(1), Rect::new(0, 25, 495, 600));
        assert_eq!(m.backend().frame(2), Rect::new(505, 25, 495, 600));
    }

    #[test]
    fn swap_trades_windows_across_displays() {
        let mut m = two_displays();
        open_on(&mut m, 1, 10);
        open_on(&mut m, 2, 10);
        open_on(&mut m, 3, 20);
        m.backend.focused = Some(2);
        m.refresh();
        m.execute(&Command::Swap(Direction::Right)).unwrap();
        m.refresh();
        assert_eq!(m.backend().frame(2), SECOND);
        assert_eq!(m.backend().frame(3), Rect::new(505, 25, 495, 600));
        assert_eq!(m.active_space(), 20);
    }

    #[test]
    fn a_window_the_user_puts_on_another_display_joins_its_layout() {
        let mut m = two_displays();
        open_on(&mut m, 1, 10);
        open_on(&mut m, 2, 10);
        open_on(&mut m, 3, 20);
        // Moved there some other way than cberry (the Window menu, say).
        m.backend.space_of.insert(1, 20);
        m.backend
            .windows
            .iter_mut()
            .find(|w| w.id == 1)
            .unwrap()
            .frame = Rect::new(1200, 300, 300, 200);
        m.refresh();
        assert_eq!(m.backend().frame(2), MAIN);
        assert_eq!(m.backend().frame(3).w + m.backend().frame(1).w, 790);
    }

    #[test]
    fn dragging_a_window_to_another_display_moves_it_there() {
        let mut m = two_displays();
        open_on(&mut m, 1, 10);
        open_on(&mut m, 2, 10);
        open_on(&mut m, 3, 20);
        // Window 2 by its title bar, over the left of window 3: it splits
        // window 3 (only a highlight of that half shows), leaving window 1
        // alone on the main display.
        m.backend.cursor = (700.0, 35.0);
        m.backend.mouse = true;
        m.track_pointer();
        m.backend.set_frame(2, Rect::new(1050, 200, 495, 600));
        m.backend.cursor = (1100.0, 330.0);
        m.track_pointer();
        let overlay = m.backend().overlay.unwrap();
        assert_eq!(overlay.above, 3);
        assert_eq!(overlay.rect, SECOND.half(Direction::Left));
        m.backend.mouse = false;
        m.track_pointer();
        m.refresh();
        assert_eq!(m.backend().frame(2), Rect::new(1000, 25, 395, 600));
        assert_eq!(m.backend().frame(3), Rect::new(1405, 25, 395, 600));
        assert_eq!(m.backend().frame(1), MAIN);

        // Window 1 onto a third display, empty: it moves there.
        m.backend.displays.push(Display {
            frame: Rect::new(-800, 0, 800, 625),
            area: Rect::new(-800, 25, 800, 600),
            space: 30,
        });
        m.refresh();
        m.backend.cursor = (200.0, 35.0);
        m.backend.mouse = true;
        m.track_pointer();
        m.backend.set_frame(1, Rect::new(-600, 200, 1000, 600));
        m.backend.cursor = (-400.0, 300.0);
        m.track_pointer();
        assert_eq!(m.backend().overlay, None);
        m.backend.mouse = false;
        m.track_pointer();
        m.refresh();
        assert_eq!(m.backend().frame(1), Rect::new(-800, 25, 800, 600));
        assert_eq!(m.backend().frame(2), Rect::new(1000, 25, 395, 600));
    }

    #[test]
    fn send_display_moves_the_window_straight_there() {
        let mut m = two_displays();
        open_on(&mut m, 1, 10);
        open_on(&mut m, 2, 10);
        open_on(&mut m, 3, 20);
        open_on(&mut m, 4, 20);
        // From the far right of the second display to the first.
        m.backend.focused = Some(4);
        m.refresh();
        m.execute(&Command::parse("send-display prev").unwrap())
            .unwrap();
        m.refresh();
        assert_eq!(m.backend().frame(3), SECOND);
        // It splits the window last focused there, tall: one above the other.
        assert_eq!(m.backend().frame(4), Rect::new(505, 330, 495, 295));
        assert_eq!(m.active_space(), 10);
        m.execute(&Command::parse("send-display 2").unwrap())
            .unwrap();
        m.refresh();
        assert_eq!(m.backend().frame(4).w, 395);
        assert!(
            m.execute(&Command::parse("send-display 2").unwrap())
                .is_err()
        );
        assert!(
            m.execute(&Command::parse("send-display 3").unwrap())
                .is_err()
        );
    }

    #[test]
    fn spaces_are_numbered_on_the_display_of_the_focused_window() {
        let mut m = two_displays();
        m.backend.display_spaces = vec![vec![10, 11], vec![20, 21, 22]];
        m.execute(&Command::parse("label 1 mail").unwrap()).unwrap();
        open_on(&mut m, 1, 10);
        open_on(&mut m, 2, 20);
        m.backend.focused = Some(2);
        m.refresh();
        let list = m.execute(&Command::Label(None)).unwrap();
        assert_eq!(list, "1\tmail\tcurrent\n2\t-\n3\t-\n");
        // Switches the second display, not the main one.
        m.execute(&Command::parse("space 3").unwrap()).unwrap();
        assert_eq!(m.backend().displays[1].space, 22);
        assert_eq!(m.backend().displays[0].space, 10);
        assert!(m.execute(&Command::parse("space 4").unwrap()).is_err());
        // And sends there too.
        m.backend.displays[1].space = 20;
        m.refresh();
        m.execute(&Command::parse("send 2").unwrap()).unwrap();
        assert_eq!(m.backend().space_of[&2], 21);
    }

    #[test]
    fn going_after_a_sent_window_focuses_it() {
        let mut m = two_displays();
        m.backend.display_spaces = vec![vec![10, 11], vec![20, 21, 22]];
        open_on(&mut m, 1, 10);
        open_on(&mut m, 2, 20);
        m.backend.focused = Some(2);
        m.refresh();
        m.execute(&Command::parse("send 2").unwrap()).unwrap();
        m.backend.focused = None;
        m.backend.focus_calls.clear();
        m.execute(&Command::parse("space 2").unwrap()).unwrap();
        assert_eq!(m.backend().focus_calls, [2]);
        // macOS hands focus elsewhere: it is taken back.
        m.backend.focused = Some(1);
        m.backend.focus_calls.clear();
        std::thread::sleep(HOLD_FOCUS_RETRY);
        m.pump();
        assert_eq!(m.backend().focus_calls, [2]);
        // Only for a moment, and only that once.
        m.holding_focus = None;
        m.backend.focus_calls.clear();
        m.execute(&Command::parse("space 2").unwrap()).unwrap();
        assert!(m.backend().focus_calls.is_empty());
    }

    #[test]
    fn unplugging_a_display_brings_its_windows_to_the_one_left() {
        let mut m = two_displays();
        open_on(&mut m, 1, 10);
        open_on(&mut m, 2, 20);
        m.backend.displays.truncate(1);
        // macOS moves the windows over, to the Space shown there.
        m.backend.space_of.insert(2, 10);
        m.backend
            .windows
            .iter_mut()
            .find(|w| w.id == 2)
            .unwrap()
            .frame = Rect::new(400, 100, 300, 200);
        m.refresh();
        assert_eq!(m.backend().frame(1).w, 495);
        assert_eq!(m.backend().frame(2).w, 495);
    }

    fn three_spaces() -> Manager<MockBackend> {
        let mut m = manager(0.0);
        m.backend.spaces = vec![10, 20, 30];
        m.backend.active = 10;
        for line in [
            "label 1 general",
            "label 2 web",
            "label 3 coding",
            "rule space web Safari",
        ] {
            m.execute(&Command::parse(line).unwrap()).unwrap();
        }
        m
    }

    #[test]
    fn space_rule_sends_new_windows_to_their_space() {
        let mut m = three_spaces();
        m.backend.open(1, "Terminal");
        m.refresh();
        m.backend.open(2, "Safari");
        m.refresh();
        assert_eq!(m.backend().space_of[&2], 20);
        assert_eq!(m.tree().windows(), vec![1]);
        assert_eq!(m.backend().frame(1), Rect::new(0, 25, 1000, 600));
        // Over there it is tiled like any other window.
        m.backend.active = 20;
        m.refresh();
        assert_eq!(m.tree().windows(), vec![2]);
        // A window opened on its own Space stays.
        m.backend.open(3, "Safari");
        m.refresh();
        assert_eq!(m.backend().space_of[&3], 20);
        assert_eq!(m.tree().windows(), vec![2, 3]);
    }

    #[test]
    fn grid_rule_floats_new_windows_at_their_cell() {
        let mut m = manager(0.0);
        m.execute(&Command::GridRule {
            grid: [2, 2, 1, 0, 1, 1],
            app: "Notes".into(),
        })
        .unwrap();
        m.backend.open(1, "Terminal");
        m.refresh();
        m.backend.open(2, "Notes");
        m.refresh();
        assert_eq!(m.tree().windows(), vec![1]);
        assert_eq!(m.backend().frame(2), Rect::new(500, 25, 500, 300));
        // It stays a floating window: later polls leave it be.
        m.backend.set_frame(2, Rect::new(10, 40, 400, 300));
        m.refresh();
        assert_eq!(m.tree().windows(), vec![1]);
        assert_eq!(m.backend().frame(2), Rect::new(10, 40, 400, 300));
        // And can be tiled by hand.
        m.backend.focused = Some(2);
        m.execute(&Command::ToggleFloat).unwrap();
        assert_eq!(m.tree().windows(), vec![1, 2]);
    }

    #[test]
    fn grid_command_floats_the_focused_window_at_a_cell() {
        let mut m = manager(0.0);
        m.backend.open(1, "Terminal");
        m.backend.open(2, "Notes");
        m.refresh();
        assert_eq!(m.tree().windows(), vec![1, 2]);
        m.backend.focused = Some(2);
        m.execute(&Command::Grid([2, 2, 1, 0, 1, 1])).unwrap();
        assert_eq!(m.tree().windows(), vec![1]);
        assert_eq!(m.backend().frame(2), Rect::new(500, 25, 500, 300));
        // It stays floating: later polls leave it be.
        m.refresh();
        assert_eq!(m.tree().windows(), vec![1]);
        assert_eq!(m.backend().frame(2), Rect::new(500, 25, 500, 300));
        // Already floating: it just moves to another cell.
        m.execute(&Command::Grid([2, 2, 0, 1, 1, 1])).unwrap();
        assert_eq!(m.backend().frame(2), Rect::new(0, 325, 500, 300));
        // And `cberry float` tiles it again.
        m.execute(&Command::ToggleFloat).unwrap();
        assert_eq!(m.tree().windows(), vec![1, 2]);
    }

    #[test]
    fn grid_command_needs_a_focused_window() {
        let mut m = manager(0.0);
        assert!(m.execute(&Command::Grid([2, 2, 0, 0, 1, 1])).is_err());
    }

    #[test]
    fn grid_command_stays_on_a_floating_windows_display() {
        let mut m = two_displays();
        open_on(&mut m, 1, 10);
        open_on(&mut m, 2, 20);
        m.backend.focused = Some(2);
        m.refresh();
        m.execute(&Command::ToggleFloat).unwrap();
        m.backend.focused = Some(1);
        m.refresh();
        assert_eq!(m.active_space(), 10);
        // Focus on a floating window does not make its display current.
        m.backend.focused = Some(2);
        m.refresh();
        assert_eq!(m.active_space(), 10);
        m.execute(&Command::Grid([2, 2, 0, 0, 1, 1])).unwrap();
        assert_eq!(m.backend().frame(2), Rect::new(1000, 25, 400, 300));
    }

    #[test]
    fn resize_moves_a_floating_windows_edges_as_a_tiled_ones() {
        let mut m = manager(0.0);
        m.backend.open(1, "Terminal");
        m.backend.open(2, "Notes");
        m.backend.open(3, "Mail");
        m.refresh();
        m.backend.focused = Some(3);
        m.execute(&Command::Grid([4, 4, 1, 1, 2, 2])).unwrap();
        let tiled = m.backend().frame(1);
        let start = Rect::new(250, 175, 500, 300);
        assert_eq!(m.backend().frame(3), start);
        m.backend.focused = Some(3);
        let resize = |m: &mut Manager<MockBackend>, dir| {
            m.execute(&Command::Resize(dir, Some(0.1))).unwrap();
        };
        resize(&mut m, Direction::Left);
        assert_eq!(m.backend().frame(3), Rect::new(150, 175, 600, 300));
        resize(&mut m, Direction::Right);
        assert_eq!(m.backend().frame(3), start);
        resize(&mut m, Direction::Up);
        assert_eq!(m.backend().frame(3), Rect::new(250, 115, 500, 360));
        resize(&mut m, Direction::Down);
        assert_eq!(m.backend().frame(3), start);
        // The tiled windows were not the ones meant.
        assert_eq!(m.backend().frame(1), tiled);
    }

    #[test]
    fn resize_moves_the_far_edge_of_a_floating_window_at_the_display_edge() {
        let mut m = manager(0.0);
        m.backend.open(1, "Notes");
        m.refresh();
        m.backend.focused = Some(1);
        m.execute(&Command::Grid([2, 2, 0, 0, 1, 1])).unwrap();
        m.execute(&Command::Resize(Direction::Right, Some(0.1)))
            .unwrap();
        assert_eq!(m.backend().frame(1), Rect::new(0, 25, 600, 300));
        m.execute(&Command::Resize(Direction::Down, Some(0.1)))
            .unwrap();
        assert_eq!(m.backend().frame(1), Rect::new(0, 25, 600, 360));
    }

    #[test]
    fn resize_keeps_a_floating_window_within_its_display() {
        let mut m = manager(0.0);
        m.backend.open(1, "Notes");
        m.refresh();
        m.backend.focused = Some(1);
        m.execute(&Command::Grid([1, 1, 0, 0, 1, 1])).unwrap();
        // Already as wide as the display.
        assert!(
            m.execute(&Command::Resize(Direction::Right, Some(0.1)))
                .is_err()
        );
        // Never below the smallest size a gesture leaves.
        m.execute(&Command::Resize(Direction::Left, Some(0.99)))
            .unwrap();
        assert_eq!(m.backend().frame(1).w, MIN_GESTURE_SIZE);
        assert!(
            m.execute(&Command::Resize(Direction::Left, Some(0.1)))
                .is_err()
        );
    }

    #[test]
    fn grid_and_floating_resize_animate() {
        let mut m = manager(0.42);
        m.backend.open(1, "Notes");
        m.backend.open(2, "Mail");
        m.refresh();
        settle(&mut m);
        m.backend.focused = Some(2);
        let start = m.backend().frame(2);
        let cell = Rect::new(250, 175, 500, 300);
        m.execute(&Command::Grid([4, 4, 1, 1, 2, 2])).unwrap();
        assert_eq!(m.backend().frame(2), start, "not there at once");
        m.tick(1.0 / 60.0);
        let between = m.backend().frame(2);
        assert_ne!(between, start);
        assert_ne!(between, cell);
        // A key pressed twice in flight adds up from where it is heading.
        m.execute(&Command::Resize(Direction::Left, Some(0.1)))
            .unwrap();
        m.execute(&Command::Resize(Direction::Left, Some(0.1)))
            .unwrap();
        settle(&mut m);
        assert_eq!(m.backend().frame(2), Rect::new(50, 175, 700, 300));
        // Polls leave it there.
        m.refresh();
        assert_eq!(m.backend().frame(2), Rect::new(50, 175, 700, 300));
    }

    #[test]
    fn resize_leaves_a_fixed_size_floating_window_alone() {
        let mut m = manager(0.0);
        m.backend.open(1, "Terminal");
        m.backend.open(2, "Calculator");
        m.backend.windows.last_mut().unwrap().tileable = false;
        m.refresh();
        let before = m.backend().frame(2);
        m.backend.focused = Some(2);
        assert!(
            m.execute(&Command::Resize(Direction::Right, Some(0.1)))
                .is_err()
        );
        assert_eq!(m.backend().frame(2), before);
    }

    #[test]
    fn grid_rule_puts_fixed_size_windows_in_the_corner() {
        let mut m = manager(0.0);
        m.execute(&Command::GridRule {
            grid: [4, 4, 3, 3, 1, 1],
            app: "Hesap Makinesi".into(),
        })
        .unwrap();
        m.refresh();
        m.backend.windows.push(WindowInfo {
            id: 7,
            frame: Rect::new(100, 100, 230, 408),
            app: "Hesap Makinesi".into(),
            tileable: false,
            movable: true,
        });
        m.refresh();
        // Area 0,25 1000x600: right and bottom edges at 1000 and 625.
        assert_eq!(m.backend().frame(7), Rect::new(770, 217, 230, 408));
        assert!(m.tree().is_empty());
    }

    #[test]
    fn grid_rule_leaves_windows_open_before_it_alone() {
        let mut m = manager(0.0);
        m.backend.open(1, "Notes");
        m.refresh();
        m.execute(&Command::GridRule {
            grid: [2, 2, 1, 0, 1, 1],
            app: "Notes".into(),
        })
        .unwrap();
        m.refresh();
        assert_eq!(m.tree().windows(), vec![1]);
    }

    #[test]
    fn space_rule_waits_for_a_window_to_be_manageable() {
        let mut m = three_spaces();
        m.backend.open(1, "Terminal");
        m.refresh();
        m.backend.open(2, "Safari");
        m.backend.windows.last_mut().unwrap().tileable = false;
        m.refresh();
        assert_eq!(m.backend().space_of[&2], 10, "not known yet");
        m.backend.windows.last_mut().unwrap().tileable = true;
        m.refresh();
        assert_eq!(m.backend().space_of[&2], 20);
        assert_eq!(m.tree().windows(), vec![1]);
    }

    #[test]
    fn space_rule_leaves_windows_that_were_already_open() {
        let mut m = three_spaces();
        m.backend.open(1, "Safari");
        m.backend.open(2, "Terminal");
        m.refresh();
        assert_eq!(m.backend().space_of[&1], 10);
        assert_eq!(m.tree().windows(), vec![1, 2]);
        // Hidden and shown again (closed with the red button) is not new.
        m.backend.minimised.insert(1);
        m.refresh();
        m.backend.minimised.remove(&1);
        m.refresh();
        assert_eq!(m.backend().space_of[&1], 10);
    }

    #[test]
    fn send_moves_the_focused_window_to_a_space() {
        let mut m = three_spaces();
        m.backend.open(1, "Terminal");
        m.backend.open(2, "Notes");
        m.refresh();
        m.execute(&Command::parse("send coding").unwrap()).unwrap();
        assert_eq!(m.backend().space_of[&2], 30);
        assert_eq!(m.tree().windows(), vec![1]);
        assert_eq!(m.backend().frame(1), Rect::new(0, 25, 1000, 600));
        assert_eq!(m.backend().focus_calls.last(), Some(&1), "focus stays here");
        m.backend.active = 30;
        m.refresh();
        assert_eq!(m.tree().windows(), vec![2]);

        m.backend.focused = Some(2);
        assert!(
            m.execute(&Command::parse("send 3").unwrap()).is_err(),
            "already there"
        );
        assert!(m.execute(&Command::parse("send 9").unwrap()).is_err());
        assert!(m.execute(&Command::parse("send music").unwrap()).is_err());
    }

    #[test]
    fn space_switches_by_number_or_label() {
        let mut m = three_spaces();
        m.execute(&Command::parse("space web").unwrap()).unwrap();
        assert_eq!(m.backend().active, 20);
        m.execute(&Command::parse("space 3").unwrap()).unwrap();
        assert_eq!(m.backend().active, 30);
        m.execute(&Command::parse("space coding").unwrap()).unwrap();
        assert!(m.execute(&Command::parse("space music").unwrap()).is_err());
        assert!(m.execute(&Command::parse("space 7").unwrap()).is_err());
    }

    #[test]
    fn floating_space_leaves_windows_alone() {
        let mut m = three_spaces();
        m.backend.open(1, "Terminal");
        m.backend.open(2, "Notes");
        m.refresh();
        assert_eq!(m.tree().windows(), vec![1, 2]);
        m.execute(&Command::parse("layout general float").unwrap())
            .unwrap();
        assert!(m.tree().is_empty());
        assert_eq!(
            m.backend().frame(1),
            Rect::new(0, 25, 495, 600),
            "left where it was"
        );
        m.backend.open(3, "Safari"); // the Safari rule sends it away
        m.backend.open(4, "Finder");
        m.refresh();
        assert!(m.tree().is_empty(), "new windows float too");
        assert_eq!(m.backend().frame(4), Rect::new(100, 100, 300, 200));
        m.backend.focused = Some(4);
        assert!(m.execute(&Command::ToggleFloat).is_err());
        let list = m.execute(&Command::Label(None)).unwrap();
        assert!(list.starts_with("1\tgeneral\tfloat\tcurrent\n"), "{list}");

        m.execute(&Command::parse("layout 1 bsp").unwrap()).unwrap();
        assert_eq!(m.tree().len(), 3);
    }

    #[test]
    fn floating_space_elsewhere_drops_its_layout() {
        let mut m = three_spaces();
        m.backend.open(1, "Terminal");
        m.refresh();
        m.backend.active = 30;
        m.backend.open(2, "Notes");
        m.refresh();
        m.backend.active = 10;
        m.refresh();
        assert!(m.space_tree(30).is_some());
        m.execute(&Command::parse("layout coding float").unwrap())
            .unwrap();
        assert!(m.space_tree(30).is_none());
        // Sending a window there does not tile it there.
        m.backend.focused = Some(1);
        m.execute(&Command::parse("send coding").unwrap()).unwrap();
        assert!(m.space_tree(30).is_none());
        m.backend.active = 30;
        m.refresh();
        assert!(m.tree().is_empty());
    }

    #[test]
    fn label_lists_the_spaces() {
        let mut m = three_spaces();
        m.backend.spaces.push(40);
        let out = m.execute(&Command::Label(None)).unwrap();
        assert_eq!(out, "1\tgeneral\tcurrent\n2\tweb\n3\tcoding\n4\t-\n");
    }

    #[test]
    fn size_rounding_without_drag_is_ignored() {
        let mut m = manager(0.0);
        m.backend.open(1, "a");
        m.backend.open(2, "b");
        m.refresh();
        // A terminal rounds itself to its character grid: no mouse involved.
        m.backend.set_frame(1, Rect::new(0, 25, 488, 596));
        m.refresh();
        m.refresh();
        assert_eq!(m.backend().frame(2), Rect::new(505, 25, 495, 600));
    }

    #[test]
    fn config_and_rule_commands() {
        let cmd = |s: &str| Command::parse(s).unwrap();
        let mut m = manager(0.0);
        m.backend.open(1, "Terminal");
        m.backend.open(2, "Finder");
        m.refresh();

        m.execute(&cmd("config gap 20")).unwrap();
        assert_eq!(m.backend().frame(2).x, 510);
        assert_eq!(m.execute(&cmd("config gap")), Ok("20\n".into()));
        assert!(m.execute(&cmd("config gap -3")).is_err());
        assert_eq!(m.config().gap, 20);

        m.execute(&cmd("config animation 0.25")).unwrap();
        assert_eq!(m.config().animation_duration, 0.25);

        m.execute(&cmd("rule float Finder")).unwrap();
        assert_eq!(m.tree().windows(), vec![1]);
        settle(&mut m);
        assert_eq!(m.backend().frame(1).w, 1000);
        m.refresh();
        assert_eq!(m.tree().windows(), vec![1], "stays floating");
        m.execute(&cmd("rule tile Finder")).unwrap();
        m.refresh();
        assert_eq!(m.tree().len(), 2);
    }

    /// The user's case: Terminal (1) over Finder (3) on the left, Claude (2)
    /// on the right, with the column border dragged to x = 600.
    fn desk_on_first_space() -> Manager<MockBackend> {
        let mut m = three_windows();
        drag(&mut m, 2, Rect::new(600, 25, 400, 600));
        assert_eq!(m.backend().frame(1), Rect::new(0, 25, 590, 295));
        m
    }

    fn frames(m: &Manager<MockBackend>) -> Vec<Rect> {
        (1..=3).map(|id| m.backend().frame(id)).collect()
    }

    #[test]
    fn each_space_keeps_its_own_layout() {
        let mut m = desk_on_first_space();
        let desk = frames(&m);
        let tree = m.tree().clone();

        // Go to Space 2 and open a window there: it gets the whole screen.
        m.backend.active = 2;
        m.refresh();
        assert!(m.tree().is_empty());
        m.backend.open(4, "Safari");
        m.refresh();
        settle(&mut m);
        assert_eq!(m.backend().frame(4), Rect::new(0, 25, 1000, 600));
        assert_eq!(m.space_tree(0), Some(&tree), "the first Space is untouched");

        // Back to the first Space: everything is where it was.
        m.backend.active = 0;
        m.refresh();
        settle(&mut m);
        assert_eq!(m.tree(), &tree);
        assert_eq!(frames(&m), desk);
        assert_eq!(m.space_tree(2).unwrap().windows(), vec![4]);
    }

    #[test]
    fn switch_animation_does_not_mix_spaces() {
        let mut m = desk_on_first_space();
        let tree = m.tree().clone();
        m.backend.active = 2;
        m.backend.open(4, "Safari");
        m.backend.active = 0;
        m.refresh();

        // macOS already reports Space 2, but the screen still lists the first.
        m.backend.active = 2;
        m.backend.screen_space = Some(0);
        m.refresh();
        m.refresh();
        assert!(
            m.tree().is_empty(),
            "first-Space windows must not join Space 2"
        );
        assert_eq!(m.space_tree(0), Some(&tree));
        // And the other way round: Space 2 is not on screen yet.
        m.backend.screen_space = None;
        m.refresh();
        assert_eq!(m.tree().windows(), vec![4]);

        m.backend.active = 0;
        m.backend.screen_space = Some(2);
        m.refresh();
        assert_eq!(m.tree(), &tree, "not on screen yet is not minimised");
        m.backend.screen_space = None;
        m.refresh();
        assert_eq!(m.tree(), &tree);
    }

    #[test]
    fn window_moved_to_another_space_follows_it() {
        let mut m = desk_on_first_space();
        // The user sends Finder to Space 2.
        m.backend.space_of.insert(3, 2);
        m.refresh();
        settle(&mut m);
        assert_eq!(m.tree().windows(), vec![1, 2]);
        assert_eq!(m.backend().frame(1).h, 600, "Terminal takes Finder's place");
        m.backend.active = 2;
        m.refresh();
        settle(&mut m);
        assert_eq!(m.tree().windows(), vec![3]);
        assert_eq!(m.backend().frame(3), Rect::new(0, 25, 1000, 600));
    }

    #[test]
    fn closing_a_window_on_another_space() {
        let mut m = desk_on_first_space();
        m.backend.active = 2;
        m.refresh();
        m.backend.close(3);
        m.refresh();
        m.backend.active = 0;
        m.refresh();
        settle(&mut m);
        assert_eq!(m.tree().windows(), vec![1, 2]);
        assert_eq!(m.backend().frame(1), Rect::new(0, 25, 590, 600));
    }

    #[test]
    fn minimised_windows_leave_and_come_back() {
        let mut m = desk_on_first_space();
        m.backend.minimised.insert(3);
        m.refresh();
        settle(&mut m);
        assert_eq!(m.tree().windows(), vec![1, 2]);
        assert_eq!(m.backend().frame(1).h, 600);
        m.backend.minimised.remove(&3);
        m.backend.focused = Some(1);
        m.refresh();
        settle(&mut m);
        assert_eq!(m.tree().len(), 3);
    }

    #[test]
    fn raise_floating_brings_floats_forward() {
        let mut m = manager(0.0);
        m.backend.open(1, "Terminal");
        m.backend.open(2, "Calculator"); // in float_apps by default
        m.backend.open(3, "Safari");
        m.backend.open(4, "Notes");
        m.refresh();
        // The user floats Notes by hand.
        m.backend.focused = Some(4);
        m.execute(&Command::ToggleFloat).unwrap();
        assert_eq!(m.tree().windows(), vec![1, 3]);

        // Notes is focused, so it is raised last and stays on top.
        m.execute(&Command::RaiseFloating).unwrap();
        assert_eq!(m.backend().focus_calls, vec![2, 4]);

        m.backend.focus_calls.clear();
        m.backend.focused = Some(1);
        m.execute(&Command::RaiseFloating).unwrap();
        assert_eq!(m.backend().focus_calls, vec![2, 4]);
    }

    #[test]
    fn new_window_splits_the_window_focused_before_its_app_jumped_in() {
        let mut m = manager(0.0);
        m.backend.open(1, "Terminal");
        m.refresh();
        m.backend.open(2, "Safari");
        m.refresh();
        assert_eq!(m.focused, Some(2));
        // A shortcut opens a Terminal window: Terminal comes to the front,
        // its old window takes focus, then the new one appears.
        m.backend.focused = Some(1);
        m.refresh();
        m.backend.open(3, "Terminal");
        m.refresh();
        // Terminal's window keeps its half; Safari's half is split.
        assert_eq!(m.backend().frame(1), Rect::new(0, 25, 495, 600));
        assert!(m.backend().frame(3).x >= 505, "{:?}", m.backend().frame(3));

        // Long after a focus change, the focused window is split as usual.
        let mut m = manager(0.0);
        m.backend.open(1, "Terminal");
        m.refresh();
        m.backend.open(2, "Safari");
        m.refresh();
        m.backend.focused = Some(1);
        m.refresh();
        m.tick(2.0);
        m.backend.open(3, "Terminal");
        m.refresh();
        assert_eq!(m.backend().frame(3).x, 0, "Terminal's window is split");
    }

    #[test]
    fn bind_adds_hotkeys() {
        let mut m = manager(0.0);
        let bind =
            |m: &mut Manager<MockBackend>, line: &str| m.execute(&Command::parse(line).unwrap());
        assert!(bind(&mut m, "bind alt - left  : cberry focus left").is_ok());
        assert!(bind(&mut m, "bind shift + alt - 1 : cberry send 1").is_ok());
        assert!(bind(&mut m, "bind alt - : broken").is_err());
        assert_eq!(
            bind(&mut m, "bind").unwrap(),
            "alt - left  : cberry focus left\nshift + alt - 1 : cberry send 1\n"
        );
    }

    #[test]
    fn raise_floating_without_floats() {
        let mut m = manager(0.0);
        m.backend.open(1, "Terminal");
        m.refresh();
        assert!(m.execute(&Command::RaiseFloating).is_err());
        assert!(m.backend().focus_calls.is_empty());
    }

    #[test]
    fn closed_window_is_replaced_at_once() {
        for speed in [0.42, 0.0] {
            let mut m = manager(speed);
            m.backend.open(1, "Terminal");
            m.backend.open(2, "Safari");
            m.refresh();
            settle(&mut m);
            // Safari closes: the event arrives while its window is still in
            // the window list, playing the close animation.
            m.backend.events.push(WindowEvent::Destroyed(2));
            assert!(!m.handle_events());
            assert_eq!(m.tree().windows(), vec![1]);
            if speed > 0.0 {
                assert!(m.is_animating(), "slides into the gap");
                m.tick(1.0 / 60.0);
                assert!(m.backend().frame(1).w > 495);
                settle(&mut m);
            } else {
                assert!(!m.is_animating(), "animation 0: no animation");
            }
            assert_eq!(m.backend().frame(1), Rect::new(0, 25, 1000, 600));
            // Polls during the close animation must not tile it again.
            m.refresh();
            m.refresh();
            assert_eq!(m.tree().windows(), vec![1]);
            m.backend.close(2);
            m.refresh();
            assert!(m.closed.is_empty(), "forgotten once really gone");
        }
    }

    #[test]
    fn window_hidden_on_close_is_tiled_when_shown_again() {
        let mut m = manager(0.0);
        m.backend.open(1, "Terminal");
        m.backend.open(2, "Claude");
        m.refresh();
        // The red button: the app reports the window destroyed but only
        // hides it, so it stays in the list of all windows.
        m.backend.events.push(WindowEvent::Destroyed(2));
        m.handle_events();
        m.refresh();
        m.backend.minimised.insert(2);
        m.refresh();
        assert_eq!(m.tree().windows(), vec![1]);
        // Reopened from the Dock: the very same window comes back.
        m.backend.minimised.remove(&2);
        m.refresh();
        assert_eq!(m.tree().windows(), vec![1, 2]);
    }

    #[test]
    fn closing_on_another_space_by_event() {
        let mut m = desk_on_first_space();
        m.backend.active = 2;
        m.refresh();
        m.backend.events.push(WindowEvent::Destroyed(3));
        m.handle_events();
        m.backend.active = 0;
        m.backend.close(3);
        m.refresh();
        settle(&mut m);
        assert_eq!(m.tree().windows(), vec![1, 2]);
    }

    #[test]
    fn change_events_ask_for_a_refresh() {
        let mut m = manager(0.0);
        m.backend.events.push(WindowEvent::Changed);
        assert!(m.handle_events());
        assert!(!m.handle_events());
    }

    #[test]
    fn area_change_relayouts() {
        let mut m = manager(0.0);
        m.backend.open(1, "a");
        m.refresh();
        m.backend.area = Rect::new(0, 0, 800, 500);
        m.refresh();
        assert_eq!(m.backend().frame(1), Rect::new(0, 0, 800, 500));
    }
    #[test]
    fn a_window_makes_room_before_its_neighbour_grows_into_it() {
        for (grow, dir) in [(1, Direction::Right), (2, Direction::Left)] {
            let mut m = manager(0.42);
            m.backend.open(1, "a");
            m.refresh();
            m.backend.open(2, "b");
            m.refresh();
            settle(&mut m);
            m.backend.focused = Some(grow);
            m.refresh();
            m.execute(&Command::Resize(dir, Some(0.2))).unwrap();
            for _ in 0..5 {
                m.backend.writes.clear();
                m.tick(1.0 / 60.0);
                let shrink = 3 - grow;
                let order = &m.backend().writes;
                assert_eq!(order.len(), 2, "{order:?}");
                assert_eq!(order[0], shrink, "window {shrink} goes first: {order:?}");
            }
        }
    }
    #[test]
    fn an_animation_lands_with_a_final_write() {
        let mut m = manager(0.42);
        m.backend.open(1, "a");
        m.refresh();
        m.backend.open(2, "b");
        m.refresh();
        settle(&mut m);
        m.backend.focused = Some(1);
        m.refresh();
        m.backend.writes.clear();
        m.backend.in_flight.clear();
        m.execute(&Command::Resize(Direction::Right, Some(0.2)))
            .unwrap();
        settle(&mut m);
        let b = m.backend();
        for id in [1, 2] {
            let mine: Vec<bool> = b
                .writes
                .iter()
                .zip(&b.in_flight)
                .filter(|(w, _)| **w == id)
                .map(|(_, f)| *f)
                .collect();
            assert!(mine.len() > 3, "window {id} animates: {mine:?}");
            assert!(mine[..mine.len() - 1].iter().all(|f| *f), "{mine:?}");
            assert!(
                !mine.last().unwrap(),
                "window {id} lands with a final write"
            );
        }
    }
    #[test]
    fn a_move_asks_for_a_look_only_when_cberry_is_not_the_one_moving() {
        let mut m = manager(0.42);
        m.backend.open(1, "a");
        m.refresh();
        settle(&mut m);
        // Idle: the user or the app moved the window.
        m.backend.events.push(WindowEvent::Moved(1));
        assert!(m.handle_events(), "an outside move is looked at");
        // Animating: cberry's own frames; the end of the animation looks.
        m.backend.open(2, "b");
        m.refresh();
        assert!(m.is_animating());
        m.backend.events.push(WindowEvent::Moved(1));
        assert!(!m.handle_events(), "cberry's own moves are not");
        settle(&mut m);
        // The mouse is down: the user is dragging; the release looks.
        m.backend.mouse = true;
        m.backend.events.push(WindowEvent::Moved(1));
        assert!(!m.handle_events(), "a drag is not, until the release");
        m.backend.mouse = false;
        // Anything else always is.
        m.backend.events.push(WindowEvent::Changed);
        assert!(m.handle_events());
    }
    #[test]
    fn every_new_window_is_tiled_even_when_it_does_not_fit() {
        let mut m = manager(0.0);
        open_many(&mut m, 1, 12, "Terminal", (300, 180));
        assert_eq!(m.tree().len(), 12, "all tiled: {:?}", m.tree().windows());
        assert!(m.floating.is_empty(), "none floats: {:?}", m.floating);
    }

    #[test]
    fn a_window_too_big_for_its_half_stays_tiled() {
        // Each Terminal is a little too big for its half: it keeps its place
        // in the layout all the same.
        let mut m = manager(0.0);
        open_many(&mut m, 1, 1, "Safari", (0, 0));
        open_many(&mut m, 2, 1, "Notes", (0, 0));
        open_many(&mut m, 3, 1, "Notes", (0, 0));
        open_many(&mut m, 4, 1, "Terminal", (200, 150));
        open_many(&mut m, 5, 1, "Terminal", (200, 150));
        for _ in 0..5 {
            m.tick(1.0);
            m.refresh();
        }
        assert!(m.tree().contains(5), "{:?}", m.tree().windows());
        assert!(m.floating.is_empty());
        // So does the next one.
        m.backend.focused = Some(4);
        m.refresh();
        open_many(&mut m, 6, 1, "Terminal", (200, 150));
        assert!(m.tree().contains(6));
    }
    #[test]
    fn a_closing_window_leaves_at_once_beside_a_window_on_every_space() {
        let mut m = three_spaces();
        m.backend.open(1, "Terminal");
        m.refresh();
        m.backend.open(2, "Terminal");
        m.refresh();
        // A floating window on every Space, listed as on another one.
        m.backend.open(9, "Notes");
        m.backend.space_of.insert(9, 20);
        m.backend.sticky.insert(9);
        m.execute(&Command::parse("rule float Notes").unwrap())
            .unwrap();
        m.refresh();
        assert_eq!(m.tree().windows(), vec![1, 2]);
        // Window 2 closes: gone from the screen, still listed while it fades.
        m.backend.fading.insert(2);
        m.refresh();
        assert_eq!(m.tree().windows(), vec![1], "no wait for the fade");
    }
    #[test]
    fn animation_frames_follow_the_display() {
        let mut m = manager(0.42);
        let ms = |m: &mut Manager<MockBackend>| m.frame_interval().as_secs_f64() * 1000.0;
        assert!(
            (ms(&mut m) - 8.33).abs() < 0.01,
            "120 a second when unknown"
        );
        m.backend.frame_rate = Some(60.0);
        assert!((ms(&mut m) - 16.67).abs() < 0.01);
        m.backend.frame_rate = Some(120.0);
        assert!((ms(&mut m) - 8.33).abs() < 0.01);
        m.backend.frame_rate = Some(1000.0);
        assert!((ms(&mut m) - 4.17).abs() < 0.01, "no faster than 240");
    }
    /// Claude (1) over a Terminal (3) on the left, a Terminal (2) on the
    /// right. Claude will not get narrower than 400 points.
    fn claude_left_terminal_right() -> Manager<MockBackend> {
        let mut m = manager(0.0);
        m.backend.open(1, "Claude");
        m.backend.min.insert(1, (400, 0));
        m.refresh();
        m.backend.open(2, "Terminal");
        m.refresh();
        m.backend.focused = Some(1);
        m.refresh();
        m.backend.open(3, "Terminal");
        m.refresh();
        assert_eq!(m.tree().len(), 3);
        assert_eq!(m.backend().frame(3).x, 0, "window 3 under window 1");
        m.backend.focused = Some(2);
        m.refresh();
        m
    }

    #[test]
    fn resize_stops_where_a_window_will_not_go_smaller() {
        let mut m = claude_left_terminal_right();
        let mut overlaps = 0;
        let mut stopped = None;
        for _ in 0..12 {
            match m.execute(&Command::Resize(Direction::Left, Some(0.05))) {
                Ok(_) => {}
                Err(e) => stopped = Some(e),
            }
            m.refresh(); // where the windows went
            let (claude, right) = (m.backend().frame(1), m.backend().frame(2));
            if claude.max_x() + 10 > right.x {
                overlaps += 1;
            }
        }
        assert!(
            overlaps <= 1,
            "only the first try, before it is known: {overlaps}"
        );
        assert_eq!(stopped.as_deref(), Some("window 1 will not go smaller"));
        // The right Terminal grew right up to Claude, and no further.
        let (claude, right) = (m.backend().frame(1), m.backend().frame(2));
        assert_eq!(claude.w, 400);
        assert_eq!(right.x, claude.max_x() + 10);
        // One press back moves the border at once.
        m.execute(&Command::Resize(Direction::Right, Some(0.05)))
            .unwrap();
        assert!(m.backend().frame(2).x > right.x);
    }

    #[test]
    fn the_window_that_stops_a_resize_is_not_highlighted() {
        let mut m = claude_left_terminal_right();
        for _ in 0..12 {
            let _ = m.execute(&Command::Resize(Direction::Left, Some(0.05)));
            m.refresh();
        }
        assert!(
            m.execute(&Command::Resize(Direction::Left, Some(0.05)))
                .is_err()
        );
        assert_eq!(m.backend().overlay_changes, 0);
    }

    #[test]
    fn a_minimum_is_learned_from_any_frame_and_kept_for_that_window_only() {
        let mut m = manager(0.0);
        m.backend.open(1, "Claude");
        m.refresh();
        // A second window halves the first: it will not go below 600.
        m.backend.min.insert(1, (600, 0));
        m.backend.open(2, "Terminal");
        m.refresh();
        m.refresh(); // the look that sees window 1 refuse
        assert_eq!(
            m.min_sizes.get(&1),
            Some(&(600, 0)),
            "learned without a resize"
        );
        // The layout is left alone: still pure halves.
        assert_eq!(m.slots()[0].1.w, 495);
        // A second Claude window knows nothing of it.
        m.backend.open(3, "Claude");
        m.refresh();
        m.refresh();
        assert_eq!(m.min_sizes.get(&3), None);
        // Closing the window forgets it.
        m.backend.close(1);
        m.refresh();
        assert_eq!(m.min_sizes.get(&1), None);
    }

    #[test]
    fn swap_and_warp_do_not_squeeze_a_window_below_what_it_accepts() {
        let mut m = left_one_right_two();
        // Window 1 has the whole left half; it was seen to need 400 tall.
        m.min_sizes.insert(1, (0, 400));
        let before = m.tree().clone();
        // Into window 2's place, a quarter of the screen: refused.
        m.backend.focused = Some(1);
        m.refresh();
        assert_eq!(
            m.execute(&Command::Swap(Direction::Right)),
            Err("window 1 would not fit there".into())
        );
        assert_eq!(m.tree(), &before);
        assert_eq!(m.backend().overlay_changes, 0);
        // Window 3 warped into window 1's place would halve window 1.
        m.backend.focused = Some(3);
        m.refresh();
        assert!(m.execute(&Command::Warp(Direction::Left)).is_err());
        assert_eq!(m.tree(), &before);
        // Without what was learned, both go ahead as before.
        m.min_sizes.clear();
        m.execute(&Command::Warp(Direction::Left)).unwrap();
    }
    #[test]
    fn the_middle_window_grows_alone_up_to_its_neighbours_minimum() {
        // Three side by side on a wide screen: 1 | (2 | 3).
        let mut m = manager(0.0);
        m.backend.area = Rect::new(0, 25, 1800, 600);
        m.backend.open(1, "Claude");
        m.backend.min.insert(1, (500, 0));
        m.refresh();
        m.backend.open(2, "Terminal");
        m.refresh();
        m.backend.focused = Some(2);
        m.refresh();
        m.backend.open(3, "Terminal");
        m.refresh();
        // Dropped beside window 2 with the insert modifier, as by hand.
        m.tree.remove(3);
        m.tree.insert_at(3, 2, Direction::Right);
        m.relayout();
        m.backend.focused = Some(2);
        m.focused = Some(2);
        m.refresh();
        let right = m.backend().frame(3);
        assert!(m.backend().frame(1).max_x() < m.backend().frame(2).x);
        assert!(m.backend().frame(2).max_x() < right.x, "three columns");
        // The middle one grows to the left, a key press at a time.
        let mut stopped = false;
        for _ in 0..15 {
            stopped |= m
                .execute(&Command::Resize(Direction::Left, Some(0.05)))
                .is_err();
            m.refresh();
            assert_eq!(
                m.backend().frame(3),
                right,
                "the right window does not move"
            );
        }
        assert!(stopped);
        let (one, two) = (m.backend().frame(1), m.backend().frame(2));
        assert_eq!(one.w, 500, "window 1 at its minimum");
        assert_eq!(two.x, one.max_x() + 10, "window 2 right against it");
    }

    fn run(m: &mut Manager<MockBackend>, line: &str) -> Result<String, String> {
        m.execute(&Command::parse(line).unwrap())
    }

    #[test]
    fn next_and_prev_go_round_the_spaces() {
        let mut m = three_spaces();
        run(&mut m, "space next").unwrap();
        m.refresh();
        assert_eq!(m.active_space(), 20);
        run(&mut m, "space next").unwrap();
        m.refresh();
        assert_eq!(m.active_space(), 30);
        run(&mut m, "space next").unwrap();
        m.refresh();
        assert_eq!(m.active_space(), 10, "round to the first");
        run(&mut m, "space prev").unwrap();
        m.refresh();
        assert_eq!(m.active_space(), 30, "round to the last");
    }

    #[test]
    fn recent_goes_back_and_forth() {
        let mut m = three_spaces();
        m.refresh();
        assert!(run(&mut m, "space recent").is_err());
        run(&mut m, "space 3").unwrap();
        m.refresh();
        run(&mut m, "space recent").unwrap();
        m.refresh();
        assert_eq!(m.active_space(), 10);
        run(&mut m, "space recent").unwrap();
        m.refresh();
        assert_eq!(m.active_space(), 30);
    }

    #[test]
    fn a_label_named_next_wins_over_the_word() {
        let mut m = three_spaces();
        run(&mut m, "label 3 next").unwrap();
        run(&mut m, "space next").unwrap();
        m.refresh();
        assert_eq!(m.active_space(), 30);
    }

    #[test]
    fn send_next_moves_the_window_one_space_on() {
        let mut m = three_spaces();
        m.backend.open(1, "Terminal");
        m.refresh();
        run(&mut m, "send next").unwrap();
        assert_eq!(m.backend().space_of[&1], 20);
    }

    #[test]
    fn a_carry_ends_on_the_new_space() {
        let mut m = three_spaces();
        m.backend.carries = true;
        m.backend.open(1, "Terminal");
        m.refresh();
        run(&mut m, "send 2").unwrap();
        assert_eq!(m.backend().active, 20);
        assert_eq!(m.backend().focused, Some(1));
    }

    #[test]
    fn landing_on_a_space_gives_its_last_window_the_keyboard() {
        let mut m = three_spaces();
        m.backend.open(1, "Terminal");
        m.refresh();
        m.backend.active = 20;
        m.refresh();
        m.backend.open(2, "Notes");
        m.refresh();
        // Back to Space 1, with macOS leaving the focus on Notes.
        m.backend.active = 10;
        m.backend.focus_calls.clear();
        m.refresh();
        assert_eq!(m.backend().focus_calls, [1]);
    }

    #[test]
    fn landing_leaves_a_window_that_just_opened_alone() {
        let mut m = three_spaces();
        m.backend.open(1, "Terminal");
        m.refresh();
        m.backend.active = 20;
        m.refresh();
        m.backend.open(2, "Notes");
        m.refresh();
        m.backend.active = 10;
        m.backend.open(3, "Mail"); // focused, and not tiled yet
        m.backend.focus_calls.clear();
        m.refresh();
        assert!(m.backend().focus_calls.is_empty());
    }

    #[test]
    fn a_rule_does_not_carry_with_rule_carry_off() {
        let mut m = three_spaces();
        m.backend.carries = true;
        run(&mut m, "rule space 2 Safari").unwrap();
        run(&mut m, "config rule_carry off").unwrap();
        m.refresh();
        m.backend.open(1, "Safari");
        m.refresh();
        assert_eq!(m.backend().active, 10, "the user stays");
        assert_eq!(m.backend().space_of[&1], 10);
        assert_eq!(m.tree().windows(), vec![1], "tiled here instead");
    }

    #[test]
    fn a_rule_carries_by_default() {
        let mut m = three_spaces();
        m.backend.carries = true;
        run(&mut m, "rule space 2 Safari").unwrap();
        m.refresh();
        m.backend.open(1, "Safari");
        m.refresh();
        assert_eq!(m.backend().space_of[&1], 20);
    }

    #[test]
    fn query_json_lists_the_windows() {
        let mut m = manager(0.0);
        assert_eq!(run(&mut m, "query --json").unwrap(), "[]\n");
        m.backend.open(1, "Term\"inal");
        m.refresh();
        assert_eq!(
            run(&mut m, "query --json").unwrap(),
            "[{\"id\":1,\"app\":\"Term\\\"inal\",\"x\":0,\"y\":25,\"w\":1000,\"h\":600,\
             \"state\":\"tiled\",\"focused\":true}]\n"
        );
    }

    #[test]
    fn the_daemon_says_its_version() {
        let mut m = manager(0.0);
        assert_eq!(
            run(&mut m, "daemon-version").unwrap().trim(),
            env!("CARGO_PKG_VERSION")
        );
    }

    fn bordered() -> Manager<MockBackend> {
        let mut m = manager(0.42);
        run(&mut m, "config border on").unwrap();
        m.backend.open(1, "Terminal");
        m.backend.open(2, "Notes");
        m.refresh();
        settle(&mut m);
        m.refresh();
        m.pump();
        m
    }

    #[test]
    fn no_border_unless_asked_for() {
        let mut m = manager(0.0);
        m.backend.open(1, "Terminal");
        m.refresh();
        m.pump();
        assert_eq!(m.backend().border_calls, 0);
    }

    #[test]
    fn the_border_goes_round_the_focused_window() {
        let m = bordered();
        let border = m.backend().border.unwrap();
        assert_eq!(border.window, 2);
        assert_eq!(border.rect, m.backend().frame(2));
    }

    #[test]
    fn the_border_follows_focus() {
        let mut m = bordered();
        m.backend.focused = Some(1);
        m.refresh();
        m.pump();
        assert_eq!(m.backend().border.unwrap().window, 1);
    }

    #[test]
    fn the_border_moves_with_the_window_in_flight() {
        let mut m = bordered();
        m.backend.close(1);
        m.refresh();
        m.tick(1.0 / 60.0);
        m.tick(1.0 / 60.0);
        m.pump();
        assert!(m.is_animating());
        assert_eq!(m.backend().border.unwrap().rect, m.backend().frame(2));
    }

    #[test]
    fn a_still_border_is_left_alone() {
        let mut m = bordered();
        let calls = m.backend().border_calls;
        for _ in 0..10 {
            m.pump();
        }
        assert_eq!(m.backend().border_calls, calls);
    }

    #[test]
    fn the_border_goes_when_turned_off_or_the_window_closes() {
        let mut m = bordered();
        m.backend.close(2);
        m.backend.close(1);
        m.backend.focused = None;
        m.refresh();
        m.pump();
        assert_eq!(m.backend().border, None);
        let mut m = bordered();
        run(&mut m, "config border off").unwrap();
        m.pump();
        assert_eq!(m.backend().border, None);
    }

    #[test]
    fn the_border_follows_a_window_dragged_by_its_title_bar_at_full_speed() {
        let mut m = bordered();
        let frame = m.backend().frame(2);
        assert!(!m.is_held());
        m.backend.mouse = true;
        m.backend
            .server_frames
            .insert(2, Rect::new(40, 50, 300, 200));
        m.pump();
        assert!(m.is_held(), "frames follow the display");
        m.backend
            .server_frames
            .insert(2, Rect::new(70, 45, 300, 200));
        m.pump();
        let moved = Rect::new(frame.x + 30, frame.y - 5, frame.w, frame.h);
        assert_eq!(m.backend().border.unwrap().rect, moved);
        // A look at the windows mid-drag does not count the move twice.
        m.frames.insert(2, moved);
        m.pump();
        assert_eq!(m.backend().border.unwrap().rect, moved);
        m.frames.insert(2, frame);
        m.backend.mouse = false;
        m.pump();
        assert!(!m.is_held());
        assert_eq!(m.backend().border.unwrap().rect, frame);
    }

    #[test]
    fn a_click_leaves_the_border_where_it_is() {
        let mut m = bordered();
        let before = m.backend().border;
        let calls = m.backend().border_calls;
        // The window server's frame is a little off from the known one.
        let f = m.backend().frame(2);
        let off = Rect::new(f.x + 2, f.y - 1, f.w, f.h + 3);
        m.backend.mouse = true;
        m.backend.server_frames.insert(2, off);
        m.pump();
        m.pump();
        assert_eq!(m.backend().border, before);
        assert_eq!(m.backend().border_calls, calls);
    }

    #[test]
    fn an_app_can_have_corners_of_its_own() {
        let mut m = bordered();
        assert_eq!(m.backend().border.unwrap().radius, 12);
        run(&mut m, "rule border_radius 0 Notes").unwrap();
        m.pump();
        assert_eq!(m.backend().border.unwrap().radius, 0);
        m.backend.focused = Some(1);
        m.refresh();
        m.pump();
        assert_eq!(m.backend().border.unwrap().radius, 12);
    }

    #[test]
    fn float_at_a_cell_goes_there_and_back() {
        let mut m = manager(0.0);
        m.backend.open(1, "Terminal");
        m.backend.open(2, "Notes");
        m.refresh();
        let tiled = m.backend().frame(2);
        m.backend.focused = Some(2);
        run(&mut m, "float 6:6:1:1:4:4").unwrap();
        assert_eq!(m.tree().windows(), vec![1]);
        assert_eq!(m.backend().frame(2), Rect::new(166, 125, 667, 400));
        m.refresh();
        m.backend.focused = Some(2);
        run(&mut m, "float 6:6:1:1:4:4").unwrap();
        assert_eq!(m.tree().windows(), vec![1, 2]);
        assert_eq!(m.backend().frame(2), tiled);
    }

    #[test]
    fn zoom_covers_the_screen_for_a_floating_window_and_back() {
        let mut m = manager(0.0);
        m.backend.open(1, "Calculator");
        m.refresh();
        m.backend.focused = Some(1);
        let before = m.backend().frame(1);
        run(&mut m, "zoom").unwrap();
        assert_eq!(m.backend().frame(1), Rect::new(0, 25, 1000, 600));
        run(&mut m, "zoom").unwrap();
        assert_eq!(m.backend().frame(1), before);
    }

    #[test]
    fn a_floating_window_moved_after_zoom_zooms_afresh() {
        let mut m = manager(0.0);
        m.backend.open(1, "Calculator");
        m.refresh();
        m.backend.focused = Some(1);
        run(&mut m, "zoom").unwrap();
        run(&mut m, "grid 2:2:0:0:1:1").unwrap();
        let cell = m.backend().frame(1);
        run(&mut m, "zoom").unwrap();
        assert_eq!(m.backend().frame(1), Rect::new(0, 25, 1000, 600));
        run(&mut m, "zoom").unwrap();
        assert_eq!(m.backend().frame(1), cell);
    }

    #[test]
    fn query_spaces_lists_the_spaces_of_the_display() {
        let mut m = three_spaces();
        m.backend.open(1, "Terminal");
        m.refresh();
        assert_eq!(
            run(&mut m, "query --spaces").unwrap(),
            "[{\"index\":1,\"label\":\"general\",\"focused\":true,\"float\":false,\
             \"windows\":1},\
             {\"index\":2,\"label\":\"web\",\"focused\":false,\"float\":false,\
             \"windows\":0},\
             {\"index\":3,\"label\":\"coding\",\"focused\":false,\"float\":false,\
             \"windows\":0}]\n"
        );
    }

    #[test]
    fn the_border_hides_while_fingers_switch_spaces() {
        let mut m = bordered();
        assert!(m.backend().border.is_some());
        m.backend.swiping = true;
        m.pump();
        assert_eq!(m.backend().border, None);
        m.backend.swiping = false;
        m.pump();
        assert!(m.backend().border.is_some());
    }
}
