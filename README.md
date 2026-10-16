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

## What works

- Plain TCP, TLS (`spice+tls://`, `--ca-file`, `--host-subject`,
  `--secure-channels`) and Unix sockets (not on Windows); SPICE ticket
  passwords (from the URL, `--password-stdin`, `SPICE_PASSWORD`, or a prompt).
- Display: every SPICE image compression (QUIC, LZ, GLZ, ZLIB_GLZ, LZ4,
  JPEG, JPEG_ALPHA, bitmaps), every draw operation, and MJPEG video streams
  synchronised to the audio clock. H.264 streams with the optional `h264`
  feature (see below).
- Keyboard and mouse in both server (grabbed) and client (agent) mouse
  modes, the guest cursor, lock-key synchronisation.
- With spice-vdagent in the guest: the guest resolution follows the window,
  and text copy/paste works both ways.
- Audio playback and microphone recording, raw or Opus.

Not yet: VP8/VP9/H.265 streams, file transfer, USB redirection, multiple
monitors, smart cards, migration, SASL.

## Installing

Tagged releases on GitHub carry:

| Platform | Artifacts |
|---|---|
| Linux | `.tar.gz` with the binary, and an `.AppImage` |
| macOS | `spiceport.app` (zipped) and a bare-binary `.tar.gz` |
| Windows | `.zip` with `spiceport.exe` |

The binaries link SDL statically. On Linux they need only glibc; SDL
loads X11/Wayland and ALSA/PulseAudio/PipeWire libraries at run time.

On macOS run the app with a URL: `open -a spiceport --args spice://host:5900`,
or `spiceport.app/Contents/MacOS/spiceport spice://host:5900`. The microphone
permission prompt only appears when the guest starts recording. To grab
system shortcuts (Cmd-Tab and so on) the terminal or app needs the
Accessibility permission.

## Using

```
spiceport [OPTIONS] <URL>

spiceport spice://localhost:5900
spiceport "spice://host?port=5900&tls-port=5901" --ca-file ca-cert.pem --secure-channels main
spiceport spice+tls://host:5901 --ca-file ca-cert.pem --host-subject "C=GB,O=Example,CN=host"
```

Run `spiceport --help` for every option.

| Keys (default `--release-keys`) | Action |
|---|---|
| Ctrl+Alt | Release the mouse/keyboard grab, or grab again |
| Ctrl+Alt+End | Send Ctrl+Alt+Del to the guest |
| Ctrl+Alt+Q | Quit |
| Shift+F11 | Toggle fullscreen |

Click in the window to grab the mouse when the guest has no agent (server
mouse mode). Exit status is 0 on a clean disconnect, 1 on connection or
authentication errors, and 2 for bad options or URLs.

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

### H.264 video streams

`cargo build --release --features h264` adds H.264 stream decoding through the
system FFmpeg's libavcodec. It is off by default because it links FFmpeg
dynamically (libavcodec, libavformat and libavutil, FFmpeg 3.4 or later) and
needs their development files and libclang to build:

```
sudo apt-get install libavcodec-dev libavformat-dev libavutil-dev libclang-dev
```

FFmpeg rather than openh264 because spice-server encodes the guest's RGB
surface with x264 at 4:4:4 (High 4:4:4 Predictive), which openh264 can't
decode. Distribute such builds under LGPL terms only with an FFmpeg built
without `--enable-gpl`.

spice-server picks MJPEG unless the client prefers otherwise, so ask for H.264
with `--preferred-video-codec h264,mjpeg`. The server needs GStreamer's
`x264enc` (gstreamer1.0-plugins-ugly on Debian/Ubuntu).

## Layout

| Path | Contents |
|---|---|
| `crates/spice-proto` | Wire types and (de)marshalling; no I/O |
| `crates/spice-decode` | Image decoders (QUIC, LZ, GLZ, LZ4, JPEG, ...) |
| `crates/spice-canvas` | Surfaces, draw operations and caches |
| `crates/spice-client` | tokio-based session and channels; no SDL |
| `spiceport` | The binary: CLI, logging, SDL frontend |
| `xtask` | `cargo xtask gen-keymap` and `cargo xtask dist` (packaging) |
| `packaging` | macOS `Info.plist`/entitlements, Linux `.desktop`, icon |
| `scripts` | Test servers: QEMU with SPICE, TLS certificates |
| `tests/c-oracle` | spice-common's C encoders, for golden decoder tests |
| `fuzz` | `cargo fuzz` targets for decoders and message parsing |
| `spice-gtk`, `spice-protocol` | Reference C sources (git submodules); not built |

## Testing

`cargo test --workspace` runs the unit tests, property tests and, when a C
compiler, glib and the `spice-gtk` submodule are available, golden tests that
compare the decoders with spice-common's own encoders
(`SPICEPORT_REQUIRE_ORACLE=1` makes a missing oracle an error, as on CI).

For manual testing against a real server:

```
scripts/test-server.sh                          # QEMU on port 5900, no password
scripts/test-server.sh --port 5910 --password secret --streaming all -- -cdrom some.iso
scripts/gen-test-certs.sh /tmp/certs
scripts/test-server.sh --tls-port 5901 --tls-dir /tmp/certs
spiceport --ca-file /tmp/certs/ca-cert.pem spice+tls://localhost:5901
```

`cargo run -p spice-client --example screenshot -- spice://localhost:5900 out.ppm`
captures the guest display without a window. `SPICE_VIDEO_CODECS=3,1` (with
`--features h264`) makes it prefer H.264 streams, and `SPICE_DEBUG=1` adds
debug logging.

## Releasing

`cargo xtask dist [--target T]` builds a release binary and packages it for
the host OS into `target/dist/` (an AppImage too, when `appimagetool` is on
`PATH` or `APPIMAGETOOL` points at it). Pushing a `v*` tag runs
`.github/workflows/release.yml`, which builds Linux (x86_64, aarch64),
Windows (x86_64) and macOS (arm64, x86_64) and drafts a GitHub release.
macOS builds are signed and notarised when these secrets are set:
`MACOS_CERT_P12` (base64 .p12), `MACOS_CERT_PASSWORD`, `MACOS_SIGN_IDENTITY`,
`APPLE_ID`, `APPLE_TEAM_ID` and `APPLE_APP_PASSWORD`.

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
