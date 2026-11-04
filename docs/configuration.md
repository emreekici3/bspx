# Configuration

The config is a shell script at `~/.config/cranberry/cberryrc`, run on startup:

```sh
#!/bin/sh
cberry config gap 8
cberry config padding 8 8 8 8
cberry config animation 0.25
cberry rule float "System Settings"
```

Start from the example config:

```sh
mkdir -p ~/.config/cranberry
curl -L https://raw.githubusercontent.com/emreekici3/cranberry/master/examples/cberryrc -o ~/.config/cranberry/cberryrc
cberry reload
```

| Setting | Description | Default |
|---|---|---|
| `gap` | Space between windows | `8` |
| `padding` | Space around the edges | `8 8 8 8` |
| `animation` | Animation duration in seconds, `0` to disable | `0.42` |
| `split_ratio` | Share the old window keeps when split | `0.5` |
| `resize_step` | Step for `cberry resize` | `0.05` |
| `rule_carry` | Let a Space rule carry a new window there, pointer and all, `on` / `off` | `on` |
| `drag_swap` | Swap windows by drag and drop, `on` / `off` | `on` |
| `drag_color` | Drop highlight colour, `RRGGBB` or `RRGGBBAA` | `5e9eff40` |
| `mouse_modifier` | Key to hold for mouse moves and resizes: `alt`, `cmd`, `ctrl`, `shift`, `fn` or `off` | `alt` |
| `mouse_insert_modifier` | Also held while moving a window, dropping it splits the window under it instead of swapping: `alt`, `cmd`, `ctrl`, `shift`, `fn` or `off` | `off` |
| `mouse_action1` | Left-drag with the modifier: `move` or `resize` | `move` |
| `mouse_action2` | Right-drag with the modifier: `move` or `resize` | `resize` |
| `mouse_follows_focus` | Move the pointer to windows focused from the keyboard, `on` / `off` | `off` |
| `focus_follows_mouse` | Focus the window under the pointer, `on` / `off` | `off` |
| `log_level` | `error`, `warn`, `info` or `debug`; try `debug` when something misbehaves | `info` |

`cberry rule float "<app>"` keeps an app's windows out of the layout. In rules, use the app name exactly as `cberry query` shows it; on a non-English Mac it is the localized name (e.g. `"Sistem Ayarları"`). The built-in float rules use the English names (System Settings, Calculator, Archive Utility), so add your own with the local names.

An app name between slashes is a regular expression, searched for anywhere in the name. It helps with apps whose name changes or is hard to type:

```sh
cberry rule float "/Webex/"          # any app with Webex in its name
cberry rule float "/^Zoom/"          # names starting with Zoom
cberry rule space 3 "/(?i)slack/"    # ignoring case
```

Any other name must match exactly, so names with brackets or dots need no escaping. `cberry query` ends a floating window's line with the rule that floats it, e.g. `rule: /Webex/`; a window without one matched no rule.

Floating windows cannot be kept above tiled ones without disabling SIP, so cranberry does not try; `cberry raise-floating` brings them all back to the front when you need them.

[Back to the README](../README.md)
