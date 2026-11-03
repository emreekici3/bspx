# Keyboard shortcuts

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

[Back to the README](../README.md)
