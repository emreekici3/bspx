//! Keyboard shortcuts.
//!
//! Every `bspx bind '<line>'` in bspxrc adds one shortcut:
//!
//! ```text
//! shift + alt - left : bspx swap left
//! ^ modifiers   ^ key  ^ command
//! ```
//!
//! Modifiers are joined with `+` and end with `-`; a shortcut can have none.
//! The key is a character, a named key (`return`, `left`, `f5`, ...) or a
//! key code (`0x32`). `->` before the `:` lets the key press through to the
//! app as well. A command that is just `bspx ...` runs inside bspx; anything
//! else runs in the shell.
//!
//! This module only parses and matches; reading the keyboard is the
//! platform's business.

use std::collections::HashMap;

/// One side, both or either of a modifier pair.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Side {
    /// Not held.
    #[default]
    Off,
    /// Either key, or both.
    Any,
    Left,
    Right,
}

/// The modifiers of a shortcut.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub alt: Side,
    pub shift: Side,
    pub cmd: Side,
    pub ctrl: Side,
    pub fun: bool,
}

/// The modifiers held during a key press: for each pair, whether the left
/// and the right key are down.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Held {
    pub alt: (bool, bool),
    pub shift: (bool, bool),
    pub cmd: (bool, bool),
    pub ctrl: (bool, bool),
    pub fun: bool,
}

fn side_matches(want: Side, (left, right): (bool, bool)) -> bool {
    match want {
        Side::Off => !left && !right,
        Side::Any => left || right,
        Side::Left => left && !right,
        Side::Right => right && !left,
    }
}

impl Mods {
    /// Whether these modifiers are the ones held. `ignore_fn`: the key is
    /// one macOS always reports with fn (arrows, function keys, ...).
    pub fn matches(&self, held: &Held, ignore_fn: bool) -> bool {
        side_matches(self.alt, held.alt)
            && side_matches(self.shift, held.shift)
            && side_matches(self.cmd, held.cmd)
            && side_matches(self.ctrl, held.ctrl)
            && (ignore_fn || self.fun == held.fun)
    }

    fn add(&mut self, name: &str) -> Result<(), String> {
        let set = |slot: &mut Side, side: Side| -> Result<(), String> {
            // Both sides named: either will do.
            *slot = if *slot == Side::Off || *slot == side {
                side
            } else {
                Side::Any
            };
            Ok(())
        };
        match name {
            "alt" | "opt" | "option" => set(&mut self.alt, Side::Any),
            "lalt" => set(&mut self.alt, Side::Left),
            "ralt" => set(&mut self.alt, Side::Right),
            "shift" => set(&mut self.shift, Side::Any),
            "lshift" => set(&mut self.shift, Side::Left),
            "rshift" => set(&mut self.shift, Side::Right),
            "cmd" | "command" => set(&mut self.cmd, Side::Any),
            "lcmd" => set(&mut self.cmd, Side::Left),
            "rcmd" => set(&mut self.cmd, Side::Right),
            "ctrl" | "control" => set(&mut self.ctrl, Side::Any),
            "lctrl" => set(&mut self.ctrl, Side::Left),
            "rctrl" => set(&mut self.ctrl, Side::Right),
            "fn" => {
                self.fun = true;
                Ok(())
            }
            "hyper" => ["cmd", "alt", "shift", "ctrl"]
                .iter()
                .try_for_each(|m| self.add(m)),
            "meh" => ["alt", "shift", "ctrl"]
                .iter()
                .try_for_each(|m| self.add(m)),
            other => Err(format!("unknown modifier '{other}'")),
        }
    }
}

/// The key of a shortcut, before the keyboard layout is known.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Key {
    /// Whatever key types this character on the current layout.
    Char(char),
    /// A fixed key code (`kVK_*`).
    Code(u16),
}

/// Named keys and their codes (`kVK_*` in Carbon's Events.h). The `bool`:
/// macOS reports the key with the fn flag set.
const NAMED_KEYS: &[(&str, u16, bool)] = &[
    ("return", 0x24, false),
    ("enter", 0x24, false),
    ("tab", 0x30, false),
    ("space", 0x31, false),
    ("backspace", 0x33, false),
    ("escape", 0x35, false),
    ("esc", 0x35, false),
    ("delete", 0x75, true),
    ("insert", 0x72, true),
    ("home", 0x73, true),
    ("end", 0x77, true),
    ("pageup", 0x74, true),
    ("pagedown", 0x79, true),
    ("left", 0x7B, true),
    ("right", 0x7C, true),
    ("down", 0x7D, true),
    ("up", 0x7E, true),
    ("f1", 0x7A, true),
    ("f2", 0x78, true),
    ("f3", 0x63, true),
    ("f4", 0x76, true),
    ("f5", 0x60, true),
    ("f6", 0x61, true),
    ("f7", 0x62, true),
    ("f8", 0x64, true),
    ("f9", 0x65, true),
    ("f10", 0x6D, true),
    ("f11", 0x67, true),
    ("f12", 0x6F, true),
    ("f13", 0x69, true),
    ("f14", 0x6B, true),
    ("f15", 0x71, true),
    ("f16", 0x6A, true),
    ("f17", 0x40, true),
    ("f18", 0x4F, true),
    ("f19", 0x50, true),
    ("f20", 0x5A, true),
];

/// Whether macOS sets the fn flag on presses of key `code`.
fn reports_fn(code: u16) -> bool {
    NAMED_KEYS.iter().any(|&(_, c, f)| c == code && f)
}

/// A shortcut as written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    pub mods: Mods,
    pub key: Key,
    pub passthrough: bool,
    pub command: String,
}

fn parse_key(text: &str) -> Result<Key, String> {
    let lower = text.to_lowercase();
    if let Some(hex) = lower.strip_prefix("0x") {
        let code = u16::from_str_radix(hex, 16).map_err(|_| format!("bad key code '{text}'"))?;
        return Ok(Key::Code(code));
    }
    if let Some(&(_, code, _)) = NAMED_KEYS.iter().find(|(n, _, _)| *n == lower) {
        return Ok(Key::Code(code));
    }
    let mut chars = lower.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Ok(Key::Char(c)),
        (None, _) => Err("missing key".into()),
        _ => Err(format!("unknown key '{text}'")),
    }
}

impl Binding {
    /// Parses one shortcut line.
    pub fn parse(line: &str) -> Result<Self, String> {
        let (keys, command) = line
            .split_once(':')
            .ok_or("expected ':' and a command after the key")?;
        let command = command.trim();
        if command.is_empty() {
            return Err("missing command after ':'".into());
        }
        let mut keys = keys.trim();
        let passthrough = match keys.strip_suffix("->") {
            Some(rest) => {
                keys = rest.trim_end();
                true
            }
            None => false,
        };
        // "mods - key", or just "key". The key itself may be '-'.
        let (mods_text, key_text) = match keys.rfind('-') {
            Some(i) if i + 1 < keys.len() && !keys[i + 1..].trim().is_empty() => {
                (Some(&keys[..i]), keys[i + 1..].trim())
            }
            Some(i) if i > 0 && keys[..i].trim_end().ends_with('-') => {
                let head = keys[..i].trim_end();
                (Some(&head[..head.len() - 1]), "-")
            }
            _ => (None, keys),
        };
        let mut mods = Mods::default();
        if let Some(text) = mods_text {
            if text.trim().is_empty() {
                return Err("missing modifier before '-'".into());
            }
            for name in text.split('+') {
                let name = name.trim().to_lowercase();
                if name.is_empty() {
                    return Err("empty modifier between '+'".into());
                }
                mods.add(&name)?;
            }
        }
        Ok(Self {
            mods,
            key: parse_key(key_text)?,
            passthrough,
            command: command.to_string(),
        })
    }
}

/// Checks shortcut lines; returns how many there are, or the first error.
pub fn check(text: &str) -> Result<usize, String> {
    let mut n = 0;
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        Binding::parse(line).map_err(|e| format!("'{line}': {e}"))?;
        n += 1;
    }
    Ok(n)
}

/// What to do with a key press.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    /// A bspx command line, run by the daemon.
    Bspx(String),
    /// A command for the shell.
    Shell(String),
}

/// A command that only runs bspx with plain words runs in the daemon; one
/// that needs the shell (quotes, pipes, variables, several commands) gets
/// the shell.
pub fn action(command: &str) -> Action {
    const SHELL_CHARS: &[char] = &[
        ';', '&', '|', '$', '`', '<', '>', '"', '\'', '\\', '(', ')', '{', '}', '*', '?', '[', ']',
        '~', '#', '=', '!', '\n',
    ];
    let mut words = command.split_whitespace();
    let is_bspx = words
        .next()
        .is_some_and(|w| w == "bspx" || w.ends_with("/bspx"));
    let rest: Vec<&str> = words.collect();
    let plain = !command.contains(SHELL_CHARS);
    let first = rest.first().copied().unwrap_or("");
    // Service commands manage the daemon itself, from outside.
    let service = matches!(
        first,
        "run" | "install" | "uninstall" | "start" | "stop" | "restart" | "help" | "-h" | "--help"
    );
    if is_bspx && plain && !rest.is_empty() && !service {
        Action::Bspx(rest.join(" "))
    } else {
        Action::Shell(command.to_string())
    }
}

/// A shortcut with its key code known.
#[derive(Clone, Debug)]
struct Compiled {
    code: u16,
    mods: Mods,
    passthrough: bool,
    command: String,
}

/// The shortcuts, ready to match key presses.
#[derive(Debug, Default)]
pub struct Hotkeys {
    by_code: HashMap<u16, Vec<Compiled>>,
    count: usize,
}

impl Hotkeys {
    /// Builds the shortcuts from `text`, one per line, using `layout` to
    /// find the key that types a character. Returns the problems found,
    /// such as a character no key types; those shortcuts are left out.
    pub fn build(text: &str, layout: &HashMap<char, u16>) -> (Self, Vec<String>) {
        let mut hotkeys = Self::default();
        let mut problems = Vec::new();
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let binding = match Binding::parse(line) {
                Ok(b) => b,
                Err(e) => {
                    problems.push(format!("'{line}': {e}"));
                    continue;
                }
            };
            let code = match binding.key {
                Key::Code(c) => c,
                Key::Char(c) => match layout.get(&c) {
                    Some(&code) => code,
                    None => {
                        problems.push(format!("'{line}': no key types '{c}' on this layout"));
                        continue;
                    }
                },
            };
            hotkeys.by_code.entry(code).or_default().push(Compiled {
                code,
                mods: binding.mods,
                passthrough: binding.passthrough,
                command: binding.command,
            });
            hotkeys.count += 1;
        }
        (hotkeys, problems)
    }

    pub fn count(&self) -> usize {
        self.count
    }

    /// The shortcut for a press of key `code`: what to run, and whether the
    /// press is kept from the app. A later shortcut for the same keys wins.
    pub fn press(&self, code: u16, held: &Held) -> Option<(Action, bool)> {
        let ignore_fn = reports_fn(code);
        self.by_code
            .get(&code)?
            .iter()
            .rev()
            .find(|h| h.code == code && h.mods.matches(held, ignore_fn))
            .map(|h| (action(&h.command), !h.passthrough))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> HashMap<char, u16> {
        [
            ('h', 0x04),
            ('l', 0x25),
            ('1', 0x12),
            ('-', 0x1B),
            ('ş', 0x29),
        ]
        .into_iter()
        .collect()
    }

    fn held_alt_left() -> Held {
        Held {
            alt: (true, false),
            ..Held::default()
        }
    }

    #[test]
    fn parses_lines() {
        let b = Binding::parse("shift + alt - left : bspx swap left").unwrap();
        assert_eq!(b.mods.shift, Side::Any);
        assert_eq!(b.mods.alt, Side::Any);
        assert_eq!(b.key, Key::Code(0x7B));
        assert_eq!(b.command, "bspx swap left");
        assert!(!b.passthrough);

        let b = Binding::parse("hyper - 1 : echo one").unwrap();
        assert_eq!(
            (b.mods.cmd, b.mods.alt, b.mods.shift, b.mods.ctrl),
            (Side::Any, Side::Any, Side::Any, Side::Any)
        );
        let b = Binding::parse("lalt - 0x32 -> : echo grave").unwrap();
        assert_eq!(
            (b.mods.alt, b.key.clone(), b.passthrough),
            (Side::Left, Key::Code(0x32), true)
        );
        let b = Binding::parse("f5 : echo five").unwrap();
        assert_eq!((b.mods, b.key), (Mods::default(), Key::Code(0x60)));
        let b = Binding::parse("alt - - : echo minus").unwrap();
        assert_eq!(b.key, Key::Char('-'));
        let b = Binding::parse("cmd - H : x").unwrap();
        assert_eq!(b.key, Key::Char('h'));
        let b = Binding::parse("alt - return : open -na Terminal").unwrap();
        assert_eq!(b.command, "open -na Terminal");
    }

    #[test]
    fn rejects_bad_lines() {
        assert!(Binding::parse("alt - h").is_err());
        assert!(Binding::parse("alt - h :").is_err());
        assert!(
            Binding::parse("super - h : x")
                .unwrap_err()
                .contains("super")
        );
        assert!(
            Binding::parse("alt - nokey : x")
                .unwrap_err()
                .contains("nokey")
        );
        assert!(Binding::parse("- h : x").is_err());
        assert!(
            check("alt - h : a\nalt - : b")
                .unwrap_err()
                .contains("alt - : b")
        );
        assert_eq!(check("alt - h : a\n\nalt - l : b\n"), Ok(2));
    }

    #[test]
    fn matches_presses() {
        let (hk, problems) = Hotkeys::build(
            "alt - h : bspx focus left\n\
             ralt - l : bspx focus right\n\
             alt - left -> : echo through\n\
             alt - ş : echo turkish\n",
            &layout(),
        );
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(hk.count(), 4);

        // Either alt key takes a plain `alt`.
        assert_eq!(
            hk.press(0x04, &held_alt_left()),
            Some((Action::Bspx("focus left".into()), true))
        );
        let right = Held {
            alt: (false, true),
            ..Held::default()
        };
        assert!(hk.press(0x04, &right).is_some());
        // Extra modifiers or none: no match.
        let with_shift = Held {
            shift: (true, false),
            ..held_alt_left()
        };
        assert_eq!(hk.press(0x04, &with_shift), None);
        assert_eq!(hk.press(0x04, &Held::default()), None);
        // `ralt` wants the right one only.
        assert_eq!(hk.press(0x25, &held_alt_left()), None);
        assert!(hk.press(0x25, &right).is_some());
        // Arrows come with fn set; passthrough lets the key through.
        let arrow = Held {
            fun: true,
            ..held_alt_left()
        };
        assert_eq!(
            hk.press(0x7B, &arrow),
            Some((Action::Shell("echo through".into()), false))
        );
        assert!(hk.press(0x29, &held_alt_left()).is_some());
    }

    #[test]
    fn later_shortcut_wins_and_missing_keys_are_reported() {
        let (hk, problems) = Hotkeys::build(
            "alt - h : first\nalt - h : second\nalt - ğ : nowhere\n",
            &layout(),
        );
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("'ğ'"));
        assert_eq!(
            hk.press(0x04, &held_alt_left()),
            Some((Action::Shell("second".into()), true))
        );
    }

    #[test]
    fn bspx_commands_run_inside() {
        assert_eq!(action("bspx focus left"), Action::Bspx("focus left".into()));
        assert_eq!(
            action("~/bin/bspx swap up"),
            Action::Shell("~/bin/bspx swap up".into())
        );
        assert_eq!(
            action("/Users/me/bin/bspx  space 2"),
            Action::Bspx("space 2".into())
        );
        assert_eq!(action("bspx restart"), Action::Shell("bspx restart".into()));
        assert_eq!(
            action("bspx focus left && echo hi"),
            Action::Shell("bspx focus left && echo hi".into())
        );
        assert_eq!(
            action("open -a Safari"),
            Action::Shell("open -a Safari".into())
        );
    }
}
