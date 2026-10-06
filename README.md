# spiceport

A cross-platform [SPICE](https://www.spice-space.org/) client written in Rust,
using SDL3 for the window, input and audio. It is a port of the behaviour of
[spice-gtk](https://gitlab.freedesktop.org/spice/spice-gtk) with no GTK, GLib,
GStreamer, pixman or OpenSSL dependency.

```
spiceport [OPTIONS] <spice-url>
spiceport -v spice://localhost:5900
```

See [PLAN.md](PLAN.md) for the design and milestones.

## Building

You need a Rust toolchain plus **cmake** and a **C compiler**: SDL3 (and libopus,
with the default `opus` feature) are built from source and linked statically, so
the resulting binary needs no system SDL.

On Linux, SDL only gets working video and audio backends if the development
headers are present when it is built:

```
sudo apt-get install cmake pkg-config libasound2-dev libdbus-1-dev libegl-dev \
  libgl-dev libpulse-dev libudev-dev libwayland-dev libx11-dev libxcursor-dev \
  libxext-dev libxfixes-dev libxi-dev libxkbcommon-dev libxrandr-dev libxss-dev \
  libxtst-dev wayland-protocols
cargo build --release
```

The devcontainer in `.devcontainer/` has all of this installed.

## Layout

| Path | Contents |
|---|---|
| `crates/spice-proto` | Wire types and (de)marshalling; no I/O |
| `crates/spice-decode` | Image decoders (QUIC, LZ, GLZ, LZ4, JPEG, ...) |
| `crates/spice-canvas` | Surfaces, draw operations and caches |
| `crates/spice-client` | tokio-based session and channels; no SDL |
| `spiceport` | The binary: CLI, logging, SDL frontend |
| `xtask` | `cargo xtask` helpers (keymap generation) |
| `spice-gtk`, `spice-protocol` | Reference C sources (git submodules); not built |

## Logging

`-v`, `-vv`, `-vvv` raise the level to info, debug and trace; `-q` shows errors
only. `RUST_LOG` overrides both, per module:

```
RUST_LOG=spice_client::display_ch=trace spiceport spice://host:5900
```

`--log-file PATH` also writes the log, without colour, to a file.

## Licence

LGPL-2.1-or-later, the same as spice-gtk and spice-common, from which the
decoders and canvas are ported. See [LICENSE](LICENSE).
