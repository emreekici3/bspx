# Commands

`<dir>` is `left`, `down`, `up`, `right` (or `h`, `j`, `k`, `l`).

### Windows

| Command | What it does |
|---|---|
| `cberry focus <dir>` | Focus the neighbouring window |
| `cberry swap <dir>` | Swap with the neighbouring window |
| `cberry warp <dir>` | Move into the neighbouring window's place, splitting it |
| `cberry resize <dir> [amount]` | Move a border of the window, tiled or floating |
| `cberry zoom [fullscreen\|parent]` | Grow the window over the others, or put it back |

### Layout

| Command | What it does |
|---|---|
| `cberry rotate` | Rotate the layout 90° |
| `cberry flip` | Swap the two halves of a split |
| `cberry toggle-split` | Switch a split between side by side and stacked |
| `cberry balance [area]` | Undo every resize; `area` gives every window the same area |

### Floating

| Command | What it does |
|---|---|
| `cberry float` | Float or tile the focused window |
| `cberry raise-floating` | Bring floating windows to the front |
| `cberry grid <rows:cols:x:y:w:h>` | Float the window at a grid cell; `6:6:1:1:4:4` centres it |

### Spaces & displays

| Command | What it does |
|---|---|
| `cberry space <space>` | Switch to a Space |
| `cberry send <space>` | Move the focused window to a Space |
| `cberry send-display <next\|prev\|n>` | Move the focused window to another display |
| `cberry label [<n> <name>]` | Name Space `n`; alone, list the Spaces |
| `cberry layout <space> bsp\|float` | Tile a Space, or leave its windows where you put them |

### Rules

| Command | What it does |
|---|---|
| `cberry rule float\|tile "<app>"` | Never tile an app's windows, or tile them again |
| `cberry rule space <space> "<app>"` | Send an app's new windows to a Space |
| `cberry rule grid <rows:cols:x:y:w:h> "<app>"` | Open an app's new windows floating at a grid cell |

### Service

| Command | What it does |
|---|---|
| `cberry install` / `uninstall` | Add or remove the login service |
| `cberry start` / `stop` / `restart` | Control the service |
| `cberry run` | Run in the terminal instead of as a service |
| `cberry reload` | Reload the config |
| `cberry config <key> [value]` | Show or change a setting |
| `cberry bind ['<hotkey>']` | Add a keyboard shortcut; alone, list them |
| `cberry query` | List windows with their app names |
| `cberry version` | Print the version |

[Back to the README](../README.md)
