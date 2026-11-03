# cranberry

A BSP tiling window manager for macOS. Every Space and display keeps its own layout. SIP stays on.

## Install

```sh
brew install emreekici3/formulae/cranberry

mkdir -p ~/.config/cranberry
cp "$(brew --prefix)/share/cranberry/cberryrc" ~/.config/cranberry/cberryrc

cberry install
cberry start
```

Then enable `cberry` under *System Settings → Privacy & Security → Accessibility*.

To update, run `brew upgrade cranberry` and `cberry restart`. If windows stop moving, remove `cberry` from the Accessibility list, add it again and run `cberry restart`.

### Manual install

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

Enable `~/bin/cberry` under Accessibility as above. To update, run `git pull`, the `cargo install` line and `cberry restart`.

## Docs

- [Commands](docs/commands.md)
- [Keyboard shortcuts](docs/shortcuts.md)
- [Mouse](docs/mouse.md)
- [Spaces and displays](docs/spaces.md)
- [Configuration](docs/configuration.md)

## License

MIT
