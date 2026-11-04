//! Settings. There is no config file format: the config is a
//! shell script (`cberryrc`) that cberry runs at startup, made of `cberry config`
//! and `cberry rule` commands. The same commands change settings at runtime.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::geometry::Padding;
use crate::platform::{Modifier, MouseButton};

/// `animation_speed` is an older way to say how long a window takes:
/// this many over the duration.
const SPEED_PER_DURATION: f64 = 5.0;

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    /// Space between tiled windows, in points.
    pub gap: i32,
    /// Space between the tiled area and the screen edges.
    pub padding: Padding,
    /// How long a window takes to reach its place, in seconds; `0` disables
    /// animations.
    pub animation_duration: f64,
    /// Share of the area the existing window keeps when a new one splits it.
    pub split_ratio: f32,
    /// Default amount for `resize` commands, as a fraction of the split.
    pub resize_step: f32,
    /// Kept so older configs still load: cberry no longer polls, it waits
    /// for window, app, Space and display events instead.
    pub poll_interval_ms: u64,
    /// Apps (by name, e.g. "System Settings") whose windows are never tiled.
    pub float_apps: Vec<String>,
    /// A Space rule may carry a new window there by its title bar, taking
    /// the pointer and the user along, when nothing else moves it.
    pub rule_carry: bool,
    /// Dropping a dragged window onto another one swaps them.
    pub drag_swap: bool,
    /// Drop-target highlight colour, RGBA in 0..=1.
    pub drag_color: [f32; 4],
    /// Draw a ring around the focused window.
    pub border: bool,
    /// The ring's colour, RGBA in 0..=1.
    pub border_color: [f32; 4],
    /// The ring's width, in points.
    pub border_width: i32,
    /// The roundness of windows' corners, for the ring to follow, in points.
    pub border_radius: i32,
    /// Apps whose windows have corners of their own: (app, radius).
    pub border_radius_rules: Vec<(String, i32)>,
    /// Held with a click anywhere on a window, moves or resizes it.
    pub mouse_modifier: Option<Modifier>,
    /// Held as well while moving a tiled window with the modifier, dropping
    /// it on another window splits that one instead of swapping with it.
    pub mouse_insert_modifier: Option<Modifier>,
    /// What dragging with the left button (with the modifier) does.
    pub mouse_action1: MouseAction,
    /// What dragging with the right button (with the modifier) does.
    pub mouse_action2: MouseAction,
    /// Focusing a window from the keyboard puts the pointer in its middle.
    pub mouse_follows_focus: bool,
    /// The window under the pointer gets focus (and is raised: macOS has no
    /// focus without raising, short of disabling SIP).
    pub focus_follows_mouse: bool,
    /// How much to log; `None` leaves it as cberry was started with.
    pub log_level: Option<log::LevelFilter>,
    /// Space names, by Space number (from 1, as in Mission Control).
    pub labels: BTreeMap<usize, String>,
    /// Apps whose new windows go to a Space: (app, Space number or label).
    pub space_rules: Vec<(String, String)>,
    /// Apps whose new windows float at a grid cell:
    /// (app, [rows, cols, x, y, w, h]).
    pub grid_rules: Vec<(String, [u32; 6])>,
    /// Spaces (numbers or labels) whose windows are not tiled.
    pub float_spaces: Vec<String>,
    /// Keyboard shortcut lines (`cberry bind`).
    pub hotkeys: Vec<String>,
}

/// What a modifier drag does to the window under the pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseAction {
    Move,
    Resize,
}

impl MouseAction {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "move" => Some(Self::Move),
            "resize" => Some(Self::Resize),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Move => "move",
            Self::Resize => "resize",
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            gap: 8,
            padding: Padding::uniform(8),
            animation_duration: 0.42,
            split_ratio: 0.5,
            resize_step: 0.05,
            poll_interval_ms: 150,
            float_apps: vec![
                "System Settings".into(),
                "System Preferences".into(),
                "Calculator".into(),
                "Archive Utility".into(),
            ],
            rule_carry: true,
            drag_swap: true,
            drag_color: [0.37, 0.62, 1.0, 0.25],
            border: false,
            border_color: [0.541, 0.663, 0.973, 1.0],
            border_width: 2,
            border_radius: 12,
            border_radius_rules: Vec::new(),
            mouse_modifier: Some(Modifier::Alt),
            mouse_insert_modifier: None,
            mouse_action1: MouseAction::Move,
            mouse_action2: MouseAction::Resize,
            mouse_follows_focus: false,
            focus_follows_mouse: false,
            log_level: None,
            labels: BTreeMap::new(),
            space_rules: Vec::new(),
            grid_rules: Vec::new(),
            float_spaces: Vec::new(),
            hotkeys: Vec::new(),
        }
    }
}

impl Config {
    /// What dragging with `button` and the modifier does.
    pub fn mouse_action(&self, button: MouseButton) -> MouseAction {
        match button {
            MouseButton::Left => self.mouse_action1,
            MouseButton::Right => self.mouse_action2,
        }
    }
}

/// Names accepted by `cberry config`, for help and error messages.
pub const KEYS: &[&str] = &[
    "gap",
    "padding",
    "animation_duration",
    "split_ratio",
    "resize_step",
    "poll_interval",
    "rule_carry",
    "drag_swap",
    "drag_color",
    "border",
    "border_color",
    "border_width",
    "border_radius",
    "mouse_modifier",
    "mouse_insert_modifier",
    "mouse_action1",
    "mouse_action2",
    "mouse_follows_focus",
    "focus_follows_mouse",
    "log_level",
];

/// Whether a rule's app pattern matches a window's app name.
///
/// A pattern between slashes, such as `/Webex/`, is a regular expression
/// searched for anywhere in the name (anchor it with `^` and `$`, ignore case
/// with `(?i)`). Any other pattern must equal the name. Names are compared
/// without invisible formatting characters and surrounding whitespace: some
/// apps put such characters in their names (WhatsApp reports itself as
/// "\u{200e}WhatsApp"), and the name typed in `cberryrc` cannot contain them.
pub fn app_matches(rule: &str, app: &str) -> bool {
    let app = clean_app_name(app);
    match app_regex(rule) {
        Some(Ok(re)) => re.is_match(&app),
        Some(Err(_)) => false,
        None => clean_app_name(rule) == app,
    }
}

/// Whether two rules name the same app pattern, so the newer replaces the
/// older. Patterns are compared as written: `WhatsApp` does not replace
/// `/Whats/` even though the regex would match it.
pub fn same_rule(a: &str, b: &str) -> bool {
    clean_app_name(a) == clean_app_name(b)
}

/// Checks a rule's app pattern before it is stored: a `/regex/` must be a
/// valid, non-empty regular expression.
pub fn check_app_pattern(rule: &str) -> Result<(), String> {
    match app_regex(rule) {
        Some(Err(e)) => Err(e),
        _ => Ok(()),
    }
}

/// The regular expression of a `/pattern/` rule, compiled once and cached;
/// `None` for a plain app name.
fn app_regex(rule: &str) -> Option<Result<regex::Regex, String>> {
    use std::cell::RefCell;
    use std::collections::HashMap;
    thread_local! {
        static CACHE: RefCell<HashMap<String, Result<regex::Regex, String>>> =
            RefCell::new(HashMap::new());
    }
    let rule = clean_app_name(rule);
    let source = rule.strip_prefix('/')?.strip_suffix('/')?;
    Some(CACHE.with(|cache| {
        cache
            .borrow_mut()
            .entry(rule.clone())
            .or_insert_with(|| {
                if source.is_empty() {
                    return Err(format!("empty regular expression {rule}"));
                }
                regex::Regex::new(source).map_err(|e| format!("bad regular expression {rule}: {e}"))
            })
            .clone()
    }))
}

/// An app name without invisible formatting characters (direction marks,
/// zero-width spaces and joiners, byte order marks, soft hyphens) and
/// without surrounding whitespace.
pub fn clean_app_name(name: &str) -> String {
    let visible: String = name.chars().filter(|&c| !is_invisible(c)).collect();
    visible.trim().to_string()
}

fn is_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{FEFF}'
    )
}

/// Parses `RRGGBB` or `RRGGBBAA`, with an optional `#` or `0x` prefix.
fn parse_color(value: &str) -> Option<[f32; 4]> {
    let hex = value
        .strip_prefix('#')
        .or_else(|| value.strip_prefix("0x"))
        .unwrap_or(value);
    if !(hex.len() == 6 || hex.len() == 8) || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |i: usize| {
        u8::from_str_radix(&hex[i..i + 2], 16)
            .ok()
            .map(|b| f32::from(b) / 255.0)
    };
    let alpha = if hex.len() == 8 { byte(6)? } else { 0.25 };
    Some([byte(0)?, byte(2)?, byte(4)?, alpha])
}

fn format_color(c: [f32; 4]) -> String {
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!(
        "{:02x}{:02x}{:02x}{:02x}",
        b(c[0]),
        b(c[1]),
        b(c[2]),
        b(c[3])
    )
}

fn number<T: std::str::FromStr>(key: &str, value: &str) -> Result<T, String> {
    value
        .parse()
        .map_err(|_| format!("{key}: '{value}' is not a valid number"))
}

fn on_off(key: &str, value: &str) -> Result<bool, String> {
    match value {
        "on" | "true" | "yes" | "1" => Ok(true),
        "off" | "false" | "no" | "0" => Ok(false),
        v => Err(format!("{key}: expected on or off, got '{v}'")),
    }
}

fn on_off_name(v: bool) -> String {
    if v { "on" } else { "off" }.to_string()
}

fn one<'a>(key: &str, args: &[&'a str]) -> Result<&'a str, String> {
    match args {
        [v] => Ok(v),
        [] => Err(format!("{key}: missing value")),
        _ => Err(format!("{key}: expected one value")),
    }
}

impl Config {
    /// Applies `cberry config <key> <args...>`.
    pub fn set(&mut self, key: &str, args: &[&str]) -> Result<(), String> {
        match key {
            "gap" => {
                let v: i32 = number(key, one(key, args)?)?;
                if v < 0 {
                    return Err("gap: must not be negative".into());
                }
                self.gap = v;
            }
            "padding" => {
                let v: Vec<i32> = args
                    .iter()
                    .map(|a| number(key, a))
                    .collect::<Result<_, _>>()?;
                if v.iter().any(|x| *x < 0) {
                    return Err("padding: must not be negative".into());
                }
                self.padding = match v[..] {
                    [all] => Padding::uniform(all),
                    [vertical, horizontal] => Padding {
                        top: vertical,
                        right: horizontal,
                        bottom: vertical,
                        left: horizontal,
                    },
                    [top, right, bottom, left] => Padding {
                        top,
                        right,
                        bottom,
                        left,
                    },
                    _ => return Err("padding: give 1, 2 (vertical horizontal) or 4 (top right bottom left) values".into()),
                };
            }
            "animation_duration" | "animation" => {
                let secs: f64 = number(key, one(key, args)?)?;
                if !(0.0..=5.0).contains(&secs) {
                    return Err(format!("{key}: must be between 0 and 5 seconds"));
                }
                self.animation_duration = secs;
            }
            // Kept so older configs still load.
            "animation_speed" => {
                let v: f64 = number(key, one(key, args)?)?;
                if v < 0.0 {
                    return Err("animation_speed: must not be negative".into());
                }
                self.animation_duration = if v == 0.0 {
                    0.0
                } else {
                    SPEED_PER_DURATION / v
                };
            }
            // Kept so older configs still load: there is one curve now.
            "animation_easing" | "easing" => {
                one(key, args)?;
            }
            "split_ratio" => {
                let v: f32 = number(key, one(key, args)?)?;
                if !(0.1..=0.9).contains(&v) {
                    return Err("split_ratio: must be between 0.1 and 0.9".into());
                }
                self.split_ratio = v;
            }
            "resize_step" => {
                let v: f32 = number(key, one(key, args)?)?;
                if !(v > 0.0 && v < 1.0) {
                    return Err("resize_step: must be between 0 and 1".into());
                }
                self.resize_step = v;
            }
            "poll_interval" | "poll_interval_ms" => {
                let v: u64 = number(key, one(key, args)?)?;
                if !(16..=5000).contains(&v) {
                    return Err(format!("{key}: must be between 16 and 5000 ms"));
                }
                self.poll_interval_ms = v;
            }
            "drag_swap" => self.drag_swap = on_off(key, one(key, args)?)?,
            "rule_carry" => self.rule_carry = on_off(key, one(key, args)?)?,
            "mouse_follows_focus" => self.mouse_follows_focus = on_off(key, one(key, args)?)?,
            "focus_follows_mouse" => self.focus_follows_mouse = on_off(key, one(key, args)?)?,
            "log_level" => {
                let v = one(key, args)?;
                self.log_level = match v {
                    "default" => None,
                    "error" => Some(log::LevelFilter::Error),
                    "warn" => Some(log::LevelFilter::Warn),
                    "info" => Some(log::LevelFilter::Info),
                    "debug" => Some(log::LevelFilter::Debug),
                    _ => {
                        return Err(format!(
                            "log_level: expected error, warn, info, debug or default, got '{v}'"
                        ));
                    }
                };
            }
            "drag_color" => {
                let v = one(key, args)?;
                self.drag_color = parse_color(v).ok_or_else(|| {
                    format!("drag_color: '{v}' is not a colour like 5e9eff or 5e9eff40")
                })?;
            }
            "border" => self.border = on_off(key, one(key, args)?)?,
            "border_color" => {
                let v = one(key, args)?;
                self.border_color = parse_color(v).ok_or_else(|| {
                    format!("border_color: '{v}' is not a colour like 8aa9f8 or 8aa9f8c0")
                })?;
            }
            "border_width" => {
                let v: i32 = number(key, one(key, args)?)?;
                if !(1..=20).contains(&v) {
                    return Err("border_width: must be between 1 and 20".into());
                }
                self.border_width = v;
            }
            "border_radius" => {
                let v: i32 = number(key, one(key, args)?)?;
                if !(0..=40).contains(&v) {
                    return Err("border_radius: must be between 0 and 40".into());
                }
                self.border_radius = v;
            }
            "mouse_modifier" | "mouse_insert_modifier" => {
                let v = one(key, args)?;
                let modifier = match v {
                    "off" | "none" => None,
                    v => Some(Modifier::parse(v).ok_or_else(|| {
                        format!("{key}: expected alt, cmd, ctrl, shift, fn or off, got '{v}'")
                    })?),
                };
                if key == "mouse_modifier" {
                    self.mouse_modifier = modifier;
                } else {
                    self.mouse_insert_modifier = modifier;
                }
            }
            "mouse_action1" | "mouse_action2" => {
                let v = one(key, args)?;
                let action = MouseAction::parse(v)
                    .ok_or_else(|| format!("{key}: expected move or resize, got '{v}'"))?;
                if key == "mouse_action1" {
                    self.mouse_action1 = action;
                } else {
                    self.mouse_action2 = action;
                }
            }
            _ => {
                return Err(format!(
                    "unknown setting '{key}' (settings: {})",
                    KEYS.join(", ")
                ));
            }
        }
        Ok(())
    }

    /// Answers `cberry config <key>`.
    pub fn get(&self, key: &str) -> Result<String, String> {
        Ok(match key {
            "gap" => self.gap.to_string(),
            "padding" => {
                let p = self.padding;
                format!("{} {} {} {}", p.top, p.right, p.bottom, p.left)
            }
            "animation_duration" | "animation" => self.animation_duration.to_string(),
            "split_ratio" => self.split_ratio.to_string(),
            "resize_step" => self.resize_step.to_string(),
            "poll_interval" | "poll_interval_ms" => self.poll_interval_ms.to_string(),
            "drag_swap" => on_off_name(self.drag_swap),
            "rule_carry" => on_off_name(self.rule_carry),
            "mouse_follows_focus" => on_off_name(self.mouse_follows_focus),
            "focus_follows_mouse" => on_off_name(self.focus_follows_mouse),
            "log_level" => self
                .log_level
                .map_or("default".into(), |l| l.as_str().to_lowercase()),
            "drag_color" => format_color(self.drag_color),
            "border" => on_off_name(self.border),
            "border_color" => format_color(self.border_color),
            "border_width" => self.border_width.to_string(),
            "border_radius" => self.border_radius.to_string(),
            "mouse_modifier" => self
                .mouse_modifier
                .map_or("off", Modifier::name)
                .to_string(),
            "mouse_insert_modifier" => self
                .mouse_insert_modifier
                .map_or("off", Modifier::name)
                .to_string(),
            "mouse_action1" => self.mouse_action1.name().to_string(),
            "mouse_action2" => self.mouse_action2.name().to_string(),
            _ => {
                return Err(format!(
                    "unknown setting '{key}' (settings: {})",
                    KEYS.join(", ")
                ));
            }
        })
    }

    /// `cberry label <n> <name>`. A name belongs to one Space only.
    pub fn set_label(&mut self, n: usize, name: &str) {
        self.labels.retain(|_, l| l != name);
        self.labels.insert(n, name.to_string());
    }

    /// `cberry rule space <space> <app>`.
    pub fn set_space_rule(&mut self, app: &str, space: &str) {
        self.space_rules.retain(|(a, _)| !same_rule(a, app));
        self.space_rules.push((app.to_string(), space.to_string()));
    }

    /// `cberry rule grid <rows:cols:x:y:w:h> <app>`.
    pub fn set_grid_rule(&mut self, app: &str, grid: [u32; 6]) {
        self.grid_rules.retain(|(a, _)| !same_rule(a, app));
        self.grid_rules.push((app.to_string(), grid));
    }

    /// `cberry layout <space> float|bsp`.
    pub fn set_float_space(&mut self, space: &str, float: bool) {
        let n = self.space_number(space);
        let kept: Vec<String> = self
            .float_spaces
            .iter()
            .filter(|s| *s != space && (n.is_none() || self.space_number(s) != n))
            .cloned()
            .collect();
        self.float_spaces = kept;
        if float {
            self.float_spaces.push(space.to_string());
        }
    }

    /// The Space number `space` (a number or a label) stands for.
    pub fn space_number(&self, space: &str) -> Option<usize> {
        space.parse().ok().filter(|n| *n > 0).or_else(|| {
            self.labels
                .iter()
                .find(|(_, l)| l.as_str() == space)
                .map(|(n, _)| *n)
        })
    }

    /// `cberry rule border_radius <radius> <app>`.
    pub fn set_border_radius_rule(&mut self, app: &str, radius: i32) {
        self.border_radius_rules.retain(|(a, _)| !same_rule(a, app));
        self.border_radius_rules.push((app.to_string(), radius));
    }

    /// The roundness of the ring around a window of `app`.
    pub fn border_radius_for(&self, app: &str) -> i32 {
        self.border_radius_rules
            .iter()
            .rev()
            .find(|(a, _)| app_matches(a, app))
            .map_or(self.border_radius, |(_, radius)| *radius)
    }

    /// `cberry rule float <app>` / `cberry rule tile <app>`.
    pub fn set_float_app(&mut self, app: &str, float: bool) {
        self.float_apps.retain(|a| !same_rule(a, app));
        if float {
            self.float_apps.push(app.to_string());
        }
    }

    /// Where to look for the config script, in priority order.
    pub fn rc_paths() -> Vec<PathBuf> {
        let mut paths = Vec::new();
        if let Some(p) = std::env::var_os("CBERRY_CONFIG") {
            paths.push(PathBuf::from(p));
        }
        if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
            paths.push(PathBuf::from(xdg).join("cranberry/cberryrc"));
        }
        if let Some(home) = std::env::var_os("HOME") {
            let home = PathBuf::from(home);
            paths.push(home.join(".config/cranberry/cberryrc"));
            paths.push(home.join(".cberryrc"));
        }
        paths
    }

    /// The config script to run, if there is one.
    pub fn find_rc() -> Option<PathBuf> {
        Self::rc_paths().into_iter().find(|p| p.is_file())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_and_get() {
        let mut c = Config::default();
        c.set("gap", &["12"]).unwrap();
        assert_eq!(c.get("gap").unwrap(), "12");

        c.set("animation", &["0.25"]).unwrap();
        assert_eq!(c.animation_duration, 0.25);
        assert_eq!(c.get("animation_duration").unwrap(), "0.25");
        c.set("animation_duration", &["0"]).unwrap();
        assert_eq!(c.animation_duration, 0.0);

        c.set("padding", &["40", "8", "8", "8"]).unwrap();
        assert_eq!(c.get("padding").unwrap(), "40 8 8 8");
        c.set("padding", &["10", "20"]).unwrap();
        assert_eq!(c.get("padding").unwrap(), "10 20 10 20");
        c.set("padding", &["5"]).unwrap();
        assert_eq!(c.padding, Padding::uniform(5));
    }

    #[test]
    fn rejects_bad_values() {
        let mut c = Config::default();
        assert!(c.set("gap", &[]).is_err());
        assert!(c.set("gap", &["-1"]).is_err());
        assert!(c.set("gap", &["wide"]).is_err());
        assert!(c.set("gap", &["1", "2"]).is_err());
        assert!(c.set("padding", &["1", "2", "3"]).is_err());
        assert!(c.set("split_ratio", &["1.5"]).is_err());
        assert!(c.set("animation", &["10"]).is_err());
        assert!(c.set("colour", &["red"]).is_err());
        assert!(c.get("colour").is_err());
        assert_eq!(c, Config::default(), "failed sets change nothing");
    }

    #[test]
    fn easing_setting() {
        let mut c = Config::default();
        // Older configs still load; the curve is not theirs to choose now.
        let before = c.clone();
        c.set("animation_easing", &["ease_in_out"]).unwrap();
        assert_eq!(c, before);
        c.set("animation_speed", &["20"]).unwrap();
        assert_eq!(c.animation_duration, 0.25);
        c.set("easing", &["ease_out"]).unwrap();
    }

    #[test]
    fn drag_settings() {
        let mut c = Config::default();
        c.set("drag_color", &["#ff8000"]).unwrap();
        assert_eq!(c.get("drag_color").unwrap(), "ff800040");
        c.set("drag_color", &["0x00ff0080"]).unwrap();
        assert_eq!(c.get("drag_color").unwrap(), "00ff0080");
        assert!(c.set("drag_color", &["red"]).is_err());
        assert!(c.set("drag_color", &["12345"]).is_err());
        c.set("drag_swap", &["off"]).unwrap();
        assert!(!c.drag_swap);
        assert!(c.set("drag_swap", &["maybe"]).is_err());
    }

    #[test]
    fn mouse_settings() {
        let mut c = Config::default();
        assert_eq!(c.get("mouse_modifier").unwrap(), "alt");
        c.set("mouse_modifier", &["cmd"]).unwrap();
        assert_eq!(c.mouse_modifier, Some(Modifier::Cmd));
        c.set("mouse_modifier", &["off"]).unwrap();
        assert_eq!(c.mouse_modifier, None);
        assert_eq!(c.get("mouse_modifier").unwrap(), "off");
        assert!(c.set("mouse_modifier", &["hyper"]).is_err());
        assert_eq!(c.get("mouse_insert_modifier").unwrap(), "off");
        c.set("mouse_insert_modifier", &["cmd"]).unwrap();
        assert_eq!(c.mouse_insert_modifier, Some(Modifier::Cmd));
        assert_eq!(c.get("mouse_insert_modifier").unwrap(), "cmd");
        assert!(c.set("mouse_insert_modifier", &["hyper"]).is_err());
        c.set("mouse_action1", &["resize"]).unwrap();
        c.set("mouse_action2", &["move"]).unwrap();
        assert_eq!(c.mouse_action(MouseButton::Left), MouseAction::Resize);
        assert_eq!(c.mouse_action(MouseButton::Right), MouseAction::Move);
        assert!(c.set("mouse_action1", &["swap"]).is_err());
        c.set("mouse_follows_focus", &["on"]).unwrap();
        c.set("focus_follows_mouse", &["on"]).unwrap();
        assert!(c.mouse_follows_focus && c.focus_follows_mouse);
        assert_eq!(c.get("focus_follows_mouse").unwrap(), "on");
        assert!(c.set("focus_follows_mouse", &["sometimes"]).is_err());
    }

    #[test]
    fn labels_and_space_rules() {
        let mut c = Config::default();
        c.set_label(2, "web");
        c.set_label(3, "web"); // moved
        c.set_label(1, "general");
        assert_eq!(c.space_number("web"), Some(3));
        assert_eq!(c.space_number("general"), Some(1));
        assert_eq!(c.space_number("4"), Some(4));
        assert_eq!(c.space_number("0"), None);
        assert_eq!(c.space_number("music"), None);
        c.set_float_space("web", true);
        c.set_float_space("3", false); // the same Space, by number
        assert!(c.float_spaces.is_empty());
        c.set_space_rule("Safari", "web");
        c.set_space_rule("Safari", "2");
        assert_eq!(c.space_rules, vec![("Safari".into(), "2".into())]);
    }

    #[test]
    fn rules_match_apps_with_invisible_characters_in_their_names() {
        assert!(app_matches("WhatsApp", "\u{200e}WhatsApp"));
        assert!(app_matches("\u{200e}WhatsApp", "WhatsApp"));
        assert!(app_matches(" Safari ", "Safari\u{feff}"));
        assert!(!app_matches("WhatsApp", "WhatsApp Beta"));
        assert!(!app_matches("whatsapp", "WhatsApp"));
        let mut c = Config::default();
        c.set_float_app("WhatsApp", true);
        c.set_float_app("\u{200e}WhatsApp", true);
        assert_eq!(c.float_apps.len(), Config::default().float_apps.len() + 1);
    }

    #[test]
    fn regex_rules() {
        assert!(app_matches("/Webex/", "Cisco Webex Meetings"));
        assert!(app_matches("/Webex/", "Webex"));
        assert!(app_matches("/^Webex$/", "Webex"));
        assert!(!app_matches("/^Webex$/", "Cisco Webex Meetings"));
        assert!(app_matches("/(?i)whatsapp/", "\u{200e}WhatsApp"));
        assert!(app_matches("/^WhatsApp$/", "\u{200e}WhatsApp"));
        assert!(!app_matches("/webex/", "Webex"));
        // A plain name is still matched exactly, even with regex characters.
        assert!(app_matches("Code (Insiders)", "Code (Insiders)"));
        assert!(!app_matches("Code (Insiders)", "Code X"));
        // Only a name between slashes is a pattern.
        assert!(!app_matches("/Webex", "Webex"));
        assert!(app_matches("/Webex", "/Webex"));
    }

    #[test]
    fn bad_regex_rules_are_rejected_and_never_match() {
        assert!(check_app_pattern("/Webex/").is_ok());
        assert!(check_app_pattern("WhatsApp").is_ok());
        assert!(check_app_pattern("(").is_ok());
        assert!(check_app_pattern("/(/").is_err());
        assert!(check_app_pattern("//").is_err());
        assert!(!app_matches("/(/", "("));
    }

    #[test]
    fn a_rule_replaces_only_the_same_pattern() {
        let mut c = Config::default();
        c.set_float_app("/Whats/", true);
        c.set_float_app("WhatsApp", true);
        assert!(c.float_apps.contains(&"/Whats/".to_string()));
        assert!(c.float_apps.contains(&"WhatsApp".to_string()));
        c.set_float_app("/Whats/", false);
        assert!(!c.float_apps.contains(&"/Whats/".to_string()));
        assert!(c.float_apps.contains(&"WhatsApp".to_string()));
        c.set_space_rule("/Webex/", "2");
        c.set_space_rule("/Webex/", "3");
        assert_eq!(c.space_rules, vec![("/Webex/".into(), "3".into())]);
    }

    #[test]
    fn float_rules() {
        let mut c = Config::default();
        c.set_float_app("Finder", true);
        c.set_float_app("Finder", true);
        assert_eq!(c.float_apps.iter().filter(|a| *a == "Finder").count(), 1);
        c.set_float_app("Calculator", false);
        assert!(!c.float_apps.contains(&"Calculator".to_string()));
    }

    #[test]
    fn every_key_round_trips() {
        let c = Config::default();
        for key in KEYS {
            let value = c.get(key).unwrap();
            let args: Vec<&str> = value.split(' ').collect();
            let mut copy = c.clone();
            copy.set(key, &args).unwrap();
            assert_eq!(copy.get(key).unwrap(), value, "{key}");
        }
    }
}
