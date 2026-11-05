//! Commands accepted over the socket (`cberry <command>`).

use crate::config::check_app_pattern;
use crate::geometry::Direction;

/// How far `cberry zoom` grows a window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Zoom {
    /// The whole tiling area.
    Fullscreen,
    /// The area it shares with its sibling.
    Parent,
}

/// Which display `cberry send-display` means. Displays are numbered from 1,
/// left to right.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplayTarget {
    Next,
    Prev,
    Number(usize),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    /// Focus the neighbouring window.
    Focus(Direction),
    /// Swap the focused window with its neighbour.
    Swap(Direction),
    /// Move the focused window into its neighbour's place: the neighbour
    /// splits as it would for a new window, and the window keeps the side
    /// it came from.
    Warp(Direction),
    /// Move a border of the focused window; amount defaults to `resize_step`.
    Resize(Direction, Option<f32>),
    /// Rotate the whole layout 90° clockwise.
    Rotate,
    /// Swap the two halves of the focused window's split.
    Flip,
    /// Switch the focused window's split between side by side and stacked.
    ToggleSplit,
    /// Undo every resize: each split back to `split_ratio`, as the windows
    /// were laid out when they opened.
    Balance,
    /// Give every window the same area.
    BalanceArea,
    /// Let the focused window cover the whole screen (or the area of its
    /// split, for [`Zoom::Parent`]) over the others, or put it back.
    Zoom(Zoom),
    /// Take the focused window out of the layout, or put it back.
    ToggleFloat,
    /// Float the focused window at a grid cell, or tile it if it floats.
    ToggleFloatAt([u32; 6]),
    /// Bring every floating window on the current Space to the front.
    RaiseFloating,
    /// Float the focused window and put it at a grid cell:
    /// `[rows, cols, x, y, w, h]`.
    Grid([u32; 6]),
    /// Re-read windows and snap everything back into place.
    Retile,
    /// Re-run the config script from a clean slate.
    Reload,
    /// `config <key>` prints a setting, `config <key> <value...>` changes it.
    Config { key: String, args: Vec<String> },
    /// Never tile (`float`) or tile again (`tile`) the windows of an app.
    Rule { float: bool, app: String },
    /// Send the windows of an app to a Space when they open. `space` is a
    /// Space number or label, looked up when a window opens.
    SpaceRule {
        space: String,
        app: String,
        /// Go there with the window.
        follow: bool,
    },
    /// Float the windows of an app at a grid cell when they open:
    /// `[rows, cols, x, y, w, h]`.
    GridRule { grid: [u32; 6], app: String },
    /// Keep an app at hand: a name, its app, and where it floats.
    Scratchpad {
        name: String,
        app: String,
        grid: [u32; 6],
    },
    /// Show the scratchpad here, or put it away.
    ScratchpadToggle(String),
    /// How round the ring around an app's windows is.
    BorderRadiusRule { radius: i32, app: String },
    /// Name a Space (numbered from 1, as in Mission Control); `None` lists
    /// the Spaces and their names.
    Label(Option<(usize, String)>),
    /// Move the focused window to a Space, by number or label.
    Send(String),
    /// Move the focused window into another display's layout.
    SendDisplay(DisplayTarget),
    /// Switch to a Space, by number or label.
    Space(String),
    /// Tile a Space's windows (`float: false`) or leave them alone.
    Layout { space: String, float: bool },
    /// Add a keyboard shortcut line; `None` lists them.
    Bind(Option<String>),
    /// List managed windows.
    Query,
    /// List managed windows as JSON.
    QueryJson,
    /// List the Spaces of the current display as JSON.
    QuerySpaces,
    /// The version of the running window manager.
    DaemonVersion,
    /// Stop the window manager.
    Quit,
}

pub const USAGE: &str = "\
commands:
  focus  <left|right|up|down>
  swap   <left|right|up|down>
  warp   <left|right|up|down>             move into the neighbour's place, splitting it
  resize <left|right|up|down> [amount]   e.g. resize right 0.1
  rotate | flip | toggle-split
  balance [area]                           undo resizes; area: give all windows the same area
  zoom [fullscreen|parent]                 toggle zoom for the focused window
  float [r:c:x:y:w:h]                      toggle tiling for the focused window; with a
                                           grid cell, it floats there
  raise-floating                           bring floating windows to the front
  grid <r:c:x:y:w:h>                       float the focused window at a grid cell,
                                           e.g. grid 6:6:1:1:4:4 (centred)
  space <space>                            switch to a Space, also next, prev or recent
  layout <space> <bsp|float>               tile a Space, or leave its windows be
  send <space>                             move the focused window to a Space, also next,
                                           prev or recent
  send-display <next|prev|n>               move the focused window to another display
  query [--json|--spaces]                  list windows, or the Spaces as JSON
  retile | reload | quit

settings (put these in ~/.config/cranberry/cberryrc):
  config <key> [value]       e.g. config animation 0.25, config gap 10
                             keys: gap, padding, animation_duration (alias animation),
                             split_ratio, resize_step, rule_carry, poll_interval
  rule <float|tile> <app>    e.g. rule float System Settings
  rule space <space> <app>   e.g. rule space web Safari
  rule grid <r:c:x:y:w:h> <app>
                             open an app's windows floating at a grid cell,
                             e.g. rule grid 4:4:3:0:1:1 Calculator (top right)
  rule border_radius <radius> <app>
                             how round the border around an app's windows is,
                             e.g. rule border_radius 0 kitty
  label [<n> <name>]         name Space n, e.g. label 2 web; alone, list Spaces
  bind ['<hotkey>']          a keyboard shortcut, e.g. bind 'alt - h : cberry focus left';
                             alone, list them
  <space> is a Space number (from 1) or a label";

impl Command {
    pub fn parse(line: &str) -> Result<Self, String> {
        let words: Vec<&str> = line.split_whitespace().collect();
        let dir = |i: usize| -> Result<Direction, String> {
            let w = words.get(i).ok_or("missing direction")?;
            Direction::parse(w).ok_or_else(|| format!("unknown direction '{w}'"))
        };
        let no_args = |cmd: Command| {
            if words.len() == 1 {
                Ok(cmd)
            } else {
                Err(format!("'{}' takes no arguments", words[0]))
            }
        };
        let cmd = match words.first().copied() {
            None => return Err("empty command".into()),
            Some("focus") => Command::Focus(dir(1)?),
            Some("swap") => Command::Swap(dir(1)?),
            Some("warp") => Command::Warp(dir(1)?),
            Some("resize") => {
                let amount = match words.get(2) {
                    None => None,
                    Some(a) => {
                        let v: f32 = a.parse().map_err(|_| format!("bad amount '{a}'"))?;
                        if !(v > 0.0 && v < 1.0) {
                            return Err("amount must be between 0 and 1".into());
                        }
                        Some(v)
                    }
                };
                Command::Resize(dir(1)?, amount)
            }
            Some("rotate") => no_args(Command::Rotate)?,
            Some("flip") => no_args(Command::Flip)?,
            Some("toggle-split") => no_args(Command::ToggleSplit)?,
            Some("balance") => match words.get(1).copied() {
                None => Command::Balance,
                Some("area") => Command::BalanceArea,
                Some(other) => return Err(format!("balance: unknown option '{other}'")),
            },
            Some("zoom") => Command::Zoom(match words.get(1).copied() {
                None | Some("fullscreen") => Zoom::Fullscreen,
                Some("parent") => Zoom::Parent,
                Some(other) => return Err(format!("zoom: unknown mode '{other}'")),
            }),
            Some("float") => match words.get(1) {
                None => Command::ToggleFloat,
                Some(grid) => Command::ToggleFloatAt(parse_grid(grid)?),
            },
            Some("raise-floating") => no_args(Command::RaiseFloating)?,
            Some("grid") => Command::Grid(parse_grid(
                words.get(1).ok_or("grid: missing rows:cols:x:y:w:h")?,
            )?),
            Some("retile") => no_args(Command::Retile)?,
            Some("reload") => no_args(Command::Reload)?,
            Some("bind") => {
                // The rest of the line as it is: commands keep their spacing.
                let rest = line.trim_start().strip_prefix("bind").unwrap_or("").trim();
                return Ok(Command::Bind((!rest.is_empty()).then(|| rest.to_string())));
            }
            Some("query") => match words.get(1).copied() {
                None => Command::Query,
                Some("--json") => Command::QueryJson,
                Some("--spaces") => Command::QuerySpaces,
                Some(other) => return Err(format!("query: unknown option '{other}'")),
            },
            Some("daemon-version") => no_args(Command::DaemonVersion)?,
            Some("quit") => no_args(Command::Quit)?,
            Some("config") => {
                let key = words.get(1).ok_or("config: missing setting name")?;
                return Ok(Command::Config {
                    key: (*key).to_string(),
                    args: words[2..].iter().map(|w| w.to_string()).collect(),
                });
            }
            Some("send") => Command::Send(
                words
                    .get(1)
                    .ok_or("send: missing Space number or label")?
                    .to_string(),
            ),
            Some("send-display") => {
                let w = words
                    .get(1)
                    .ok_or("send-display: missing next, prev or a display number")?;
                if words.len() > 2 {
                    return Err("send-display takes one argument".into());
                }
                Command::SendDisplay(match *w {
                    "next" => DisplayTarget::Next,
                    "prev" => DisplayTarget::Prev,
                    n => match n.parse::<usize>() {
                        Ok(n) if n >= 1 => DisplayTarget::Number(n),
                        _ => return Err(format!("unknown display '{n}'")),
                    },
                })
            }
            Some("space") => Command::Space(
                words
                    .get(1)
                    .ok_or("space: missing Space number or label")?
                    .to_string(),
            ),
            Some("layout") => {
                let space = words
                    .get(1)
                    .ok_or("layout: missing Space number or label")?;
                let float = match words.get(2).copied() {
                    Some("float") => true,
                    Some("bsp" | "tile") => false,
                    _ => return Err("layout: expected bsp or float".into()),
                };
                Command::Layout {
                    space: (*space).to_string(),
                    float,
                }
            }
            Some("label") => match words.len() {
                1 => Command::Label(None),
                3 => {
                    let n: usize =
                        words[1].parse().ok().filter(|n| *n > 0).ok_or_else(|| {
                            format!("label: '{}' is not a Space number", words[1])
                        })?;
                    let name = words[2];
                    if name.parse::<usize>().is_ok() {
                        return Err("label: a label cannot be a number".into());
                    }
                    Command::Label(Some((n, name.to_string())))
                }
                _ => {
                    return Err(
                        "label: expected a Space number and a name, e.g. label 2 web".into(),
                    );
                }
            },
            Some("scratchpad") if words.get(1) == Some(&"toggle") => {
                let name = words.get(2).ok_or("scratchpad toggle: missing name")?;
                return Ok(Command::ScratchpadToggle((*name).to_string()));
            }
            Some("scratchpad") => {
                const USE: &str = "usage: scratchpad <name> <app> [rows:cols:x:y:w:h]";
                let name = words.get(1).ok_or(USE)?;
                let last = words.last().filter(|w| w.contains(':'));
                let grid = last.map_or(Ok(SCRATCHPAD_GRID), |g| parse_grid(g))?;
                let end = words.len() - usize::from(last.is_some());
                if end < 3 {
                    return Err(USE.into());
                }
                let app = words[2..end].join(" ");
                check_app_pattern(&app)?;
                if app.starts_with('/') {
                    return Err("scratchpad: the app must be a name, not a pattern".into());
                }
                return Ok(Command::Scratchpad {
                    name: (*name).to_string(),
                    app,
                    grid,
                });
            }
            Some("rule") if words.get(1) == Some(&"grid") => {
                let spec = words.get(2).ok_or("rule grid: missing rows:cols:x:y:w:h")?;
                let grid = parse_grid(spec)?;
                if words.len() < 4 {
                    return Err("rule grid: missing app name".into());
                }
                let app = words[3..].join(" ");
                check_app_pattern(&app)?;
                return Ok(Command::GridRule { grid, app });
            }
            Some("rule") if words.get(1) == Some(&"border_radius") => {
                const USE: &str = "usage: rule border_radius <radius> <app>";
                let radius = words.get(2).ok_or(USE)?;
                let radius = radius
                    .parse()
                    .ok()
                    .filter(|r| (0..=40).contains(r))
                    .ok_or("rule border_radius: must be between 0 and 40")?;
                if words.len() < 4 {
                    return Err(USE.into());
                }
                let app = words[3..].join(" ");
                check_app_pattern(&app)?;
                return Ok(Command::BorderRadiusRule { radius, app });
            }
            Some("rule") if words.get(1) == Some(&"space") => {
                let space = words
                    .get(2)
                    .ok_or("rule space: missing Space number or label")?;
                let follow = words.last() == Some(&"--follow");
                let end = words.len() - usize::from(follow);
                if end < 4 {
                    return Err("rule space: missing app name".into());
                }
                let app = words[3..end].join(" ");
                check_app_pattern(&app)?;
                return Ok(Command::SpaceRule {
                    space: (*space).to_string(),
                    app,
                    follow,
                });
            }
            Some("rule") => {
                let float =
                    match words.get(1).copied() {
                        Some("float") => true,
                        Some("tile") => false,
                        _ => return Err(
                            "rule: expected 'float', 'tile', 'space', 'grid' or 'border_radius'"
                                .into(),
                        ),
                    };
                if words.len() < 3 {
                    return Err("rule: missing app name".into());
                }
                // App names may contain spaces ("System Settings").
                let app = words[2..].join(" ");
                check_app_pattern(&app)?;
                return Ok(Command::Rule { float, app });
            }
            Some(other) => return Err(format!("unknown command '{other}'")),
        };
        let max_words = match cmd {
            Command::Focus(_)
            | Command::Swap(_)
            | Command::Warp(_)
            | Command::BalanceArea
            | Command::Zoom(_)
            | Command::Grid(_)
            | Command::ToggleFloatAt(_)
            | Command::QueryJson
            | Command::QuerySpaces
            | Command::Send(_)
            | Command::SendDisplay(_)
            | Command::Space(_) => 2,
            Command::Resize(..) | Command::Label(_) | Command::Layout { .. } => 3,
            _ => 1,
        };
        if words.len() > max_words {
            return Err(format!("too many arguments for '{}'", words[0]));
        }
        Ok(cmd)
    }
}

/// Parses `rows:cols:x:y:w:h`, checking that the cell lies inside the grid.
/// Where a scratchpad floats unless it says: the middle of the display.
const SCRATCHPAD_GRID: [u32; 6] = [6, 6, 1, 1, 4, 4];

pub fn parse_grid(spec: &str) -> Result<[u32; 6], String> {
    let bad = || format!("grid: expected rows:cols:x:y:w:h, got '{spec}'");
    let nums = spec
        .split(':')
        .map(|n| n.parse::<u32>().map_err(|_| bad()))
        .collect::<Result<Vec<_>, _>>()?;
    let grid: [u32; 6] = nums.try_into().map_err(|_| bad())?;
    let [rows, cols, x, y, w, h] = grid;
    if rows == 0 || cols == 0 || w == 0 || h == 0 {
        return Err("grid: rows, cols, width and height must be at least 1".into());
    }
    if x + w > cols || y + h > rows {
        return Err(format!(
            "grid: cell {x}:{y}:{w}:{h} does not fit a {rows}x{cols} grid"
        ));
    }
    Ok(grid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_spaces() {
        assert_eq!(
            Command::parse("label 2 web"),
            Ok(Command::Label(Some((2, "web".into()))))
        );
        assert_eq!(Command::parse("label"), Ok(Command::Label(None)));
        assert!(Command::parse("label 0 web").is_err());
        assert!(Command::parse("label web 2").is_err());
        assert!(Command::parse("label 2 3").is_err());
        assert!(Command::parse("label 2").is_err());
        assert_eq!(
            Command::parse("rule space web Google Chrome"),
            Ok(Command::SpaceRule {
                space: "web".into(),
                app: "Google Chrome".into(),
                follow: false,
            })
        );
        assert_eq!(
            Command::parse("rule space web Safari --follow"),
            Ok(Command::SpaceRule {
                space: "web".into(),
                app: "Safari".into(),
                follow: true,
            })
        );
        assert_eq!(
            Command::parse("scratchpad term kitty"),
            Ok(Command::Scratchpad {
                name: "term".into(),
                app: "kitty".into(),
                grid: SCRATCHPAD_GRID,
            })
        );
        assert_eq!(
            Command::parse("scratchpad notes Sticky Notes 4:4:1:1:2:2"),
            Ok(Command::Scratchpad {
                name: "notes".into(),
                app: "Sticky Notes".into(),
                grid: [4, 4, 1, 1, 2, 2],
            })
        );
        assert_eq!(
            Command::parse("scratchpad toggle term"),
            Ok(Command::ScratchpadToggle("term".into()))
        );
        assert!(Command::parse("scratchpad term").is_err());
        assert!(Command::parse("scratchpad toggle").is_err());
        assert!(Command::parse("scratchpad term /kitty/").is_err());
        assert!(Command::parse("rule space web").is_err());
        assert!(Command::parse("rule space web --follow").is_err());
    }

    #[test]
    fn parses_balance() {
        assert_eq!(Command::parse("balance"), Ok(Command::Balance));
        assert_eq!(Command::parse("balance area"), Ok(Command::BalanceArea));
        assert!(Command::parse("balance width").is_err());
        assert!(Command::parse("balance area now").is_err());
    }

    #[test]
    fn parses_warp() {
        assert_eq!(
            Command::parse("warp left"),
            Ok(Command::Warp(Direction::Left))
        );
        assert_eq!(Command::parse("warp j"), Ok(Command::Warp(Direction::Down)));
        assert!(Command::parse("warp").is_err());
        assert!(Command::parse("warp left now").is_err());
    }

    #[test]
    fn parses_grid_rules() {
        assert_eq!(
            Command::parse("rule grid 4:4:3:0:1:1 System Settings"),
            Ok(Command::GridRule {
                grid: [4, 4, 3, 0, 1, 1],
                app: "System Settings".into(),
            })
        );
        assert!(Command::parse("rule grid 4:4:3:0:1:1").is_err());
        assert!(Command::parse("rule grid 4:4:3:0:2:1 Finder").is_err());
        assert!(Command::parse("rule grid 0:4:0:0:1:1 Finder").is_err());
        assert!(Command::parse("rule grid 4:4:0:0:1 Finder").is_err());
        assert!(Command::parse("rule grid a:4:0:0:1:1 Finder").is_err());
        assert_eq!(Command::parse("send 3"), Ok(Command::Send("3".into())));
        assert_eq!(
            Command::parse("send next"),
            Ok(Command::Send("next".into()))
        );
        assert!(Command::parse("send 3 now").is_err());
        assert_eq!(
            Command::parse("space web"),
            Ok(Command::Space("web".into()))
        );
        assert!(Command::parse("space").is_err());
        assert_eq!(
            Command::parse("layout coding float"),
            Ok(Command::Layout {
                space: "coding".into(),
                float: true
            })
        );
        assert_eq!(
            Command::parse("layout 3 bsp"),
            Ok(Command::Layout {
                space: "3".into(),
                float: false
            })
        );
        assert!(Command::parse("layout 3").is_err());
        assert!(Command::parse("layout 3 stack").is_err());
        assert!(Command::parse("send").is_err());
        assert!(Command::parse("send-display").is_err());
        assert!(Command::parse("send-display 0").is_err());
        assert!(Command::parse("send-display left").is_err());
        assert_eq!(
            Command::parse("send-display next"),
            Ok(Command::SendDisplay(DisplayTarget::Next))
        );
        assert_eq!(
            Command::parse("send-display 2"),
            Ok(Command::SendDisplay(DisplayTarget::Number(2)))
        );
        assert!(Command::parse("send 1 2").is_err());
    }

    #[test]
    fn parses() {
        assert_eq!(
            Command::parse("focus left"),
            Ok(Command::Focus(Direction::Left))
        );
        assert_eq!(
            Command::parse("  swap   j "),
            Ok(Command::Swap(Direction::Down))
        );
        assert_eq!(
            Command::parse("resize up"),
            Ok(Command::Resize(Direction::Up, None))
        );
        assert_eq!(
            Command::parse("resize right 0.1"),
            Ok(Command::Resize(Direction::Right, Some(0.1)))
        );
        assert_eq!(Command::parse("toggle-split"), Ok(Command::ToggleSplit));
        assert_eq!(Command::parse("float"), Ok(Command::ToggleFloat));
        assert_eq!(Command::parse("zoom"), Ok(Command::Zoom(Zoom::Fullscreen)));
        assert_eq!(
            Command::parse("zoom fullscreen"),
            Ok(Command::Zoom(Zoom::Fullscreen))
        );
        assert_eq!(
            Command::parse("zoom parent"),
            Ok(Command::Zoom(Zoom::Parent))
        );
        assert_eq!(Command::parse("raise-floating"), Ok(Command::RaiseFloating));
        assert_eq!(
            Command::parse("grid 6:6:1:1:4:4"),
            Ok(Command::Grid([6, 6, 1, 1, 4, 4]))
        );
        assert!(Command::parse("grid").is_err());
        assert!(Command::parse("grid 2:2:1:0:2:1").is_err());
        assert!(Command::parse("grid 2:2:0:0:1:1 extra").is_err());
        assert_eq!(
            Command::parse("config animation 0.25"),
            Ok(Command::Config {
                key: "animation".into(),
                args: vec!["0.25".into()]
            })
        );
        assert_eq!(
            Command::parse("config gap"),
            Ok(Command::Config {
                key: "gap".into(),
                args: vec![]
            })
        );
        assert_eq!(
            Command::parse("rule float System Settings"),
            Ok(Command::Rule {
                float: true,
                app: "System Settings".into()
            })
        );
    }

    #[test]
    fn regex_rules() {
        assert_eq!(
            Command::parse("rule float /Cisco Webex/"),
            Ok(Command::Rule {
                float: true,
                app: "/Cisco Webex/".into()
            })
        );
        assert!(Command::parse("rule float /(/").is_err());
        assert!(Command::parse("rule space 2 /[/").is_err());
        assert!(Command::parse("rule grid 2:2:0:0:1:1 //").is_err());
    }

    #[test]
    fn rejects() {
        assert!(Command::parse("").is_err());
        assert!(Command::parse("focus").is_err());
        assert!(Command::parse("focus sideways").is_err());
        assert!(Command::parse("focus left now").is_err());
        assert!(Command::parse("resize left 2").is_err());
        assert!(Command::parse("rotate 1").is_err());
        assert!(Command::parse("zoom big").is_err());
        assert!(Command::parse("zoom parent now").is_err());
        assert!(Command::parse("dance").is_err());
        assert!(Command::parse("config").is_err());
        assert!(Command::parse("rule float").is_err());
        assert!(Command::parse("rule hide Finder").is_err());
    }

    #[test]
    fn query_can_ask_for_json() {
        assert_eq!(Command::parse("query"), Ok(Command::Query));
        assert_eq!(Command::parse("query --json"), Ok(Command::QueryJson));
        assert_eq!(Command::parse("query --spaces"), Ok(Command::QuerySpaces));
        assert!(Command::parse("query --xml").is_err());
    }

    #[test]
    fn border_radius_rules() {
        assert_eq!(
            Command::parse("rule border_radius 0 Google Chrome"),
            Ok(Command::BorderRadiusRule {
                radius: 0,
                app: "Google Chrome".into(),
            })
        );
        assert!(Command::parse("rule border_radius").is_err());
        assert!(Command::parse("rule border_radius 0").is_err());
        assert!(Command::parse("rule border_radius 99 kitty").is_err());
        assert!(Command::parse("rule border_radius round kitty").is_err());
    }

    #[test]
    fn float_can_name_a_grid_cell() {
        assert_eq!(Command::parse("float"), Ok(Command::ToggleFloat));
        assert_eq!(
            Command::parse("float 6:6:1:1:4:4"),
            Ok(Command::ToggleFloatAt([6, 6, 1, 1, 4, 4]))
        );
        assert!(Command::parse("float nowhere").is_err());
        assert!(Command::parse("float 6:6:1:1:4:4 more").is_err());
    }
}
