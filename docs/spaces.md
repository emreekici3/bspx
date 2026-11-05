# Spaces and displays

## Spaces

Name your Spaces (numbered from 1, as in Mission Control) and send apps to them, in `cberryrc`:

```sh
cberry label 1 general
cberry label 2 web
cberry label 3 coding
cberry rule space web "Safari"
```

New Safari windows then go to Space 2. With `--follow` you go with them: `cberry rule space web "Safari" --follow`. Windows that were already open stay where they are. `cberry space web` switches to Space 2, and `cberry send web` (or `cberry send 2`) moves the focused window there, e.g. with these shortcuts:

```sh
cberry bind 'alt - 1 : cberry space 1'
cberry bind 'alt - 2 : cberry space 2'
cberry bind 'alt - 3 : cberry space 3'
cberry bind 'shift + alt - 1 : cberry send 1'
cberry bind 'shift + alt - 2 : cberry send 2'
cberry bind 'shift + alt - 3 : cberry send 3'
```

`cberry layout coding float` stops tiling Space 3: its windows stay where you put them, and `alt`-drags still move and resize them. `cberry layout coding bsp` tiles it again.

`next` and `prev` go round the Spaces of the display, and `recent` goes back to the last one:

```sh
cberry bind 'alt - tab : cberry space recent'
cberry bind 'alt - right : cberry space next'
cberry bind 'alt - left : cberry space prev'
cberry bind 'shift + alt - right : cberry send next'
```

Spaces switch and windows move at once, with SIP enabled. To go along with a window, follow it; it keeps the focus:

```sh
cberry bind 'shift + alt - 1 : cberry send 1 && cberry space 1'
```

Turn on the macOS *Switch to Desktop N* shortcuts too, in *System Settings → Keyboard → Keyboard Shortcuts → Mission Control*. cranberry uses them when a switch at once cannot work, as in Mission Control. If a window cannot be moved at once, cranberry holds it by its title bar and switches Spaces: you go along with it. `cberry config rule_carry off` keeps Space rules from doing this to a new window; it stays on the current Space.

## Scratchpads

A scratchpad is an app at hand on every Space. Name it and bind a key, in `cberryrc`:

```sh
cberry scratchpad term "kitty"
cberry bind 'alt - s : cberry scratchpad toggle term'
```

The key brings its window to the current Space, floating in the middle and in focus, and starts the app if it is not running. Pressed again it hides the app, and focus goes back. Leaving the Space hides it too. Its windows are never tiled, so choose an app you keep for this.

## Displays

Each display has a layout of its own; a new window splits the window last focused on its display. Commands act on the display of the focused window.

At the edge of a display, `cberry focus`, `swap` and `warp` carry on to the next display: `focus right` from a window against the right edge focuses the nearest window on the display to the right, `swap right` trades the two windows' places, and `warp right` moves the window into that display's layout (onto it alone if it has no window). `cberry send-display next`, `prev` or a number (from 1, left to right) moves the focused window straight to that display. Dragging a window onto another display's window splits that window, on the side nearest the pointer; dropping it on a part of another display with no window moves it into that display's layout.

A display that is unplugged hands its windows to the ones left, as macOS moves them. Spaces are numbered from 1 on each display, as Mission Control shows them: `cberry space 2`, `send 2` and `label` act on the Spaces of the current display, and a label or `layout` setting names that number on every display. With *Displays have separate Spaces* off (*System Settings → Desktop & Dock*), the displays share one Space and only the main one is tiled.

[Back to the README](../README.md)
