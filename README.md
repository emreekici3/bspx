<h1 align="center">cranberry</h1>

<p align="center">
  An animated BSP tiling window manager for macOS, written in Rust.
</p>

<details>
<summary><b>Demo</b></summary>
<br>

<details><summary>Focus — <code>cberry focus</code></summary><br><img src="assets/demo/focus.gif" width="800" alt="Focus"></details>
<details><summary>Swap — <code>cberry swap</code></summary><br><img src="assets/demo/swap.gif" width="800" alt="Swap"></details>
<details><summary>Warp — <code>cberry warp</code></summary><br><img src="assets/demo/warp.gif" width="800" alt="Warp"></details>
<details><summary>Resize — <code>cberry resize</code></summary><br><img src="assets/demo/resize.gif" width="800" alt="Resize"></details>
<details><summary>Rotate — <code>cberry rotate</code></summary><br><img src="assets/demo/rotate.gif" width="800" alt="Rotate"></details>
<details><summary>Toggle split — <code>cberry toggle-split</code></summary><br><img src="assets/demo/toggle-split.gif" width="800" alt="Toggle split"></details>
<details><summary>Balance — <code>cberry balance</code></summary><br><img src="assets/demo/balance-v2.gif" width="800" alt="Balance"></details>
<details><summary>Zoom — <code>cberry zoom</code></summary><br><img src="assets/demo/zoom.gif" width="800" alt="Zoom"></details>
<details><summary>Zoom parent — <code>cberry zoom parent</code></summary><br><img src="assets/demo/zoom-parent.gif" width="800" alt="Zoom parent"></details>
<details><summary>Float — <code>cberry float</code></summary><br><img src="assets/demo/float.gif" width="800" alt="Float"></details>
<details><summary>Swap with the mouse</summary><br><img src="assets/demo/swap-with-mouse.gif" width="800" alt="Swap with the mouse"></details>
<details><summary>Split by dropping</summary><br><img src="assets/demo/warp-with-mouse.gif" width="800" alt="Split by dropping"></details>
<details><summary>Move with <code>alt</code>-drag</summary><br><img src="assets/demo/move-window.gif" width="800" alt="Move with <code>alt</code>-drag"></details>
<details><summary>Resize with <code>alt</code>-right-drag</summary><br><img src="assets/demo/resize-window.gif" width="800" alt="Resize with <code>alt</code>-right-drag"></details>
<details><summary>Switch Spaces — <code>cberry space</code></summary><br><img src="assets/demo/go-space.gif" width="800" alt="Switch Spaces"></details>
<details><summary>Take a window to a Space — <code>cberry send</code></summary><br><img src="assets/demo/carry-space.gif" width="800" alt="Take a window to a Space"></details>

</details>

## Requirements

- [Rust](https://rustup.rs)

## Installation

**1. Build it** into `~/bin`:

```sh
git clone https://github.com/emreekici3/cranberry.git
cd cranberry
cargo install --path . --root ~
```

**2. Add `~/bin` to your `PATH`.** Use `~/.zshenv` rather than `~/.zshrc`, so background tools can find `cberry` too:

```sh
echo 'export PATH="$HOME/bin:$PATH"' >> ~/.zshenv
source ~/.zshenv
```

**3. Install and start the service.** It runs in the background and starts automatically when you log in:

```sh
cberry install
cberry start
```

**4. Grant Accessibility permission.** macOS asks on the first start. Enable `cberry` under *System Settings → Privacy & Security → Accessibility*. cranberry starts within a few seconds of the permission being granted; if it does not, run `cberry restart`.

### Updating

```sh
cd cranberry
git pull
cargo install --path . --root ~
cberry restart
```

If windows stop moving after an update, macOS has dropped the permission for the new binary: remove `cberry` from the Accessibility list, add `~/bin/cberry` again, and run `cberry restart`.

## Commands

| Command | What it does |
|---|---|
| `cberry focus <dir>` | Focus the neighbouring window |
| `cberry swap <dir>` | Swap with the neighbouring window |
| `cberry warp <dir>` | Move into the neighbouring window's place: it splits as for a new window, and the window keeps the side it came from |
| `cberry resize <dir> [amount]` | Move a border of the window toward `<dir>`: its left (top) border, or its right (bottom) one when it is against the left (top) of the screen. Works on tiled and floating windows |
| `cberry rotate` | Rotate the layout 90° |
| `cberry flip` | Swap the two halves of a split |
| `cberry toggle-split` | Switch a split between side by side and stacked |
| `cberry balance [area]` | Undo every resize, as the windows were when they opened; `area` gives every window the same area instead |
| `cberry zoom [fullscreen\|parent]` | Grow the focused window over the others to the full screen (default) or its split's area; run again to put it back |
| `cberry float` | Float or tile the focused window |
| `cberry raise-floating` | Bring floating windows to the front |
| `cberry grid <rows:cols:x:y:w:h>` | Float the focused window and put it at a grid cell, e.g. `6:6:1:1:4:4` centres it at two thirds of the screen |
| `cberry query` | List windows with their app names |
| `cberry config <key> [value]` | Show or change a setting |
| `cberry rule float\|tile "<app>"` | Never tile an app's windows, or tile them again; `"/regex/"` matches app names by pattern |
| `cberry rule space <space> "<app>"` | Send an app's new windows to a Space |
| `cberry rule grid <rows:cols:x:y:w:h> "<app>"` | Open an app's new windows floating at a grid cell, e.g. `4:4:3:0:1:1` is the top-right quarter of the top-right quarter |
| `cberry label [<n> <name>]` | Name Space `n`; alone, list the Spaces |
| `cberry bind ['<hotkey>']` | Add a keyboard shortcut; alone, list them |
| `cberry space <space>` | Switch to a Space |
| `cberry layout <space> bsp\|float` | Tile a Space, or leave its windows where you put them |
| `cberry send <space>` | Move the focused window to a Space |
| `cberry send-display <next\|prev\|n>` | Move the focused window to another display (numbered from 1, left to right) |
| `cberry reload` | Reload the config |
| `cberry install` | Install the login service |
| `cberry start` / `stop` / `restart` | Control the service |
| `cberry uninstall` | Remove the service |
| `cberry run` | Run in the terminal instead of as a service |

`<dir>` is `left`, `down`, `up`, `right` (or `h`, `j`, `k`, `l`).

## Keyboard shortcuts

Bind them in `cberryrc`, one `cberry bind` per shortcut: modifiers, `-`, the key, `:` and the command.

```sh
cberry bind 'alt - h : cberry focus left'
cberry bind 'alt - j : cberry focus down'
cberry bind 'alt - k : cberry focus up'
cberry bind 'alt - l : cberry focus right'

cberry bind 'shift + alt - h : cberry swap left'
cberry bind 'shift + alt - j : cberry swap down'
cberry bind 'shift + alt - k : cberry swap up'
cberry bind 'shift + alt - l : cberry swap right'

cberry bind 'meh - h : cberry warp left'
cberry bind 'meh - j : cberry warp down'
cberry bind 'meh - k : cberry warp up'
cberry bind 'meh - l : cberry warp right'

cberry bind 'ctrl + alt - h : cberry resize left'
cberry bind 'ctrl + alt - j : cberry resize down'
cberry bind 'ctrl + alt - k : cberry resize up'
cberry bind 'ctrl + alt - l : cberry resize right'

cberry bind 'alt - r : cberry rotate'
cberry bind 'alt - e : cberry toggle-split'
cberry bind 'alt - 0 : cberry balance'
cberry bind 'alt - m : cberry zoom'
cberry bind 'shift + alt - m : cberry zoom parent'
cberry bind 'alt - t : cberry float'
cberry bind 'alt - f : cberry raise-floating'
cberry bind 'alt - g : cberry grid 6:6:1:1:4:4'

cberry bind 'alt - return : open -na Terminal'
```

Modifiers are `alt`, `shift`, `cmd`, `ctrl` and `fn`, or one side only with `lalt`, `ralt`, `lshift` and so on; `hyper` is `cmd + alt + shift + ctrl` and `meh` is `alt + shift + ctrl`. The key is a character (whatever key types it on your keyboard layout), a named key (`return`, `tab`, `space`, `escape`, `backspace`, `delete`, `home`, `end`, `pageup`, `pagedown`, `left`, `right`, `up`, `down`, `f1` to `f20`) or a key code such as `0x32`. Put `->` before the `:` to let the key press reach the app too.

A command that is just `cberry ...` runs inside cranberry, without starting a process; anything else runs in your `$SHELL`. `cberry bind` alone lists the shortcuts.

## Mouse

- Drag a window by its title bar onto another to swap them.
- Drag the edge of a tiled window to move the split border.
- Hold `alt` and drag anywhere on a window to move it; drop it on another window to swap them.
- Hold `alt` and right-drag to resize a window from its nearest corner; the windows next to it follow.

To split a window instead of swapping with it, pick a second key:

```sh
cberry config mouse_insert_modifier cmd
```

Hold it along with `alt` and drop a window near an edge of another: that window is split in two and the dropped one takes the half on that side (the highlight shows which).

While `alt` is held, clicks do not reach apps. If an app needs `alt`-clicks, pick another key with `mouse_modifier`.

Focus follows mouse and mouse follows focus are off by default:

```sh
cberry config focus_follows_mouse on   # focus the window under the pointer
cberry config mouse_follows_focus on   # move the pointer to windows focused with cberry focus / swap
```

With focus follows mouse, the focused window is also raised: macOS cannot focus a window without raising it unless SIP is disabled. So that floating windows are not buried, a tiled window that would cover one is left alone until you click it. Focus also stays put while Spotlight, a launcher or a menu is open, so they do not close under the pointer.

## Spaces

Name your Spaces (numbered from 1, as in Mission Control) and send apps to them, in `cberryrc`:

```sh
cberry label 1 general
cberry label 2 web
cberry label 3 coding
cberry rule space web "Safari"
```

New Safari windows then go to Space 2. Windows that were already open stay where they are. `cberry space web` switches to Space 2, and `cberry send web` (or `cberry send 2`) moves the focused window there, e.g. with these shortcuts:

```sh
cberry bind 'alt - 1 : cberry space 1'
cberry bind 'alt - 2 : cberry space 2'
cberry bind 'alt - 3 : cberry space 3'
cberry bind 'shift + alt - 1 : cberry send 1'
cberry bind 'shift + alt - 2 : cberry send 2'
cberry bind 'shift + alt - 3 : cberry send 3'
```

`cberry layout coding float` stops tiling Space 3: its windows stay where you put them, and `alt`-drags still move and resize them. `cberry layout coding bsp` tiles it again.

These use the macOS *Switch to Desktop N* shortcuts, so turn them on first: *System Settings → Keyboard → Keyboard Shortcuts → Mission Control*. With SIP enabled, recent macOS only lets a window change Spaces the way a person does it, so cranberry holds the window by its title bar and switches Spaces: you go along with it.

## Displays

Each display has a layout of its own; a new window splits the window last focused on its display. Commands act on the display of the focused window.

At the edge of a display, `cberry focus`, `swap` and `warp` carry on to the next display: `focus right` from a window against the right edge focuses the nearest window on the display to the right, `swap right` trades the two windows' places, and `warp right` moves the window into that display's layout (onto it alone if it has no window). `cberry send-display next`, `prev` or a number (from 1, left to right) moves the focused window straight to that display. Dragging a window onto another display's window splits that window, on the side nearest the pointer; dropping it on a part of another display with no window moves it into that display's layout.

A display that is unplugged hands its windows to the ones left, as macOS moves them. Spaces are numbered from 1 on each display, as Mission Control shows them: `cberry space 2`, `send 2` and `label` act on the Spaces of the current display, and a label or `layout` setting names that number on every display. With *Displays have separate Spaces* off (*System Settings → Desktop & Dock*), the displays share one Space and only the main one is tiled.

## Configuration

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

## License

MIT. See `LICENSE.txt`.
