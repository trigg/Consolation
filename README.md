# Consolation

Consolation is a Wayland compositor based on [Smithay](https://github.com/Smithay/smithay), forked from the reference commpositor Anvil.

Consolation is intended to fill the feature gaps for fullscreen gaming compositors.

## Installation

### From Sources
```
git clone https://github.com/trigg/Consolation.git
cd Consolation
cargo build --release
```
and the binary will be in
`./target/release/consolation`

## Running

`consolation`

Consolation will autodetect an X11 or Wayland environment and fall back to drawing with the winit backend if needed.

### Debug

`cargo run`

## Features

Currently this is not as feature complete as hoped. More to come soon!

- One window focused at a time
- - ~~Pop ups kept to parent scale~~
- Window aspect-scaled to fit display
- 'Menu' key or 'Alt Gr' used to open menu.
- - Arrow keys navigate options, Enter to select, Backspace to go back
- - Switch between active windows
- - More settings & controls to come
- wlroots layer shell to allow overlays, popups, and panels

