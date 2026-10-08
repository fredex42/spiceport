# spiceport — plan for a Rust + SDL port of spice-gtk

Goal: one cross-platform executable (Windows, macOS, Linux), run as

```
spiceport [OPTIONS] <spice-url>
```

that connects to a SPICE server (QEMU/libvirt), shows the guest display in an
SDL window, plays and records audio through SDL, and forwards keyboard and mouse
input. It uses no GTK, GLib, GStreamer, pixman or OpenSSL.

The C source in [spice-gtk/](spice-gtk/) (plus its `spice-common` and
`keycodemapdb` submodules) and the protocol headers in
[spice-protocol/](spice-protocol/) are the reference. We port behaviour, not structure:
GObject signals, coroutines and the GTK widget are replaced with Rust-native
equivalents.

---

## 1. Scope

### In scope for v1 (the "MVP")

| Area | spice-gtk source | Notes |
|---|---|---|
| URI parsing: `spice://`, `spice+tls://`, `spice+unix://` (Unix only) | `spice-session.c:spice_parse_uri` | Same rules, including `?port=&tls-port=&password=` |
| Link handshake, capabilities, mini-header | `spice-channel.c` | |
| Ticket (RSA-OAEP password) auth | `spice-channel.c:spice_channel_send_spice_ticket` | |
| TLS (`tls-port`, `--ca-file`, `--host-subject`, `--secure-channels`) | `spice-channel.c`, `spice-common/common/ssl_verify.c` | rustls |
| Base channel messages: ping/pong, set_ack, notify, disconnecting, wait_for_channels | `channel-base.c` | |
| Main channel: init, channels list, mouse mode, agent tokens, name/uuid, mm time | `channel-main.c` | Migration comes later |
| VD agent: capabilities, monitors config (resize guest to window), mouse state, text clipboard | `channel-main.c`, `spice-gtk-session.c` | File transfer comes later |
| Display channel: surfaces, draw ops, image caches, compression formats, MJPEG streams | `channel-display*.c`, `spice-common/common/canvas_base.c`, `sw_canvas.c` | See section 6 |
| Cursor channel | `channel-cursor.c` | |
| Inputs channel: scancodes, modifiers, server and client mouse modes, motion ack | `channel-inputs.c`, `spice-widget.c` | |
| Playback: raw S16 and Opus | `channel-playback.c`, `spice-common/common/snd_codec.c` | |
| Record (microphone): raw and Opus | `channel-record.c` | |
| Keyboard grab, pointer grab, release key combo, fullscreen | `spice-widget.c`, `spice-grabsequence.c`, `tools/spicy.c` | |

### Later phases (designed for, not built in v1)

- H.264 video streams (FFmpeg libavcodec, see section 5), then VP8/VP9/H.265 if a pure-Rust or easily bundled decoder is available.
- Agent file transfer (drag-and-drop a file onto the window), richer clipboard (images).
- Seamless and semi-seamless migration (`SPICE_MSG_MAIN_MIGRATE_*`).
- Multiple monitors (one SDL window per display channel / monitor).
- SASL auth. There is no mature pure-Rust Cyrus SASL; it would go behind a cargo feature.
- HTTP CONNECT proxy (`SPICE_PROXY`, `spice-uri.c`).
- Port and WebDAV channels (folder sharing).

### Out of scope

- USB redirection (`usb-*.c`, `channel-usbredir.c`, `cd-*.c`): needs libusb, usbredir, plus a Windows driver. Large and platform-specific.
- Smartcard (libcacard).
- GL scanout (`SPICE_MSG_DISPLAY_GL_SCANOUT*_UNIX`): local Unix only, using dmabuf fd passing. We will not advertise `SPICE_DISPLAY_CAP_GL_SCANOUT`.
- The GObject/introspection library API.

---

## 2. Protocol reference: spice-protocol

The protocol headers come from **spice-protocol**, checked out at
[spice-protocol/](spice-protocol/) in the repo root. The files we port from:

| Header | What we take from it | Ported to |
|---|---|---|
| `spice/protocol.h` | `SPICE_MAGIC` ("REDQ"), version 2.2, `SpiceLinkHeader`/`SpiceLinkMess`/`SpiceLinkReply`, `SpiceDataHeader`/`SpiceMiniDataHeader`, all `*_CAP_*` capability bits, `SPICE_TICKET_PUBKEY_BYTES` | `spice-proto/header.rs`, `caps.rs` |
| `spice/enums.h` | Channel types, message numbers, image/codec/surface enums. These are generated from `spice.proto`, so cross-check them against `spice-common/spice.proto`. | `spice-proto/enums.rs` |
| `spice/vd_agent.h` | `VDAgentMessage`, `VDAgentMonitorsConfig`, clipboard, mouse state, file-transfer and `VD_AGENT_CAP_*` | `spice-proto/agent.rs` |
| `spice/error_codes.h` | Link error codes | `spice-proto/enums.rs` |
| `spice/stream-device.h`, `qxl_dev.h`, `qxl_windows.h`, `vdi_dev.h`, `controller_prot.h`, `foreign_menu_prot.h`, `ipc_ring.h`, `stats.h` | Server/guest-side only, not needed by a client | — |

**Version mismatch (fix before milestone 1).** spice-gtk requires
spice-protocol **0.14.5** (`spice-gtk/meson.build:38`), but the checkout is at
`v0.12.13-19-g33b5255`, a May 2018 commit. That checkout is missing
definitions spice-gtk uses, including:

- `SPICE_DISPLAY_CAP_CODEC_H265` and `SPICE_VIDEO_CODEC_TYPE_H265`
- `SPICE_DISPLAY_CAP_GL_SCANOUT2`
- `VD_AGENT_CAP_CLIPBOARD_GRAB_SERIAL` and `VD_AGENT_CAP_CLIPBOARD_NO_RELEASE_ON_REGRAB`. These matter for v1, because the clipboard code in `channel-main.c` and `spice-gtk-session.c` depends on them to avoid clipboard grab races.
- `VD_AGENT_CAP_GRAPHICS_DEVICE_INFO`

**Action:** check out the matching tag:
`git -C spice-protocol fetch --tags && git -C spice-protocol checkout v0.14.5`.
The local clone only has tags up to v0.14.4, hence the fetch. Consider making
it a submodule alongside spice-gtk so the pinned version is recorded.

Porting rules:
- Copy constant values and struct layouts into Rust by hand. Each Rust item
  gets a comment naming the C identifier, e.g. `// SPICE_MAIN_CAP_AGENT_CONNECTED_TOKENS`,
  so a grep finds both.
- All wire structs are packed little-endian (`start-packed.h`). Encode and
  decode them field by field with the `Reader` and `BytesMut`, never by
  transmuting `#[repr(C, packed)]` structs.
- Add a unit test per struct that asserts its encoded size matches the C
  `sizeof`. For example, `SpiceLinkHeader` is 16 bytes and
  `SpiceMiniDataHeader` is 6.
- Nothing in the build reads `spice-protocol/`. It is reference only.

---

## 3. Architecture

```
┌─────────────────────────── main thread ──────────────────────────────┐
│ spiceport (bin)                                                      │
│  CLI (clap) → logging (colog) → SDL init → event loop                │
│  • window / renderer / streaming texture per display                 │
│  • keyboard → XT scancodes, mouse → server/client mode               │
│  • cursor sprites, grab handling, fullscreen                         │
└───────▲────────────── UiEvent (mpsc + SDL user event wakeup) ────────┘
        │                         │ ClientCommand (tokio mpsc)
┌───────┴─────────────── tokio runtime (worker threads) ───────────────┐
│ spice-client                                                         │
│  Session ── owns connection params, session id, channel registry     │
│   ├─ MainChannel task      (agent, mouse mode, channel list)         │
│   ├─ DisplayChannel task   → spice-canvas → shared framebuffer       │
│   ├─ CursorChannel task                                              │
│   ├─ InputsChannel task                                              │
│   ├─ PlaybackChannel task  → AudioSink trait (SDL impl in bin)       │
│   └─ RecordChannel task    ← AudioSource trait                       │
│  each task: Transport (TCP | TLS | Unix) → Link → Auth → msg loop    │
└──────────────────────────────────────────────────────────────────────┘
```

Key decisions:

1. **SDL stays on the main thread.** macOS requires it, and SDL3 says video and
   events belong on the main thread. The network runs on a tokio runtime
   started on a background thread before the SDL loop.
2. **One tokio task per channel.** This replaces spice-gtk's coroutines
   (`coroutine_*.c`, `gio-coroutine.c`). Each task splits its stream into a
   read half and a write half. Outgoing messages go through an mpsc queue to a
   writer task, so sends never block the reader.
3. **Client to UI:** a `std::sync::mpsc` (or crossbeam) queue of `UiEvent`s,
   plus an SDL user event pushed after each send so the blocking
   `wait_event` loop wakes up. UI to client: `tokio::sync::mpsc` of
   `ClientCommand` (key, mouse, resize, clipboard, quit).
4. **Framebuffer:** the display channel decodes into the primary surface
   (`Vec<u32>`, xRGB8888) inside an `Arc<Mutex<Surface>>`. It keeps a dirty-rect
   union and sends `UiEvent::Invalidate`. The UI locks the surface, copies the
   dirty region into an `SDL_PIXELFORMAT_XRGB8888` streaming texture, then
   unlocks. The lock is held only for that copy. Several pending invalidates
   collapse into one upload per frame.
5. **Audio needs no main-thread hop.** SDL3 `SDL_AudioStream` is thread-safe,
   so the playback task pushes decoded PCM directly. SDL also resamples, which
   covers 44.1 kHz vs 48 kHz. Wrap the stream in a `Send` newtype and keep
   that `unsafe` in one place.
6. **The client library has no SDL dependency.** `spice-client` exposes traits
   and events. This keeps it testable headless (a screenshot tool, like
   `tools/spicy-screenshot.c`, becomes trivial) and mirrors the
   spice-client-glib / spice-client-gtk split.

---

## 4. Workspace layout

```
spiceport/
├── Cargo.toml                 # [workspace]
├── crates/
│   ├── spice-proto/           # wire types; no I/O, no async
│   │   ├── enums.rs           # from spice.proto + protocol.h
│   │   ├── header.rs          # link header/mess/reply, mini & full data header
│   │   ├── reader.rs          # bounds-checked LE reader with @ptr offset support
│   │   ├── msg/{base,main,display,inputs,cursor,playback,record}.rs
│   │   └── agent.rs           # VDAgentMessage, monitors config, clipboard
│   ├── spice-decode/          # pure decoders, #![forbid(unsafe_code)], fuzzable
│   │   ├── quic.rs  lz.rs  glz.rs  lz4.rs  zlib.rs  jpeg.rs  mjpeg.rs
│   ├── spice-canvas/          # surfaces, clipping, raster ops, image/palette caches
│   └── spice-client/          # tokio: transport, link, auth, session, channels
├── spiceport/                 # the binary: CLI, logging, SDL frontend
│   ├── src/main.rs  cli.rs  logging.rs  ui/{window,input,cursor,audio}.rs
│   └── src/keymap_generated.rs   # USB HID → AT set 1, from keycodemapdb
├── xtask/                     # `cargo xtask gen-keymap`, release packaging
├── spice-gtk/                 # reference: C client (+ spice-common, keycodemapdb submodules)
└── spice-protocol/            # reference: protocol and agent headers (pin to v0.14.5)
```

`spice-gtk/` and `spice-protocol/` stay in the repo as reference material.
Nothing in the build depends on them. Generated files are checked in. Exclude
both directories from the Cargo workspace, and from editor indexing if it gets
noisy.

---

## 5. Dependencies (all cross-platform)

| Purpose | Crate | Why / cross-platform notes |
|---|---|---|
| Logging facade | `log` 0.4 | Every crate logs through this. |
| Logging backend | **`colog`** 1.4 | Coloured formatter on top of `env_logger` 0.11. `colog::default_builder()` returns an `env_logger::Builder`, so `RUST_LOG` filtering works too. Licence is LGPL-3.0. |
| CLI | `clap` 4 (derive) | |
| Async runtime | `tokio` 1 (`rt-multi-thread`, `net`, `io-util`, `sync`, `time`, `macros`) | `UnixStream` behind `cfg(unix)` |
| TLS | `rustls` 0.23 + `tokio-rustls` 0.26 + `rustls-pemfile` + `rustls-native-certs` | Pure Rust, so no OpenSSL to install on Windows or macOS. Use the `ring` provider (avoids aws-lc's cmake/nasm needs on Windows). A custom `ServerCertVerifier` implements `--host-subject` (see `ssl_verify.c`). |
| Ticket auth | `rsa` 0.9 + `sha1` + `spki` | RSA-OAEP(SHA-1) over the 1024-bit SPKI key in `SpiceLinkReply`; the password includes its NUL terminator. |
| Window, render, input, audio | **`sdl3`** 0.20 with `build-from-source-static` | Builds SDL 3 from source and links it statically, so the binary needs no system SDL. Needs cmake and a C compiler at build time (document in README and devcontainer). |
| JPEG / MJPEG | `zune-jpeg` | Pure Rust and fast. |
| LZ4 | `lz4_flex` (`safe-decode`) | SPICE uses LZ4 *streaming* blocks where each block's dictionary is the previous output (`canvas_base.c:canvas_get_lz4`); handle this with `block::decompress_into_with_dict`. |
| zlib (ZLIB_GLZ_RGB) | `flate2` (`rust_backend` / miniz_oxide) | Pure Rust. |
| Opus | `opus` 0.4 (→ `opusic-sys`, bundled libopus) | Behind a default cargo feature `opus`. Without it we simply don't advertise `PLAYBACK_CAP_OPUS` / `RECORD_CAP_OPUS` and the server sends raw PCM. |
| H.264 | `ffmpeg-next` 9 (system libavcodec) | Behind feature `h264`, off by default. Not openh264: spice-server feeds x264 RGB, so its streams are High 4:4:4 Predictive, which openh264 (and the pure-Rust decoders) can't decode. YUV to xRGB is done in Rust, so no libswscale. |
| Errors | `thiserror` (libs), `anyhow` (bin) | |
| Bytes | `bytes` | Zero-copy message slices. |
| Password prompt | `rpassword` | Cross-platform terminal prompt when the server rejects an empty password. |
| Tests | `proptest`, `cargo-fuzz` (Linux CI only) | |

Avoided on purpose: `pixman` bindings, `openssl`, `gstreamer`, anything GTK or
GLib.

**Toolchain additions for the devcontainer:** `cmake`, `pkg-config`, `g++`
(for SDL and libopus source builds), plus the FFmpeg development files and
`libclang-dev` for the `h264` feature. CI matrix:
`ubuntu-latest`, `windows-latest` (MSVC), `macos-latest` (arm64), plus a
cross-compile check for `x86_64-pc-windows-gnu`.

---

## 6. Module-by-module porting map

### 6.1 `spice-proto` (from `spice-common/spice.proto`, `spice-gtk/src/spice-channel.c`, `spice-protocol/spice/{protocol,enums,vd_agent}.h`)

- **Message definitions:** write the demarshallers by hand from
  `spice.proto`, covering only the messages listed in section 1 (about 70).
  The C code generates them with `spice_codegen.py`. A Rust backend for that
  generator is possible but not worth it for this subset. Revisit if the
  display messages turn out too repetitive.
- **`@ptr32` fields:** many display messages hold 32-bit offsets *from the
  start of the message body* to images, palettes, clip rects, masks and
  brushes. `Reader` must support `at_offset(u32) -> Reader` with bounds
  checks. This is the most error-prone part of the protocol, so it gets
  proptest and fuzz coverage.
- **Chunked data:** `SpiceChunks` with multiple chunks is linearised
  (`spice_chunks_linearize`).
- **Headers:** always advertise `COMMON_CAP_MINI_HEADER` (6 bytes: `u16 type`,
  `u32 size`). Also support the 18-byte full header with `sub_list` for old
  servers. Sub-messages are rare but cheap to support.
- **Marshalling:** client messages (`SPICE_MSGC_*`) are small and fixed, so
  hand-write them as `fn encode(&self, buf: &mut BytesMut)`.

### 6.2 `spice-client`

| Rust module | Ports | Notes |
|---|---|---|
| `uri.rs` | `spice_parse_uri` | Must match the C behaviour exactly: `user@`, `[ipv6]:port`, query keys `port`/`tls-port`/`password`, `;` or `&` separators, error on unknown or duplicated keys, `spice+tls://` rejecting `port`. Port the C tests from `tests/session.c` as unit tests. Redact the password whenever the URI is logged. |
| `transport.rs` | `channel_connect`, `bio-gio.c` | `enum Transport { Tcp, Tls, Unix }` implementing `AsyncRead + AsyncWrite`. Choose the plain or TLS port per channel type, following `--secure-channels` and the server's `SPICE_LINK_ERR_NEED_SECURED` retry. |
| `link.rs` | `spice_channel_send_link`, `recv_link_hdr`, `recv_link_msg`, `send_spice_ticket`, `recv_auth` | State machine: send `SpiceLinkHeader` + `SpiceLinkMess`, read the reply, choose an auth method (`PROTOCOL_AUTH_SELECTION`), RSA-encrypt the ticket, read the `u32` result. Map `link_err` to typed errors. |
| `channel.rs` | `spice-channel.c`, `channel-base.c` | Generic message loop: read header, read body, dispatch to the channel handler, else to base handling (ping, set_ack/ack_sync, notify, disconnecting). Implement ACK flow control (`SPICE_MSGC_ACK` every `ack_window` messages). |
| `session.rs` | `spice-session.c` | Holds host, ports, password, TLS config, `connection_id`, the channel registry, mm-time clock and the shared GLZ window. Opens the main channel; on `CHANNELS_LIST` it spawns the supported channels and ignores the rest with an `info!`. |
| `main_ch.rs` | `channel-main.c` | `MAIN_INIT`, then `ATTACH_CHANNELS`. Mouse mode: request client mode when the agent connects. Agent: token accounting, chunking of `VDAgentMessage` (max 2048 bytes per `AGENT_DATA`), capability announce, monitors config (debounced on window resize, as `spice_main_channel_update_display` does), clipboard grab/request/data for UTF-8 text with CRLF/LF conversion per guest capability. |
| `display_ch.rs` | `channel-display.c`, `channel-display-mjpeg.c` | Advertises caps: `SIZED_STREAM`, `STREAM_REPORT`, `MULTI_CODEC`, `CODEC_MJPEG` (+ `CODEC_H264` with that feature), `MONITORS_CONFIG`, `COMPOSITE`, `A8_SURFACE`, `LZ4_COMPRESSION`, `PREF_COMPRESSION`, `PREF_VIDEO_CODEC_TYPE`. Sends `DISPLAY_INIT` with cache sizes. Drives `spice-canvas`. Streams: create/clip/data/destroy, decode and blit into the surface, honour mm-time (drop late frames, delay early ones), send `STREAM_REPORT`. |
| `cursor_ch.rs` | `channel-cursor.c` | Cursor cache; converts mono, 4/8 bpp and alpha cursors to RGBA. Emits `CursorSet{rgba,w,h,hot}`, `CursorMove`, `CursorHide`. |
| `inputs_ch.rs` | `channel-inputs.c` | Key down/up with spice's scancode encoding (`0x100` bit becomes an `0xE0` prefix, see `spice_make_scancode`); `KEY_SCANCODE` batch when the cap is present; modifiers sync; motion (relative) vs position (absolute plus display id); button masks; motion throttling with `SPICE_INPUT_MOTION_ACK_BUNCH` (4). |
| `playback_ch.rs` / `record_ch.rs` | `channel-playback.c`, `channel-record.c`, `snd_codec.c` | Mode, start, data, stop, volume, mute, latency. `AudioSink`/`AudioSource` traits are implemented by the binary. |
| `agent.rs` | parts of `channel-main.c` | Separated out so it can be unit tested against captured byte streams. |

### 6.3 `spice-decode` (the hardest code to port; port it carefully and test against C)

| Format | C source | Approx C lines | Notes |
|---|---|---|---|
| QUIC | `common/quic.c`, `quic_tmpl.c`, `quic_family_tmpl.c` | 2,100 | Adaptive Golomb coding with model state. C macro templates become generic functions or `macro_rules!`. Covers RGB32/RGB24/RGB16/RGBA/GRAY. |
| LZ | `common/lz.c`, `lz_decompress_tmpl.c` | 1,100 | LZ_RGB 16/24/32, RGBA, LZ_PLT 1/4/8 bpp with palettes. |
| GLZ | `src/decode-glz.c`, `decode-glz-tmpl.c` | 780 | References pixels in *earlier images* by id, so it needs a window shared by every display channel in the session and must wait for missing images (`glz_decoder_window` blocks in C; here, an async wait). |
| ZLIB_GLZ_RGB | `src/decode-zlib.c` | small | Inflate, then GLZ. |
| LZ4 | `canvas_base.c:canvas_get_lz4` | 90 | Streaming blocks, see section 5. |
| JPEG / JPEG_ALPHA | `src/decode-jpeg.c`, `canvas_base.c` | 300 | JPEG_ALPHA = JPEG RGB + LZ-compressed alpha plane. |
| BITMAP | `canvas_base.c` | | Uncompressed, palettes, top-down flag. |

Each decoder is `fn decode(input: &[u8], ...) -> Result<Image, DecodeError>`,
never panics on bad input, and is fuzzed.

**Golden tests:** build a small C harness under `tests/c-oracle/` that links
the original `quic.c`/`lz.c` (they have no GLib dependency beyond `mem.c`
shims). Use it to encode random and real images and to decode captured
streams, then compare the Rust output byte-for-byte. Linux CI only.

### 6.4 `spice-canvas` (replaces `canvas_base.c` + `sw_canvas.c` + pixman)

- `Surface { w, h, stride, fmt, data: Vec<u32 | u8> }` for formats
  `32_xRGB`, `32_ARGB`, `16_555`, `16_565`, `8_A`, `1_A`. Internally, convert
  16 bpp to 32 bpp on decode.
- `Clip`: none, or a rect list (`SpiceClipRects`). Implement it as a
  `Vec<Rect>` intersection iterator; no general region algebra is needed.
- Caches: image cache keyed by `u64` id (`CACHE_ME`, `FROM_CACHE`,
  `FROM_CACHE_LOSSLESS`, `CACHE_REPLACE_ME`, `INVAL_*`), palette cache, and
  surface references (`SPICE_IMAGE_TYPE_SURFACE`).
- Draw ops, in priority order. Real-world QXL/virtio guests mostly send the
  first group.
  1. `DRAW_COPY` (with nearest/bilinear scaling and the `SRC_COPY` rop
     fast path), `DRAW_FILL` (solid brush), `COPY_BITS`, `DRAW_ALPHA_BLEND`,
     `DRAW_COMPOSITE`, `DRAW_BLACKNESS/WHITENESS/INVERS`.
  2. `DRAW_OPAQUE`, `DRAW_BLEND`, `DRAW_TRANSPARENT`, pattern brushes,
     masks, the 16 ROPD rops.
  3. `DRAW_ROP3` (port `common/rop3.c`), `DRAW_STROKE` (port the needed
     subset of `common/lines.c`, mostly thin solid lines), `DRAW_TEXT`
     (glyph rasters A1/A4/A8).
- Unimplemented ops log `warn!` **once per op type** and are skipped. This
  keeps the client usable while coverage grows.

### 6.5 `spiceport` binary (replaces `spice-widget.c`, `spice-gtk-session.c`, `tools/spicy.c`)

- **Window:** one SDL window per display channel, resizable, titled with
  `MAIN_NAME` or the host. Use `SDL_SetRenderLogicalPresentation` with
  letterbox scaling when the guest can't follow the window size. When the
  agent is present, resizing the window sends monitors config so the guest
  resizes instead. HiDPI comes from `SDL_GetWindowPixelDensity`.
- **Keyboard:** SDL scancodes are USB HID usages, mapped through a table
  generated from `keycodemapdb/data/keymaps.csv` (columns "USB Keycodes" to
  "AT set1 keycode"), which gives the same scancodes on every OS. Track
  pressed keys and release them all when focus is lost (as
  `release_keys` in `spice-widget.c:1678`). Sync lock-key modifiers on focus.
  Keyboard grab uses `SDL_SetWindowKeyboardGrab`, so Alt-Tab and Win go to the
  guest.
- **Mouse:**
  - Client mode (agent present): absolute `MOUSE_POSITION` scaled to guest
    coordinates. The local cursor is shown with the guest's cursor shape.
  - Server mode: click to grab, `SDL_SetWindowRelativeMouseMode`, relative
    `MOUSE_MOTION`. Draw the guest cursor sprite ourselves at the position
    from `CURSOR_MOVE`.
  - Release combo: Ctrl+Alt by default (configurable, like
    `spice-grabsequence.c`). The window title says how to release.
- **Cursor:** `SDL_CreateColorCursor` from RGBA plus hotspot, cached by
  cursor id.
- **Audio:** `SDL_OpenAudioDeviceStream` for playback, using the format from
  `PLAYBACK_START`. Apply volume and mute via `SDL_SetAudioStreamGain`.
  Recording uses an SDL capture stream: 20 ms frames go to the record channel,
  Opus-encoded when negotiated. Recording only opens on `RECORD_START`, so the
  OS microphone permission prompt appears only when the guest asks.
- **Clipboard:** SDL clipboard text, polled through
  `SDL_EVENT_CLIPBOARD_UPDATE`, then a clipboard grab to the agent.
- **Shortcuts:** F11 toggles fullscreen; Ctrl+Alt+Del, sent via a key combo
  or later a small overlay; Ctrl+Alt+Q quits.

---

## 7. CLI

```
spiceport [OPTIONS] <URL>

  <URL>                    spice://host[:port][?port=N&tls-port=M&password=P]
                           spice+tls://host:port
                           spice+unix:///path/to/socket       (Unix only)

  --ca-file <PEM>          CA bundle for TLS (default: system roots)
  --host-subject <SUBJ>    Expected cert subject, e.g. "C=GB,O=Example,CN=host"
  --secure-channels <LIST> Channels that must use TLS (main,display,inputs,... or all)
  --password-stdin         Read password from stdin (avoid putting it in the URL)
  -f, --fullscreen
  --no-audio / --no-record
  --no-agent-resize        Don't resize the guest to the window
  --preferred-compression <auto-glz|auto-lz|quic|glz|lz|lz4|off>
  --release-keys <COMBO>   Default "ctrl+alt"
  -v, -vv, -vvv            Raise log level (warn → info → debug → trace)
  -q                       Errors only
  --log-file <PATH>        Also write logs (without colour) to a file
```

The password can come from the URL, `--password-stdin`, or the
`SPICE_PASSWORD` env var. If the server answers `PERMISSION_DENIED` and the
terminal is interactive, prompt with `rpassword` and retry once. Warn when the
password is in the URL, as the C code does (`spice-session.c:551`).

Exit codes: 0 for a clean disconnect or user quit, 1 for a connection or auth
failure (message on stderr), 2 for usage errors (from clap).

---

## 8. Logging

All crates log through the `log` macros. Only the binary configures the
backend.

```rust
// spiceport/src/logging.rs
pub fn init(verbosity: i8, log_file: Option<&Path>) {
    let default = match verbosity { ..=-1 => "error", 0 => "warn", 1 => "info", 2 => "debug", _ => "trace" };
    let mut b = colog::default_builder();         // env_logger::Builder with colog formatting
    b.filter_level(LevelFilter::Off)
     .parse_filters(&format!("spiceport={default},spice_client={default},spice_canvas={default},spice_decode={default},spice_proto={default}"))
     .parse_env("RUST_LOG");                     // RUST_LOG overrides, e.g. RUST_LOG=spice_client::display_ch=trace
    // --log-file: wrap stderr+file in a tee writer via Target::Pipe, with colour stripped for the file
    b.init();
}
```

Conventions:

- **Levels.** `error!` is for fatal problems for the session or a channel.
  `warn!` is for recoverable protocol oddities, unsupported features (once
  each) and TLS warnings. `info!` covers connection lifecycle: connecting,
  TLS established, channel up, agent connected, mouse mode changed, guest
  resized. `debug!` is per-message summaries and state transitions. `trace!`
  is hex dumps and per-draw-op details.
- **Targets follow module paths**, so `RUST_LOG=spice_client::inputs_ch=debug`
  works without extra setup.
- **Channel context:** a `chan_debug!(self, ...)`-style macro set prefixes
  `"{type}-{id}:"` (e.g. `display-0:`), like spice-gtk's `CHANNEL_DEBUG`.
- **Cheap when disabled:** wrap hex dumps and expensive formatting in
  `if log_enabled!(Level::Trace)`.
- **Secrets:** never log passwords or tickets. `SpiceUri`'s `Display` and
  `Debug` impls redact the password.
- **Warn once:** a `warn_once!` macro (backed by a `static Once` per call
  site) for unimplemented draw ops and unknown messages.
- **Windows:** keep the console subsystem in v1 so the logs show up. Enable
  ANSI handling for older consoles with
  `colored::control::set_virtual_terminal(true)` under `cfg(windows)`.
  Later, use `windows_subsystem = "windows"` in release builds plus
  `AttachConsole(ATTACH_PARENT_PROCESS)` so launching from a terminal still
  shows logs.
- **Colour** turns off when stderr is not a TTY and honours `NO_COLOR`.
  Check that colog/`colored` actually do this, and force it off in the
  `--log-file` path if they don't.

---

## 9. Milestones

Each milestone ends in something runnable against a real server. The test
server is QEMU with `-spice port=5900,disable-ticketing=on -device qxl-vga`
(plus a TLS variant with a generated CA). Add a `docker/` or `scripts/`
recipe to launch it.

| # | Milestone | Done when |
|---|---|---|
| 0 | **Scaffolding.** Workspace, CI matrix (3 OSes), devcontainer gets cmake, `colog` logging, clap CLI, `spice-protocol/` checked out at v0.14.5. | `spiceport --help` works on all three OSes in CI. |
| 1 | **Protocol core.** URI parser (with ported tests), `spice-proto` headers and link, TCP transport, ticket auth, main channel up to `CHANNELS_LIST`, ping/ack. | `spiceport -vv spice://localhost:5900` logs the channel list and stays connected. |
| 2 | **First pixels.** Display channel with surfaces, `DRAW_FILL`, `DRAW_COPY` from BITMAP/LZ4/JPEG, `COPY_BITS`, image cache. SDL window and texture upload. | Guest BIOS/boot screen and console are visible and update. |
| 3 | **Input.** Inputs channel, keymap generation, server-mode mouse with grab and release, cursor channel. | You can log in at a text console and use a desktop with the mouse. |
| 4 | **Full decoders.** QUIC, LZ, GLZ (+ shared window), ZLIB_GLZ, JPEG_ALPHA. C-oracle golden tests and fuzz targets. | A Linux and a Windows guest desktop render correctly under every `--preferred-compression`. |
| 5 | **Agent.** Client mouse mode, monitors config on resize, text clipboard both ways. | With spice-vdagent in the guest, the window resize resizes the guest and copy/paste works. |
| 6 | **Audio.** Playback raw + Opus, record raw + Opus, volume and mute. | Video in the guest plays sound in sync to the human eye; the microphone reaches the guest. |
| 7 | **Video streams and remaining draw ops.** MJPEG streams with mm-time sync and stream reports; draw ops groups 2 and 3. | Video playback in the guest is smooth; no `warn_once` for unimplemented ops during a normal Windows session. |
| 8 | **TLS.** rustls transport, `--ca-file`, `--host-subject`, `--secure-channels`, `NEED_SECURED` retry. | `spice+tls://` works against QEMU with a self-signed CA on all three OSes. |
| 9 | **Release.** Static SDL builds; signed/notarised macOS `.app` + bare binary; Windows `.exe` (+ optional MSI); Linux tarball/AppImage. README. | Downloadable artifacts from a tagged CI release that run on clean VMs. |
| 10+ | **Later phases** from section 1: H.264, file transfer, multi-monitor, migration, SASL, proxy. | |

Milestones 4 and 5 can run in parallel with 3 if more than one person works on
this. TLS (8) can move earlier if the target servers need it.

---

## 10. Testing strategy

- **Unit:** the URI parser (port the cases from `spice-gtk/tests/session.c`),
  header and link encode/decode round-trips, scancode mapping, agent
  chunking and token accounting.
- **Golden:** decoders against the C oracle (section 6.3). Canvas ops against
  PNG fixtures rendered by the C `sw_canvas` via the same oracle harness.
- **Fuzz:** `cargo fuzz` targets for every decoder and for
  `spice-proto` demarshalling of each server message type.
- **Replay:** add a `--record-session <file>` debug option that writes raw
  incoming channel bytes. A test harness feeds those recordings into the
  channel state machines without a network and snapshots the final
  framebuffer.
- **Integration (Linux CI):** start QEMU (or `spice-server`'s test display
  server) in CI, connect headless (the client crate with no SDL), and assert
  that channels come up and a frame arrives.

---

## 11. Risks and open questions

1. **Licensing (decision needed).** spice-gtk and spice-common are
   LGPL-2.1-or-later. A line-by-line port of `quic.c`, `lz*.c`,
   `decode-glz.c`, `canvas_base.c` and so on is a derivative work, so
   spiceport should be LGPL-2.1-or-later (or a compatible licence). `colog`
   is LGPL-3.0, which is compatible with that when linked statically,
   provided relinking is possible (source distribution covers that).
   keycodemapdb's generated table may be used under BSD-3.
2. **SDL3 Rust bindings maturity.** `sdl3` 0.20 is pre-1.0, so expect API
   churn. Use `sdl3-sys` directly in the few places the safe wrapper lacks
   (audio stream gain, `SDL_AudioStream` across threads, user events).
   Fallback: `sdl2` 0.38 with `bundled`. The frontend sits behind our own
   small trait boundary, so swapping is contained.
3. **Build-time C dependencies:** SDL (cmake) and libopus (cmake) are
   bundled, so builders need cmake and a C compiler. H.264 uses the system
   FFmpeg instead (linked dynamically; libclang for bindgen). Document it,
   and keep `opus` and `h264` as features so a minimal build needs only SDL.
4. **GLZ cross-image dependencies** make decoding order-sensitive across
   display channels. Get this right with a session-wide window and an async
   "wait for image id" with a timeout. Log `warn!` and drop the draw on
   timeout instead of deadlocking.
5. **Canvas fidelity** for rarer ops (rop3, strokes, text) is a long tail.
   Mitigated by warn-once and by prioritising by what real guests send. Use
   the replay recordings from section 10 to find out what they send.
6. **macOS specifics:** keyboard grab needs Accessibility permission for
   system shortcuts, the microphone needs `NSMicrophoneUsageDescription` in
   the `.app` bundle, and Cmd should map to the guest's Win/Super key.
7. **Windows specifics:** no `spice+unix://` (reject it with a clear error),
   set DPI awareness via SDL hints, and Win-key/Alt-Tab capture needs
   keyboard grab (SDL installs a low-level hook).

---

## 12. First concrete steps

1. Check out `spice-protocol/` at v0.14.5 (section 2), and add
   `cmake`/`pkg-config`/`g++` to `.devcontainer/Dockerfile`.
2. `cargo new --vcs none` the workspace crates from section 4. Set up
   `colog` logging and clap in the binary, and the CI matrix.
3. Port `spice_parse_uri` and its tests. It is the first thing the CLI touches
   and has exact, testable behaviour.
4. Implement `spice-proto` link structs plus `spice-client` link/auth over TCP,
   and get milestone 1 working against a local QEMU.
