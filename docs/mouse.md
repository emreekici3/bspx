# Mouse

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

[Back to the README](../README.md)
