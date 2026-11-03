# cranberry

A BSP tiling window manager for macOS. Every Space and display keeps its own layout. SIP stays on.

<img src="assets/demo/balance-v2.gif" width="800" alt="cranberry balancing a layout">

## Install

You need [Rust](https://rustup.rs).

```sh
git clone https://github.com/emreekici3/cranberry.git
cd cranberry
cargo install --path . --root ~

echo 'export PATH="$HOME/bin:$PATH"' >> ~/.zshenv
source ~/.zshenv

mkdir -p ~/.config/cranberry
cp examples/cberryrc ~/.config/cranberry/cberryrc

cberry install
cberry start
```

Then enable `cberry` under *System Settings → Privacy & Security → Accessibility*.

## Update

```sh
git pull
cargo install --path . --root ~
cberry restart
```

If windows stop moving, remove `cberry` from the Accessibility list, add `~/bin/cberry` again and run `cberry restart`.

## Docs

- [Commands](docs/commands.md)
- [Keyboard shortcuts](docs/shortcuts.md)
- [Mouse](docs/mouse.md)
- [Spaces and displays](docs/spaces.md)
- [Configuration](docs/configuration.md)
- [Demo](docs/demo.md)

## License

MIT
