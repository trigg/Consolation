# Consolation

Consolation is a Wayland compositor based on [Smithay](https://github.com/Smithay/smithay), forked from the reference commpositor Anvil. 

Consolation is intended to fill the feature gaps for fullscreen gaming compositors. 

## Installation

To be done

## Running

Currently the compositor doesn't autodetect the running environment, so the backend must be explicitly passed on start

`consolation --tty-udev`
For running on a TTY console


`consolation --winit`
For running as an embedded window inside an X11 or Wayland environment (intended for testing)

### Debug

```
cargo run -- --winit
```

## Features

Currently this is not as feature complete as hoped. More to come soon!

- One screen focused at a time
- Screen aspect-scaled to fit display
- 'Menu' key right of R-Alt used to open menu.
- - Arrow keys navigate options, Enter to select, Backspace to go back
- - Switch between active windows
- - More settings & controls to come
- wlroots layer shell to allow overlays, popups, and panels

