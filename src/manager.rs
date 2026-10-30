//! Ties the BSP tree, the animator and a window-system backend together.

use std::collections::{HashMap, HashSet};

use log::{debug, info, warn};

use crate::anim::Animator;
use crate::bsp::{self, BspTree, MinSizes, WinId};
use crate::command::{Command, Zoom};
use crate::config::{Config, MouseAction, app_matches};
use crate::geometry::{Direction, Rect};
use crate::platform::{Backend, Modifier, MouseButton, Overlay, WindowEvent, WindowInfo};

/// The layout of a Space (virtual desktop) that is not on screen.
#[derive(Debug, Default)]
struct SpaceState {
    tree: BspTree,
    focused: Option<WinId>,
    zoomed: Option<(WinId, Zoom)>,
}

pub struct Manager<B: Backend> {
    backend: B,
    config: Config,
    /// Layout of the Space on screen; `spaces` holds the others.
    tree: BspTree,
    active_space: u64,
    spaces: HashMap<u64, SpaceState>,
    anim: Animator,
    /// Last frame each window was seen at or sent to.
    frames: HashMap<WinId, Rect>,
    apps: HashMap<WinId, String>,
    /// Windows the user took out of the layout.
    floating: HashSet<WinId>,
    /// Windows reported closed that may still show up in the window list
    /// during their close animation; they must not be tiled again. Some
    /// apps (Electron ones like Claude or Slack) only hide a window closed
    /// with the red button and show the same window again when reopened,
    /// so a window is forgotten here as soon as it leaves the screen.
    closed: HashSet<WinId>,
    /// Last focused window that is part of the layout; new windows split it.
    focused: Option<WinId>,
    /// Smallest sizes windows turned out to accept: an app that refuses to
    /// shrink further keeps its size, and the layout must give it that.
    min_sizes: MinSizes,
    /// The drop highlight on screen, if any.
    shown_overlay: Option<Overlay>,
    /// The minimum size last learned for each app's windows, a guess for a
    /// new window of the app before it has refused a size itself.
    app_mins: HashMap<String, (i32, i32)>,
    /// The window tiled last, watched for a moment: if it turns out not to
    /// fit after all, it is the one that floats.
    last_tiled: Option<LastTiled>,
    /// Each window's frame when bspx first saw it: the size it floats at
    /// when there is no room to tile it.
    opened: HashMap<WinId, Rect>,
    /// What the system last said had focus; see `sync_focus`.
    reported_focus: Option<WinId>,
    /// The window focused before the system's focus last moved, and when
    /// (`clock`): see `split_target`.
    focus_moved: Option<(WinId, f64)>,
    /// The window `bspx zoom` grew over the others on this Space, if any.
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
    /// Those of them that are real document windows bspx can manage (the
    /// rest: dialogs, and the invisible helper windows many apps and tools
    /// keep on screen).
    manageable: HashSet<WinId>,
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
    /// under the pointer, which it would swap with if dropped now.
    Moving {
        win: WinId,
        target: Option<WinId>,
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

/// The smallest a window can be resized to by a mouse gesture.
const MIN_GESTURE_SIZE: i32 = 50;

/// Frame differences below this many points are noise (e.g. apps rounding
/// their size to a character grid), not a user's drag.
const DRAG_THRESHOLD: i32 = 4;

/// A window this many points larger than its slot is taken to have hit its
/// minimum size. Large enough that an app rounding its size up to whole
/// text cells is not mistaken for one.
const MIN_SIZE_THRESHOLD: i32 = 24;

/// A window just tiled, and what it was tiled into.
#[derive(Clone)]
struct LastTiled {
    id: WinId,
    /// The window it split, if any.
    target: Option<WinId>,
    at: f64,
    /// The tree right after it went in. Once the user changes it (a swap,
    /// a warp, a resize...), the window is no longer judged.
    tree: BspTree,
    /// Where every window went then.
    layout: Vec<(WinId, Rect)>,
}

/// How long after it was tiled a window that turns out not to fit (its
/// minimum size was not known yet) is taken out of the layout again.
const NO_ROOM_SECS: f64 = 3.0;

impl<B: Backend> Manager<B> {
    pub fn new(backend: B, config: Config) -> Self {
        let mut anim = Animator::new(config.animation_speed);
        anim.set_easing(config.animation_easing);
        Self {
            backend,
            config,
            tree: BspTree::new(),
            active_space: 0,
            spaces: HashMap::new(),
            anim,
            frames: HashMap::new(),
            apps: HashMap::new(),
            floating: HashSet::new(),
            closed: HashSet::new(),
            focused: None,
            min_sizes: MinSizes::new(),
            shown_overlay: None,
            app_mins: HashMap::new(),
            last_tiled: None,
            opened: HashMap::new(),
            reported_focus: None,
            focus_moved: None,
            zoomed: None,
            area: Rect::default(),
            mouse_was_down: false,
            drag: Drag::None,
            before_drag: HashMap::new(),
            stack: Vec::new(),
            manageable: HashSet::new(),
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

    /// `bspx ...` command lines from keyboard shortcuts.
    pub fn hotkey_commands(&mut self) -> Vec<String> {
        self.backend.hotkey_commands()
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

    pub fn is_animating(&self) -> bool {
        !self.anim.is_idle()
    }

    /// Applies new settings. Windows of apps that became floating leave the
    /// layout right away; those that may be tiled again join at the next poll.
    pub fn set_config(&mut self, config: Config) {
        if let Some(level) = config.log_level {
            log::set_max_level(level);
        }
        self.anim.set_speed(config.animation_speed);
        self.anim.set_easing(config.animation_easing);
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
                self.anim.cancel(id);
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

        let area = self.backend.tiling_area();
        if area != self.area {
            self.area = area;
            dirty = true;
        }

        let space = self.backend.active_space();
        if space != self.active_space {
            self.switch_space(space);
            dirty = true;
        }

        let focused_now = self.sync_focus();

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
        let switching = windows
            .iter()
            .any(|w| spaces.get(&w.id).is_some_and(|s| *s != space))
            || (!self.tree.is_empty()
                && !self.tree.windows().iter().any(|w| on_screen.contains(w)));

        // Floating Spaces have no layout; their windows stay where they are.
        let float_spaces = self.float_space_ids();
        if float_spaces.contains(&space) && !self.tree.is_empty() {
            debug!("Space {space} floats: leaving its windows be");
            for id in self.tree.windows() {
                self.anim.cancel(id);
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
                match spaces.get(&id) {
                    None => Some("minimised or hidden"),
                    Some(&s) if s != tree_space => {
                        moved.push((s, id));
                        Some("moved to another Space")
                    }
                    Some(_) if tree_space == space && !on_screen.contains(&id) && !switching => {
                        Some("minimised or hidden")
                    }
                    Some(_) => None,
                }
            };
            if let Some(reason) = leave {
                debug!("window {id} leaves the layout of Space {tree_space}: {reason}");
                self.remove_from_space(tree_space, id);
                dirty |= tree_space == space;
            }
        }
        // A window moved to another Space joins that Space's layout. One
        // moved to this Space is picked up below, once it is on screen.
        for (s, id) in moved {
            if s != space && !float_spaces.contains(&s) {
                let (inner, gap, ratio) =
                    (self.inner_area(), self.config.gap, self.config.split_ratio);
                let state = self.spaces.entry(s).or_default();
                state.tree.insert(id, state.focused, inner, gap, ratio);
            }
        }

        self.floating.retain(|w| alive.contains(w));
        if !self.synced {
            self.synced = true;
            self.seen.extend(alive.iter().copied());
        }
        self.seen.retain(|w| alive.contains(w));
        self.closed.retain(|w| on_screen.contains(w));
        self.frames.retain(|w, _| on_screen.contains(w));
        self.apps.retain(|w, _| alive.contains(w));
        self.min_sizes.retain(|w, _| alive.contains(w));
        self.opened.retain(|w, _| alive.contains(w));
        if self.focused.is_some_and(|f| !self.tree.contains(f)) {
            self.focused = None;
        }

        // Where the windows were sent, to see which ones would not go.
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
        let mut new = Vec::new();
        for info in windows {
            self.opened.entry(info.id).or_insert(info.frame);
            if !self.anim.is_animating(info.id) {
                self.frames.insert(info.id, info.frame);
                if let Some(&slot) = expected.get(&info.id) {
                    dirty |= self.learn_min_size(info.id, slot, info.frame);
                }
            }
            self.apps.insert(info.id, info.app.clone());
            // A window counts as seen once bspx can manage it: a new one is
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
            let wanted = info.tileable
                && !float_spaces.contains(&space)
                && spaces.get(&info.id) == Some(&space)
                && !self.floating.contains(&info.id)
                && !self.closed.contains(&info.id)
                && !self
                    .config
                    .float_apps
                    .iter()
                    .any(|a| app_matches(a, &info.app));
            if wanted && !self.is_tiled_anywhere(info.id) {
                new.push(info.id);
            }
        }
        new.sort_by_key(|w| Some(*w) == focused_now);
        for id in new {
            let target = self.split_target(id);
            if !self.tile_where_it_fits(id, target) {
                self.float_in_the_middle(id);
                continue;
            }
            if focused_now == Some(id) || self.focused.is_none() {
                self.focused = Some(id);
            }
            dirty = true;
        }

        // A window tiled before its minimum size was known may have turned
        // out too big for its half: the other windows make room for it, or
        // it floats when they cannot.
        if let Some(last) = self.last_tiled.clone() {
            let id = last.id;
            let without = {
                let mut t = self.tree.clone();
                t.remove(id);
                t
            };
            let done = self.clock - last.at > NO_ROOM_SECS || self.tree != last.tree;
            // Learn what every window that was resized for it really took,
            // even a little more than it was given.
            let over = !done
                && self.tree.len() > 1
                && self.targets() == last.layout
                && self.split_overflows(&without, id, &expected);
            if done {
                self.last_tiled = None;
            } else if self.tree.len() > 1
                && (over || !self.splits_in_place(&without, &self.tree, id))
            {
                let mut tree = self.tree.clone();
                if self.outgrows(id, last.target) && self.make_room(&mut tree) {
                    if tree != self.tree {
                        debug!("making room for window {id}");
                        self.tree = tree;
                        dirty = true;
                    }
                    // Still watched, in case it refuses more.
                    self.last_tiled = Some(LastTiled {
                        tree: self.tree.clone(),
                        layout: self.targets(),
                        ..last
                    });
                } else {
                    self.last_tiled = None;
                    self.tree.remove(id);
                    self.anim.cancel(id);
                    if self.focused == Some(id) {
                        self.focused = self.tree.windows().last().copied();
                    }
                    self.float_in_the_middle(id);
                    dirty = true;
                }
            }
        }

        if dirty {
            self.relayout();
        }
    }

    /// Tiles `id` by splitting `target` (or the last window). When the two
    /// halves are too small for them and `id` is bigger than `target`, the
    /// other windows give up some of their room, down to their minimum
    /// sizes. Returns `false`, leaving the layout alone, if `id` does not
    /// fit: a part of the screen filling up with windows of one size does
    /// not grow into the rest of it.
    fn tile_where_it_fits(&mut self, id: WinId, target: Option<WinId>) -> bool {
        let (inner, gap, ratio) = (self.inner_area(), self.config.gap, self.config.split_ratio);
        if self.tree.is_empty() {
            self.tree.insert(id, None, inner, gap, ratio);
            self.watch_tiled(id, None);
            return true;
        }
        // The window `insert` splits.
        let target = target
            .filter(|t| self.tree.contains(*t))
            .or_else(|| self.tree.windows().last().copied());
        let mut tree = self.tree.clone();
        tree.insert(id, target, inner, gap, ratio);
        if self.splits_in_place(&self.tree, &tree, id) {
            debug!("tiling window {id} next to {target:?}");
        } else if self.outgrows(id, target) && self.make_room(&mut tree) {
            debug!("tiling window {id} next to {target:?}, making room for it");
        } else {
            return false;
        }
        self.tree = tree;
        self.watch_tiled(id, target);
        true
    }

    /// Whether `id` needs more room than `target`, the window it splits:
    /// a larger minimum width or height, as far as they are known. Only such
    /// a window makes the others give up room.
    fn outgrows(&self, id: WinId, target: Option<WinId>) -> bool {
        let Some(target) = target else {
            return false;
        };
        let mins = self.likely_min_sizes(&[id, target]);
        let (new, old) = (mins[&id], mins[&target]);
        new.0 > old.0 || new.1 > old.1
    }

    /// Resizes the splits of `tree` so every window gets at least its
    /// minimum size, as far as it is known. Returns `false`, leaving `tree`
    /// alone, when the windows do not fit on the screen however it is split.
    fn make_room(&self, tree: &mut BspTree) -> bool {
        let (inner, gap) = (self.inner_area(), self.config.gap);
        let mins = self.likely_min_sizes(&tree.windows());
        if !tree.fits(inner, gap, &mins) {
            return false;
        }
        tree.settle_ratios(inner, gap, &mins);
        true
    }

    /// Watches `id`, just tiled, for a moment: see [`LastTiled`].
    fn watch_tiled(&mut self, id: WinId, target: Option<WinId>) {
        self.last_tiled = Some(LastTiled {
            id,
            target,
            at: self.clock,
            tree: self.tree.clone(),
            layout: self.targets(),
        });
    }

    /// Whether adding `id` turned `without` into `with` by splitting one
    /// window's slot alone, both halves no smaller than they can be: every
    /// other window keeps its place and size. A part of the screen the user
    /// tiles into never grows into the rest of it.
    fn splits_in_place(&self, without: &BspTree, with: &BspTree, id: WinId) -> bool {
        let (inner, gap) = (self.inner_area(), self.config.gap);
        let mins = self.likely_min_sizes(&with.windows());
        let before: HashMap<WinId, Rect> =
            without.layout_with(inner, gap, &mins).into_iter().collect();
        let mut changed = 0;
        for (w, r) in with.layout_with(inner, gap, &mins) {
            let (min_w, min_h) = mins.get(&w).copied().unwrap_or((0, 0));
            if r.w < min_w || r.h < min_h {
                return false;
            }
            if w != id && before.get(&w) != Some(&r) {
                changed += 1;
            }
        }
        // Only the window that was split.
        changed <= 1
    }

    /// Whether `id`, tiled last, or the window it split stayed larger than
    /// its slot, even by a little: then the two do not fit, and the size
    /// kept is remembered for the app's next windows. (A larger difference
    /// is learned as a minimum size anyway; a small one is otherwise taken
    /// for an app rounding its size, but here it would eat the gap.)
    ///
    /// Only a window that was sent its slot before this poll is judged, by
    /// the size it has come back with.
    fn split_overflows(
        &mut self,
        without: &BspTree,
        id: WinId,
        sent: &HashMap<WinId, Rect>,
    ) -> bool {
        if !self.anim.is_idle() || self.drag != Drag::None {
            return false;
        }
        let before: HashMap<WinId, Rect> = without
            .layout_with(self.inner_area(), self.config.gap, &self.min_sizes)
            .into_iter()
            .collect();
        let mut over = false;
        for (w, slot) in self.targets() {
            if w != id && before.get(&w) == Some(&slot) || sent.get(&w) != Some(&slot) {
                continue;
            }
            let Some(&f) = self.frames.get(&w) else {
                continue;
            };
            // Apps that size to a text grid round a little either way (a
            // Terminal asked for 453 points of height keeps 455); that may
            // take up to half the gap, so that a gap is always left.
            let slack = (self.config.gap / 2).max(2);
            let (wider, taller) = (f.w > slot.w + slack, f.h > slot.h + slack);
            if !(wider || taller) {
                continue;
            }
            over = true;
            debug!(
                "window {w} stayed {}x{} in a {}x{} slot",
                f.w, f.h, slot.w, slot.h
            );
            if let Some(app) = self.apps.get(&w) {
                let known = self.app_mins.entry(app.clone()).or_default();
                if wider {
                    known.0 = known.0.max(f.w);
                }
                if taller {
                    known.1 = known.1.max(f.h);
                }
            }
        }
        over
    }

    /// The minimum sizes of `ids` as far as they are known: what each window
    /// refused, or what other windows of its app did, whichever is larger.
    fn likely_min_sizes(&self, ids: &[WinId]) -> MinSizes {
        ids.iter()
            .map(|id| {
                let own = self.min_sizes.get(id).copied().unwrap_or((0, 0));
                let app = self
                    .apps
                    .get(id)
                    .and_then(|a| self.app_mins.get(a))
                    .copied()
                    .unwrap_or((0, 0));
                (*id, (own.0.max(app.0), own.1.max(app.1)))
            })
            .collect()
    }

    /// Floats `id` in the middle of the screen, at its own size: there is
    /// no room left to tile it.
    fn float_in_the_middle(&mut self, id: WinId) {
        info!("no room to tile window {id}: it floats in the middle");
        self.floating.insert(id);
        let Some(now) = self.frames.get(&id).copied() else {
            return;
        };
        // At the size it opened with, not one it was squeezed to meanwhile.
        let size = self.opened.get(&id).copied().unwrap_or(now);
        let area = self.inner_area();
        let frame = area.align_in(area, size.w.min(area.w), size.h.min(area.h));
        if frame != now {
            self.backend.update_frame(id, frame, now);
            self.frames.insert(id, frame);
        }
    }

    /// The ids of the Spaces set to float.
    fn float_space_ids(&mut self) -> HashSet<u64> {
        if self.config.float_spaces.is_empty() {
            return HashSet::new();
        }
        let spaces = self.backend.spaces();
        self.config
            .float_spaces
            .iter()
            .filter_map(|s| self.config.space_number(s))
            .filter_map(|n| spaces.get(n - 1).copied())
            .collect()
    }

    /// The Space number and id `space` (a number or a label) stands for.
    fn resolve_space(&mut self, space: &str) -> Result<(usize, u64), String> {
        let n = self
            .config
            .space_number(space)
            .ok_or_else(|| format!("no Space is labelled '{space}'"))?;
        let id = *self
            .backend
            .spaces()
            .get(n - 1)
            .ok_or_else(|| format!("there is no Space {n}"))?;
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
        self.anim.cancel(id);
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
        if self.backend.move_to_space(id, sid) {
            info!("window {id} of {app} sent to Space {n}");
            true
        } else {
            warn!("could not send window {id} of {app} to Space {n}");
            false
        }
    }

    /// Compares where a settled window is with the slot it was sent to. One
    /// that stayed larger has hit its minimum size; one that has since gone
    /// below what was learned can shrink further after all. Returns whether
    /// the layout must change.
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
        if let Some(app) = self.apps.get(&id) {
            // Each window may have shown only one side of its minimum.
            let known = self.app_mins.entry(app.clone()).or_default();
            *known = (known.0.max(min.0), known.1.max(min.1));
        }
        true
    }

    /// Reacts to window events right away. A closed window leaves the layout
    /// at once, so its neighbours fill the gap without waiting for the next
    /// poll (animated, or instantly with `animation 0`). Returns whether the
    /// window list should be read again now.
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
                        dirty |= space == self.active_space;
                        if self.focused == Some(id) {
                            self.focused = self.tree.windows().last().copied();
                        }
                    }
                    if self.drag != Drag::None {
                        self.drag = Drag::Other;
                        self.set_overlay(None);
                    }
                }
                WindowEvent::Changed => refresh = true,
            }
        }
        if dirty {
            self.relayout();
        }
        refresh
    }

    /// Puts the current layout away and brings out the one of `space`.
    fn switch_space(&mut self, space: u64) {
        debug!("Space {} -> {space}", self.active_space);
        for id in self.tree.windows() {
            self.anim.cancel(id);
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
    /// must not undo a focus change bspx saw first. Returns the answer.
    fn sync_focus(&mut self) -> Option<WinId> {
        let now = self.backend.focused_window();
        if now != self.reported_focus {
            self.reported_focus = now;
            if let Some(f) = now.filter(|f| self.tree.contains(*f)) {
                if let Some(prev) = self.focused.filter(|p| *p != f) {
                    self.focus_moved = Some((prev, self.clock));
                }
                self.focused = Some(f);
            }
        }
        now
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
        if space == self.active_space {
            self.tree.remove(id);
            self.anim.cancel(id);
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
        let tiled = self.tree.contains(win);
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
                .filter(|id| self.manageable.contains(id) && !self.tree.contains(**id))
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
        if tiled {
            self.focused = Some(win);
        }
    }

    /// The frontmost window bspx manages at a point, from the last poll.
    /// Helper windows that only look like windows are looked through.
    fn window_under(&self, x: f32, y: f32) -> Option<WinId> {
        self.stack
            .iter()
            .copied()
            .filter(|id| self.manageable.contains(id) || self.tree.contains(*id))
            .find(|id| self.frames.get(id).is_some_and(|f| f.contains_point(x, y)))
    }

    /// Mouse follows focus: puts the pointer in the middle of `win`'s slot,
    /// unless it is already on it.
    fn warp_to(&mut self, win: WinId) {
        if !self.config.mouse_follows_focus {
            return;
        }
        let Some((_, rect)) = self.targets().into_iter().find(|(id, _)| *id == win) else {
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
        self.backend.pump();
    }

    fn press(&mut self) {
        // Where everything was before the user touched it.
        self.before_drag.clone_from(&self.frames);
        let grabbed = self.backend.cursor().and_then(|(x, y)| {
            let hit = |id: &WinId| self.frames.get(id).is_some_and(|f| f.contains_point(x, y));
            // A zoomed window is in front of the ones it covers.
            let zoomed = self.zoomed.map(|(w, _)| w).filter(hit);
            zoomed.or_else(|| {
                self.tree
                    .windows()
                    .into_iter()
                    .filter(|id| !self.anim.is_animating(*id))
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
                    self.drag = Drag::Moving { win, target: None };
                    self.dragging();
                }
            }
            Drag::Moving { win, target } => {
                let Some(cursor) = self.backend.cursor() else {
                    return;
                };
                let target = self.show_drop_target(win, (target, None), cursor, false).0;
                self.drag = Drag::Moving { win, target };
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
        let tiled = self.tree.contains(win);
        if tiled && self.zoomed.is_some_and(|(w, _)| w == win) && action == MouseAction::Resize {
            // Resizing works on the split borders, which a zoom hides.
            self.zoomed = None;
            self.relayout();
        }
        self.anim.cancel(win);
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
                    self.relayout_now();
                } else {
                    let prev = self.frames.get(&grab.win).copied().unwrap_or(f);
                    if frame != prev {
                        self.backend.update_frame(grab.win, frame, prev);
                        self.frames.insert(grab.win, frame);
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
            let target = grab.target.filter(|t| self.tree.contains(*t));
            if let Some(other) = target.filter(|_| self.tree.contains(grab.win)) {
                match grab.side {
                    Some(side) => {
                        info!("putting window {} {side:?} of {other}", grab.win);
                        if self
                            .zoomed
                            .is_some_and(|(w, _)| w == grab.win || w == other)
                        {
                            self.zoomed = None;
                        }
                        self.tree.remove(grab.win);
                        self.tree.insert_at(grab.win, other, side);
                    }
                    None => {
                        info!("swapping window {} with {other}", grab.win);
                        self.tree.swap(grab.win, other);
                    }
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
            .targets()
            .into_iter()
            .filter(|(id, _)| *id != win)
            .map(|(id, slot)| (id, slot, self.drawn_frame(id, slot)))
            .filter(|(_, _, frame)| frame.contains_point(x, y))
            // Where frames overlap, the window whose slot it is wins.
            .max_by_key(|(_, slot, _)| slot.contains_point(x, y));
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
        if self.anim.is_animating(id) {
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
            Drag::Moving { win, target } => {
                self.set_overlay(None);
                // Animate from where the user let go.
                if let Some(now) = self.backend.window_frame(win) {
                    self.frames.insert(win, now);
                }
                match target {
                    Some(other) => {
                        info!("swapping window {win} with {other}");
                        self.tree.swap(win, other);
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
            .filter(|id| !self.anim.is_animating(*id))
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
        // Each edge the user dragged becomes a border position in the tree.
        let (area, gap) = (self.inner_area(), self.config.gap);
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
        debug!("window {id} resized by mouse");
        true
    }

    /// Advances animations by `dt` seconds and pushes the new frames.
    pub fn tick(&mut self, dt: f64) {
        self.clock += dt;
        for (id, frame) in self.anim.step(dt) {
            match self.frames.insert(id, frame) {
                Some(prev) => self.backend.update_frame(id, frame, prev),
                None => self.backend.set_frame(id, frame),
            }
        }
    }

    fn inner_area(&self) -> Rect {
        self.area.inset(&self.config.padding)
    }

    /// Where every tiled window belongs: its slot in the tree, or the zoomed
    /// window's larger area.
    fn targets(&self) -> Vec<(WinId, Rect)> {
        let area = self.inner_area();
        // The minimum sizes the room was made with, including what other
        // windows of the same app refused.
        let mins = self.likely_min_sizes(&self.tree.windows());
        let mut targets = self.tree.layout_with(area, self.config.gap, &mins);
        if let Some(zoomed) = self.zoom_rect() {
            for t in &mut targets {
                if t.0 == zoomed.0 {
                    t.1 = zoomed.1;
                }
            }
        }
        // A window that will stay larger than its slot is moved back so
        // that it stays on screen (else macOS pushes it somewhere).
        for (id, r) in &mut targets {
            if let Some(&(min_w, min_h)) = self.min_sizes.get(id) {
                if r.w < min_w {
                    r.x = r.x.min(area.max_x() - min_w).max(area.x);
                }
                if r.h < min_h {
                    r.y = r.y.min(area.max_y() - min_h).max(area.y);
                }
            }
        }
        targets
    }

    /// The zoomed window and the area it covers.
    fn zoom_rect(&self) -> Option<(WinId, Rect)> {
        let (win, zoom) = self.zoomed?;
        let area = self.inner_area();
        let rect = match zoom {
            Zoom::Fullscreen => self.tree.contains(win).then_some(area)?,
            Zoom::Parent => self
                .tree
                .parent_rect(win, area, self.config.gap, &self.min_sizes)?,
        };
        Some((win, rect))
    }

    /// Frames as the tree lays them out, ignoring zoom: what the neighbours
    /// of a window are, for `focus` and `swap`.
    fn slots(&self) -> Vec<(WinId, Rect)> {
        self.tree
            .layout_with(self.inner_area(), self.config.gap, &self.min_sizes)
    }

    /// Sends every tiled window toward its slot in the tree.
    /// Puts every tiled window in its slot at once, without animating:
    /// while the user drags a border, the windows must keep up with it.
    fn relayout_now(&mut self) {
        for (id, target) in self.targets() {
            self.anim.cancel(id);
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
        for (id, target) in self.targets() {
            if Some(id) == held {
                continue;
            }
            let current = self.frames.get(&id).copied().unwrap_or(target);
            if let Some(frame) = self.anim.animate_to(id, current, target) {
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
                let to =
                    bsp::neighbor(&self.slots(), from, dir).ok_or("no window in that direction")?;
                self.backend.focus(to);
                self.focused = Some(to);
                self.warp_to(to);
            }
            Command::Swap(dir) => {
                let from = focused()?;
                let to =
                    bsp::neighbor(&self.slots(), from, dir).ok_or("no window in that direction")?;
                self.tree.swap(from, to);
                self.warp_to(from);
            }
            Command::Warp(dir) => {
                let from = focused()?;
                let to =
                    bsp::neighbor(&self.slots(), from, dir).ok_or("no window in that direction")?;
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
                // Every window must fit its plain share: the layout may not
                // take room from other windows to make it work.
                let mins = self.likely_min_sizes(&tree.windows());
                let room = tree
                    .layout(self.inner_area(), self.config.gap)
                    .into_iter()
                    .all(|(w, r)| {
                        let (min_w, min_h) = mins.get(&w).copied().unwrap_or((0, 0));
                        r.w >= min_w && r.h >= min_h
                    });
                if !room {
                    return Err("no room for this window there".into());
                }
                if self.zoomed.is_some_and(|(w, _)| w == from || w == to) {
                    self.zoomed = None;
                }
                info!("window {from} warped {dir:?} into window {to}'s place");
                self.tree = tree;
                self.warp_to(from);
            }
            Command::Resize(dir, amount) => {
                let win = focused()?;
                let amount = amount.unwrap_or(self.config.resize_step);
                let (area, gap) = (self.inner_area(), self.config.gap);
                // Start from the shares windows really have, so a step
                // always moves the border from where it is on screen.
                self.tree.settle_ratios(area, gap, &self.min_sizes);
                let before = self.tree.clone();
                if !self.tree.resize(win, dir, amount) {
                    return Err("nothing to resize in that direction".into());
                }
                self.tree.settle_ratios(area, gap, &self.min_sizes);
                if self.slots() == before.layout_with(area, gap, &self.min_sizes) {
                    self.tree = before;
                    return Err("a window there is at its minimum size".into());
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
                let win = focused()?;
                if self.zoomed == Some((win, zoom)) {
                    self.zoomed = None;
                } else {
                    if zoom == Zoom::Parent
                        && self
                            .tree
                            .parent_rect(win, self.inner_area(), self.config.gap, &self.min_sizes)
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
                    self.anim.cancel(win);
                    self.focused = self.tree.windows().last().copied();
                    info!("window {win} floating");
                } else {
                    if self.float_space_ids().contains(&self.active_space) {
                        return Err(
                            "this Space floats; tile it with `bspx layout <space> bsp`".into()
                        );
                    }
                    if !self.tile_where_it_fits(win, self.focused) {
                        return Err("no room to tile this window".into());
                    }
                    self.floating.remove(&win);
                    self.focused = Some(win);
                    info!("window {win} tiled");
                }
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
                let spaces = self.backend.spaces();
                let current = self.backend.active_space();
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
            Command::Layout { ref space, float } => {
                let (n, _) = self.resolve_space(space)?;
                self.config.set_float_space(space, float);
                info!("Space {n} {}", if float { "floats" } else { "is tiled" });
                // Lets go of the windows, or tiles them, right away.
                self.refresh();
            }
            Command::Space(ref space) => {
                let (n, sid) = self.resolve_space(space)?;
                if sid != self.backend.active_space() {
                    self.backend.switch_to_space(n)?;
                }
                return Ok(String::new());
            }
            Command::Reload | Command::Quit => return Err("handled by the daemon".into()),
        }
        self.relayout();
        Ok(String::new())
    }

    /// One line per window: `id app x,y wxh [tiled|floating] [focused]`.
    pub fn describe(&self) -> String {
        let mut ids: Vec<WinId> = self.frames.keys().copied().collect();
        ids.sort_unstable();
        let mut out = String::new();
        for id in ids {
            let f = self.anim.target(id).unwrap_or(self.frames[&id]);
            let state = if self.zoomed.is_some_and(|(w, _)| w == id) {
                "zoomed"
            } else if self.tree.contains(id) {
                "tiled"
            } else {
                "floating"
            };
            let focus = if self.focused == Some(id) {
                " focused"
            } else {
                ""
            };
            let app = self.apps.get(&id).map_or("?", String::as_str);
            // Which `bspx rule float` floats the window, so a rule that
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
        /// The gesture modifier bspx asked for.
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
            let shown = self.screen_space.unwrap_or(self.active);
            self.windows
                .iter()
                .filter(|w| self.space_of.get(&w.id).copied().unwrap_or(0) == shown)
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
        fn set_frame(&mut self, id: WinId, frame: Rect) {
            let (min_w, min_h) = self.min.get(&id).copied().unwrap_or((0, 0));
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
        fn move_to_space(&mut self, id: WinId, space: u64) -> bool {
            if !self.spaces().contains(&space) {
                return false;
            }
            self.space_of.insert(id, space);
            true
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
        fn show_overlay(&mut self, overlay: Option<Overlay>) {
            self.overlay = overlay;
            self.overlay_changes += 1;
        }
        fn focus(&mut self, id: WinId) {
            self.focus_calls.push(id);
            self.focused = Some(id);
        }
    }

    fn manager(speed: f64) -> Manager<MockBackend> {
        let backend = MockBackend {
            area: Rect::new(0, 25, 1000, 600),
            ..Default::default()
        };
        let config = Config {
            gap: 10,
            padding: Padding::default(),
            animation_speed: speed,
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
        let mut m = manager(12.0);
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
    /// letting bspx see each one settle.
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
    fn windows_that_do_not_fit_float_in_the_middle() {
        let mut m = manager(0.0);
        open_many(&mut m, 1, 12, "Terminal", (300, 180));
        let tiled = m.tree().windows();
        assert!(tiled.len() >= 3, "the screen is used: {tiled:?}");
        assert!(tiled.len() < 12, "not all fit: {tiled:?}");
        assert_tidy(&m);
        // The rest float in the middle, at their own size.
        for id in 1..=12 {
            if tiled.contains(&id) {
                continue;
            }
            let f = m.backend().frame(id);
            assert!(f.w >= 300 && f.h >= 180, "window {id}: {f:?}");
            let (cx, cy) = f.center();
            assert!(
                (cx - 500.0).abs() <= 1.0 && (cy - 325.0).abs() <= 1.0,
                "window {id}: {f:?}"
            );
        }
        // Closing a tiled window makes room for one floated by hand.
        let floating = (1..=12).find(|id| !tiled.contains(id)).unwrap();
        m.backend.close(tiled[0]);
        m.refresh();
        settle(&mut m);
        m.backend.focused = Some(floating);
        m.execute(&Command::ToggleFloat).unwrap();
        m.refresh();
        settle(&mut m);
        assert!(m.tree().contains(floating));
        assert_tidy(&m);
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
        // Terminals opened from there, as many as you like.
        for id in 4..16 {
            open_many(&mut m, id, 1, "Terminal", (200, 100));
        }
        assert_eq!(m.backend().frame(1), safari, "the left side is untouched");
        assert_eq!(m.backend().frame(2), notes, "the top right is untouched");
        let tiled = m.tree().windows();
        assert!(tiled.len() > 3, "the quarter is used: {tiled:?}");
        for id in tiled.iter().filter(|w| **w > 2) {
            let f = m.backend().frame(*id);
            assert!(
                f.x >= quarter.x
                    && f.y >= quarter.y
                    && f.max_x() <= quarter.max_x()
                    && f.max_y() <= quarter.max_y(),
                "window {id} left the quarter: {f:?}"
            );
        }
        // The rest float in the middle.
        for id in (4..16).filter(|w| !tiled.contains(w)) {
            let (cx, cy) = m.backend().frame(id).center();
            assert!(
                (cx - 500.0).abs() <= 1.0 && (cy - 325.0).abs() <= 1.0,
                "window {id}"
            );
        }
        assert_tidy(&m);
    }

    #[test]
    fn a_big_window_next_to_a_full_screen_one_gets_room() {
        let mut m = manager(0.0);
        open_many(&mut m, 1, 1, "Terminal", (300, 180));
        open_many(&mut m, 2, 1, "WhatsApp", (600, 400));
        assert!(m.tree().contains(2), "WhatsApp is tiled, not floating");
        assert!(m.backend().frame(1).w >= 300);
        assert!(m.backend().frame(2).w >= 600);
        assert_tidy(&m);
    }

    #[test]
    fn windows_of_one_size_do_not_grow_their_part_after_a_big_one() {
        // WhatsApp makes room next to two Terminals; Terminals opened next
        // to it afterwards float rather than take more.
        let mut m = manager(0.0);
        open_many(&mut m, 1, 2, "Terminal", (300, 180));
        open_many(&mut m, 3, 1, "WhatsApp", (600, 400));
        assert!(m.tree().contains(3));
        m.backend.focused = Some(1);
        m.refresh();
        let mut floated = false;
        for id in 4..12 {
            let before: Vec<_> = (1..id).map(|w| m.backend().frame(w)).collect();
            open_many(&mut m, id, 1, "Terminal", (300, 180));
            if !m.tree().contains(id) {
                let after: Vec<_> = (1..id).map(|w| m.backend().frame(w)).collect();
                assert_eq!(after, before, "the layout is left alone");
                floated = true;
                break;
            }
        }
        assert!(floated, "the Terminals ran out of room");
        assert!(m.tree().contains(3), "WhatsApp keeps its place");
        assert!(m.backend().frame(3).w >= 600);
        assert_tidy(&m);
    }

    #[test]
    fn a_big_window_next_to_two_halves_gets_room() {
        // Two Terminals side by side, then WhatsApp, whose minimum size is
        // larger than the half it splits.
        let mut m = manager(0.0);
        open_many(&mut m, 1, 2, "Terminal", (300, 180));
        assert_eq!(m.tree().len(), 2);
        open_many(&mut m, 3, 1, "WhatsApp", (600, 400));
        assert!(m.tree().contains(3), "WhatsApp is tiled, not floating");
        for (id, (w, h)) in [(1, (300, 180)), (2, (300, 180)), (3, (600, 400))] {
            let f = m.backend().frame(id);
            assert!(f.w >= w && f.h >= h, "window {id}: {f:?}");
        }
        assert_tidy(&m);
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
    fn a_window_a_little_too_big_for_its_half_floats_at_its_own_size() {
        let mut m = manager(0.0);
        open_many(&mut m, 1, 1, "Safari", (0, 0));
        open_many(&mut m, 2, 1, "Notes", (0, 0));
        open_many(&mut m, 3, 1, "Notes", (0, 0));
        // 242x295, then 242x142 halves: 8 points short of what Terminal
        // takes, too little to count as a minimum size.
        open_many(&mut m, 4, 1, "Terminal", (200, 150));
        let fourth = m.backend().frame(4);
        open_many(&mut m, 5, 1, "Terminal", (200, 150));
        assert!(!m.tree().contains(5), "the gap is not eaten");
        assert_eq!(m.backend().frame(4), fourth, "the other half is given back");
        assert_eq!(
            m.backend().frame(5),
            Rect::new(350, 225, 300, 200),
            "in the middle, at the size it opened with"
        );
        assert_tidy(&m);
        // The next Terminal does not even try.
        m.backend.focused = Some(4);
        m.refresh();
        let before = m.backend().overlay_changes;
        open_many(&mut m, 6, 1, "Terminal", (200, 150));
        assert!(!m.tree().contains(6));
        assert_eq!(m.backend().frame(4), fourth);
        assert_eq!(m.backend().overlay_changes, before);
        assert_tidy(&m);
    }

    #[test]
    fn a_full_screen_refuses_to_tile_by_hand() {
        let mut m = manager(0.0);
        open_many(&mut m, 1, 12, "Terminal", (300, 180));
        let floating = (1..=12).find(|id| !m.tree().contains(*id)).unwrap();
        let before = m.tree().windows();
        m.backend.focused = Some(floating);
        assert!(m.execute(&Command::ToggleFloat).is_err());
        assert_eq!(m.tree().windows(), before);
    }

    #[test]
    fn a_new_window_only_splits_the_focused_one() {
        let mut m = manager(0.0);
        // A big Safari, then Terminals, each splitting the one before it,
        // until one does not fit next to the focused Terminal.
        open_many(&mut m, 1, 1, "Safari", (0, 0));
        let mut floating = None;
        for id in 2..20 {
            let before = m.tree().windows();
            open_many(&mut m, id, 1, "Terminal", (300, 180));
            if !m.tree().contains(id) {
                assert_eq!(m.tree().windows(), before, "the layout is left alone");
                floating = Some(id);
                break;
            }
        }
        let floating = floating.expect("the Terminals ran out of room");
        let (cx, cy) = m.backend().frame(floating).center();
        assert!((cx - 500.0).abs() <= 1.0 && (cy - 325.0).abs() <= 1.0);
        assert_tidy(&m);
        // There was room next to a big window all along; it is used when
        // that window is focused.
        let wide = *m
            .tree()
            .windows()
            .iter()
            .find(|w| {
                let f = m.backend().frame(**w);
                f.w >= 2 * 300 + 10 || f.h >= 2 * 180 + 10
            })
            .expect("a window with room beside it");
        m.backend.focused = Some(wide);
        m.refresh();
        m.backend.focused = Some(floating);
        m.execute(&Command::ToggleFloat).unwrap();
        m.refresh();
        settle(&mut m);
        assert!(m.tree().contains(floating));
        assert_tidy(&m);
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
    fn resize_respects_a_minimum_size() {
        let mut m = manager(0.0);
        m.backend.open(1, "a");
        m.refresh();
        m.backend.open(2, "settings");
        m.backend.min.insert(2, (400, 0));
        m.refresh();
        m.backend.focused = Some(1);
        m.refresh();

        // Grow window 1 into window 2 until window 2 will not shrink.
        let mut errors = 0;
        for _ in 0..10 {
            if m.execute(&Command::Resize(Direction::Right, Some(0.05)))
                .is_err()
            {
                errors += 1;
            }
            m.refresh(); // the poll that sees window 2 refuse
            let (one, two) = (m.backend().frame(1), m.backend().frame(2));
            assert!(two.w >= 400);
            assert!(one.max_x() + 10 <= two.x, "{one:?} overlaps {two:?}");
        }
        assert!(errors > 0, "the limit is reported");
        assert_eq!(m.backend().frame(2).w, 400);
        assert_eq!(m.backend().frame(2).x, 600);

        // One press back is enough to move the border again.
        m.execute(&Command::Resize(Direction::Left, Some(0.05)))
            .unwrap();
        assert!(m.backend().frame(2).w > 400);
    }

    #[test]
    fn a_window_too_big_for_its_slot_pushes_its_neighbour() {
        let mut m = manager(0.0);
        m.backend.open(1, "a");
        m.refresh();
        m.backend.open(2, "big");
        m.backend.min.insert(2, (700, 0));
        m.refresh();
        m.refresh();
        assert_eq!(m.backend().frame(2), Rect::new(300, 25, 700, 600));
        assert_eq!(m.backend().frame(1).w, 290);
        // Retile forgets what was learned, and learns it again.
        m.execute(&Command::Retile).unwrap();
        m.refresh();
        assert_eq!(m.backend().frame(1).w, 290);
    }

    #[test]
    fn a_rounding_terminal_is_not_given_a_minimum_size() {
        let mut m = manager(0.0);
        m.backend.open(1, "a");
        m.refresh();
        m.backend.open(2, "Terminal");
        m.refresh();
        // A few points over its slot, as a terminal snapping to its grid.
        let f = m.backend().frame(2);
        m.backend
            .set_frame(2, Rect::new(f.x, f.y, f.w + 7, f.h + 9));
        m.refresh();
        assert!(m.min_sizes.is_empty());
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
        let mut m = manager(12.0);
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
        // Window 1 turns around from where it was instead of jumping.
        assert!(next.w > mid.w && next.w < 1000, "{mid:?} -> {next:?}");
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
        let mut m = manager(12.0);
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
        let mut m = manager(12.0);
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
        let mut m = manager(12.0);
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
        let mut m = manager(12.0);
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
        let mut m = manager(12.0);
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
    fn warp_refuses_a_place_with_no_room() {
        let mut m = left_one_right_two();
        // Window 1 cannot be split down to half its height, though taking
        // room from the others would make it fit.
        m.min_sizes.insert(1, (0, 400));
        let before = m.tree().clone();
        m.backend.focused = Some(3);
        assert!(m.execute(&Command::Warp(Direction::Left)).is_err());
        assert_eq!(m.tree(), &before);
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
        let mut m = manager(12.0);
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
        let mut m = manager(12.0);
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
        let mut m = manager(12.0);
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
        assert_eq!(m.config().animation_speed, 20.0);

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
        assert!(bind(&mut m, "bind alt - left  : bspx focus left").is_ok());
        assert!(bind(&mut m, "bind shift + alt - 1 : bspx send 1").is_ok());
        assert!(bind(&mut m, "bind alt - : broken").is_err());
        assert_eq!(
            bind(&mut m, "bind").unwrap(),
            "alt - left  : bspx focus left\nshift + alt - 1 : bspx send 1\n"
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
        for speed in [12.0, 0.0] {
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
}
