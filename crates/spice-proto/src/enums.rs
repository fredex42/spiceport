//! Constants from `spice/enums.h` (generated from spice-common `spice.proto`)
//! and `spice/protocol.h`. Each block names its C prefix.

/// `SPICE_MAGIC` ("REDQ" as a little-endian u32).
pub const SPICE_MAGIC: u32 = u32::from_le_bytes(*b"REDQ");
/// `SPICE_VERSION_MAJOR`
pub const SPICE_VERSION_MAJOR: u32 = 2;
/// `SPICE_VERSION_MINOR`
pub const SPICE_VERSION_MINOR: u32 = 2;
/// `SPICE_MAX_PASSWORD_LENGTH`
pub const SPICE_MAX_PASSWORD_LENGTH: usize = 60;
/// `SPICE_TICKET_KEY_PAIR_LENGTH` (bits)
pub const SPICE_TICKET_KEY_PAIR_LENGTH: usize = 1024;
/// `SPICE_TICKET_PUBKEY_BYTES`
pub const SPICE_TICKET_PUBKEY_BYTES: usize = SPICE_TICKET_KEY_PAIR_LENGTH / 8 + 34;
/// `SPICE_MAX_NUM_STREAMS`
pub const SPICE_MAX_NUM_STREAMS: u32 = 64;
/// `SPICE_INPUT_MOTION_ACK_BUNCH`
pub const SPICE_INPUT_MOTION_ACK_BUNCH: u32 = 4;

/// `SPICE_CHANNEL_*`
pub mod channel_type {
    pub const MAIN: u8 = 1; // SPICE_CHANNEL_MAIN
    pub const DISPLAY: u8 = 2; // SPICE_CHANNEL_DISPLAY
    pub const INPUTS: u8 = 3; // SPICE_CHANNEL_INPUTS
    pub const CURSOR: u8 = 4; // SPICE_CHANNEL_CURSOR
    pub const PLAYBACK: u8 = 5; // SPICE_CHANNEL_PLAYBACK
    pub const RECORD: u8 = 6; // SPICE_CHANNEL_RECORD
    pub const TUNNEL: u8 = 7; // SPICE_CHANNEL_TUNNEL
    pub const SMARTCARD: u8 = 8; // SPICE_CHANNEL_SMARTCARD
    pub const USBREDIR: u8 = 9; // SPICE_CHANNEL_USBREDIR
    pub const PORT: u8 = 10; // SPICE_CHANNEL_PORT
    pub const WEBDAV: u8 = 11; // SPICE_CHANNEL_WEBDAV

    /// `spice_channel_type_to_string`
    pub fn name(t: u8) -> &'static str {
        match t {
            MAIN => "main",
            DISPLAY => "display",
            INPUTS => "inputs",
            CURSOR => "cursor",
            PLAYBACK => "playback",
            RECORD => "record",
            TUNNEL => "tunnel",
            SMARTCARD => "smartcard",
            USBREDIR => "usbredir",
            PORT => "port",
            WEBDAV => "webdav",
            _ => "unknown",
        }
    }

    /// `spice_channel_string_to_type`
    pub fn from_name(s: &str) -> Option<u8> {
        (MAIN..=WEBDAV).find(|&t| name(t) == s)
    }
}

/// `SpiceLinkErr` (`SPICE_LINK_ERR_*`)
pub mod link_err {
    pub const OK: u32 = 0;
    pub const ERROR: u32 = 1;
    pub const INVALID_MAGIC: u32 = 2;
    pub const INVALID_DATA: u32 = 3;
    pub const VERSION_MISMATCH: u32 = 4;
    pub const NEED_SECURED: u32 = 5;
    pub const NEED_UNSECURED: u32 = 6;
    pub const PERMISSION_DENIED: u32 = 7;
    pub const BAD_CONNECTION_ID: u32 = 8;
    pub const CHANNEL_NOT_AVAILABLE: u32 = 9;

    pub fn describe(e: u32) -> &'static str {
        match e {
            OK => "ok",
            ERROR => "error",
            INVALID_MAGIC => "invalid magic",
            INVALID_DATA => "invalid data",
            VERSION_MISMATCH => "version mismatch",
            NEED_SECURED => "channel requires TLS",
            NEED_UNSECURED => "channel requires a non-TLS connection",
            PERMISSION_DENIED => "permission denied",
            BAD_CONNECTION_ID => "bad connection id",
            CHANNEL_NOT_AVAILABLE => "channel not available",
            _ => "unknown link error",
        }
    }
}

/// `SpiceNotifySeverity`
pub mod notify_severity {
    pub const INFO: u32 = 0;
    pub const WARN: u32 = 1;
    pub const ERROR: u32 = 2;
}

/// `SpiceMigrateFlags`
pub mod migrate_flags {
    pub const NEED_FLUSH: u32 = 1 << 0;
    pub const NEED_DATA_TRANSFER: u32 = 1 << 1;
}

/// `SpiceMouseMode`
pub mod mouse_mode {
    pub const SERVER: u32 = 1 << 0;
    pub const CLIENT: u32 = 1 << 1;
}

/// `SpiceClipType`
pub mod clip_type {
    pub const NONE: u8 = 0;
    pub const RECTS: u8 = 1;
}

/// `SpicePathFlags`
pub mod path_flags {
    pub const BEGIN: u8 = 1 << 0;
    pub const END: u8 = 1 << 1;
    pub const CLOSE: u8 = 1 << 3;
    pub const BEZIER: u8 = 1 << 4;
}

/// `SpiceVideoCodecType`
pub mod video_codec {
    pub const MJPEG: u8 = 1;
    pub const VP8: u8 = 2;
    pub const H264: u8 = 3;
    pub const VP9: u8 = 4;
    pub const H265: u8 = 5;
}

/// `SpiceStreamFlags`
pub mod stream_flags {
    pub const TOP_DOWN: u8 = 1 << 0;
}

/// `SpiceBrushType`
pub mod brush_type {
    pub const NONE: u8 = 0;
    pub const SOLID: u8 = 1;
    pub const PATTERN: u8 = 2;
}

/// `SpiceMaskFlags`
pub mod mask_flags {
    pub const INVERS: u8 = 1 << 0;
}

/// `SpiceImageType`
pub mod image_type {
    pub const BITMAP: u8 = 0;
    pub const QUIC: u8 = 1;
    pub const RESERVED: u8 = 2;
    pub const LZ_PLT: u8 = 100;
    pub const LZ_RGB: u8 = 101;
    pub const GLZ_RGB: u8 = 102;
    pub const FROM_CACHE: u8 = 103;
    pub const SURFACE: u8 = 104;
    pub const JPEG: u8 = 105;
    pub const FROM_CACHE_LOSSLESS: u8 = 106;
    pub const ZLIB_GLZ_RGB: u8 = 107;
    pub const JPEG_ALPHA: u8 = 108;
    pub const LZ4: u8 = 109;
}

/// `SpiceImageCompression`
pub mod image_compression {
    pub const INVALID: u8 = 0;
    pub const OFF: u8 = 1;
    pub const AUTO_GLZ: u8 = 2;
    pub const AUTO_LZ: u8 = 3;
    pub const QUIC: u8 = 4;
    pub const GLZ: u8 = 5;
    pub const LZ: u8 = 6;
    pub const LZ4: u8 = 7;
}

/// `SpiceImageFlags`
pub mod image_flags {
    pub const CACHE_ME: u8 = 1 << 0;
    pub const HIGH_BITS_SET: u8 = 1 << 1;
    pub const CACHE_REPLACE_ME: u8 = 1 << 2;
}

/// `SpiceBitmapFmt`
pub mod bitmap_fmt {
    pub const INVALID: u8 = 0;
    pub const BIT1_LE: u8 = 1; // SPICE_BITMAP_FMT_1BIT_LE
    pub const BIT1_BE: u8 = 2; // SPICE_BITMAP_FMT_1BIT_BE
    pub const BIT4_LE: u8 = 3; // SPICE_BITMAP_FMT_4BIT_LE
    pub const BIT4_BE: u8 = 4; // SPICE_BITMAP_FMT_4BIT_BE
    pub const BIT8: u8 = 5; // SPICE_BITMAP_FMT_8BIT
    pub const BIT16: u8 = 6; // SPICE_BITMAP_FMT_16BIT (x555)
    pub const BIT24: u8 = 7; // SPICE_BITMAP_FMT_24BIT (bgr)
    pub const BIT32: u8 = 8; // SPICE_BITMAP_FMT_32BIT (xrgb LE)
    pub const RGBA: u8 = 9; // SPICE_BITMAP_FMT_RGBA (argb LE)
    pub const BIT8_A: u8 = 10; // SPICE_BITMAP_FMT_8BIT_A

    /// Bits per pixel (`spice_bitmap_fmt_bpp` in canvas_base.c).
    pub fn bpp(fmt: u8) -> Option<u32> {
        Some(match fmt {
            BIT1_LE | BIT1_BE => 1,
            BIT4_LE | BIT4_BE => 4,
            BIT8 | BIT8_A => 8,
            BIT16 => 16,
            BIT24 => 24,
            BIT32 | RGBA => 32,
            _ => return None,
        })
    }
}

/// `SpiceBitmapFlags`
pub mod bitmap_flags {
    pub const PAL_CACHE_ME: u8 = 1 << 0;
    pub const PAL_FROM_CACHE: u8 = 1 << 1;
    pub const TOP_DOWN: u8 = 1 << 2;
}

/// `SpiceJpegAlphaFlags`
pub mod jpeg_alpha_flags {
    pub const TOP_DOWN: u8 = 1 << 0;
}

/// `SpiceImageScaleMode`
pub mod scale_mode {
    pub const INTERPOLATE: u8 = 0;
    pub const NEAREST: u8 = 1;
}

/// `SpiceRopd`
pub mod ropd {
    pub const INVERS_SRC: u16 = 1 << 0;
    pub const INVERS_BRUSH: u16 = 1 << 1;
    pub const INVERS_DEST: u16 = 1 << 2;
    pub const OP_PUT: u16 = 1 << 3;
    pub const OP_OR: u16 = 1 << 4;
    pub const OP_AND: u16 = 1 << 5;
    pub const OP_XOR: u16 = 1 << 6;
    pub const OP_BLACKNESS: u16 = 1 << 7;
    pub const OP_WHITENESS: u16 = 1 << 8;
    pub const OP_INVERS: u16 = 1 << 9;
    pub const INVERS_RES: u16 = 1 << 10;
}

/// `SpiceLineFlags`
pub mod line_flags {
    pub const START_WITH_GAP: u8 = 1 << 2;
    pub const STYLED: u8 = 1 << 3;
}

/// `SpiceStringFlags`
pub mod string_flags {
    pub const RASTER_A1: u8 = 1 << 0;
    pub const RASTER_A4: u8 = 1 << 1;
    pub const RASTER_A8: u8 = 1 << 2;
    pub const RASTER_TOP_DOWN: u8 = 1 << 3;
}

/// `SpiceSurfaceFlags`
pub mod surface_flags {
    pub const PRIMARY: u32 = 1 << 0;
    pub const STREAMING_MODE: u32 = 1 << 1;
}

/// `SpiceSurfaceFmt`
pub mod surface_fmt {
    pub const INVALID: u32 = 0;
    pub const A1: u32 = 1; // SPICE_SURFACE_FMT_1_A
    pub const A8: u32 = 8; // SPICE_SURFACE_FMT_8_A
    pub const RGB555: u32 = 16; // SPICE_SURFACE_FMT_16_555
    pub const XRGB32: u32 = 32; // SPICE_SURFACE_FMT_32_xRGB
    pub const RGB565: u32 = 80; // SPICE_SURFACE_FMT_16_565
    pub const ARGB32: u32 = 96; // SPICE_SURFACE_FMT_32_ARGB
}

/// `SpiceAlphaFlags`
pub mod alpha_flags {
    pub const DEST_HAS_ALPHA: u8 = 1 << 0;
    pub const SRC_SURFACE_HAS_ALPHA: u8 = 1 << 1;
}

/// `SpiceCompositeFlags`
pub mod composite_flags {
    pub const OP_MASK: u32 = 0xff; // SPICE_COMPOSITE_OP0..OP7
    pub const SRC_FILTER_SHIFT: u32 = 8;
    pub const MASK_FILTER_SHIFT: u32 = 11;
    pub const SRC_REPEAT_SHIFT: u32 = 14;
    pub const MASK_REPEAT_SHIFT: u32 = 16;
    pub const COMPONENT_ALPHA: u32 = 1 << 18;
    pub const HAS_MASK: u32 = 1 << 19;
    pub const HAS_SRC_TRANSFORM: u32 = 1 << 20;
    pub const HAS_MASK_TRANSFORM: u32 = 1 << 21;
    pub const SOURCE_OPAQUE: u32 = 1 << 22;
    pub const MASK_OPAQUE: u32 = 1 << 23;
    pub const DEST_OPAQUE: u32 = 1 << 24;
}

/// `SpiceKeyboardModifierFlags`
pub mod keyboard_modifier {
    pub const SCROLL_LOCK: u16 = 1 << 0;
    pub const NUM_LOCK: u16 = 1 << 1;
    pub const CAPS_LOCK: u16 = 1 << 2;
}

/// `SpiceMouseButton`
pub mod mouse_button {
    pub const INVALID: u8 = 0;
    pub const LEFT: u8 = 1;
    pub const MIDDLE: u8 = 2;
    pub const RIGHT: u8 = 3;
    pub const UP: u8 = 4;
    pub const DOWN: u8 = 5;
    pub const SIDE: u8 = 6;
    pub const EXTRA: u8 = 7;
}

/// `SpiceMouseButtonMask`
pub mod mouse_button_mask {
    pub const LEFT: u16 = 1 << 0;
    pub const MIDDLE: u16 = 1 << 1;
    pub const RIGHT: u16 = 1 << 2;
    pub const UP: u16 = 1 << 3;
    pub const DOWN: u16 = 1 << 4;
    pub const SIDE: u16 = 1 << 5;
    pub const EXTRA: u16 = 1 << 6;
}

/// `SpiceCursorType`
pub mod cursor_type {
    pub const ALPHA: u8 = 0;
    pub const MONO: u8 = 1;
    pub const COLOR4: u8 = 2;
    pub const COLOR8: u8 = 3;
    pub const COLOR16: u8 = 4;
    pub const COLOR24: u8 = 5;
    pub const COLOR32: u8 = 6;
}

/// `SpiceCursorFlags`
pub mod cursor_flags {
    pub const NONE: u16 = 1 << 0;
    pub const CACHE_ME: u16 = 1 << 1;
    pub const FROM_CACHE: u16 = 1 << 2;
}

/// `SpiceAudioDataMode`
pub mod audio_data_mode {
    pub const INVALID: u16 = 0;
    pub const RAW: u16 = 1;
    pub const CELT_0_5_1: u16 = 2;
    pub const OPUS: u16 = 3;
}

/// `SpiceAudioFmt`
pub mod audio_fmt {
    pub const INVALID: u16 = 0;
    pub const S16: u16 = 1;
}

/// Server → client message types (`SPICE_MSG_*`).
pub mod msg {
    /// Base channel messages, valid on every channel.
    pub mod base {
        pub const MIGRATE: u16 = 1;
        pub const MIGRATE_DATA: u16 = 2;
        pub const SET_ACK: u16 = 3;
        pub const PING: u16 = 4;
        pub const WAIT_FOR_CHANNELS: u16 = 5;
        pub const DISCONNECTING: u16 = 6;
        pub const NOTIFY: u16 = 7;
        pub const LIST: u16 = 8;
        pub const BASE_LAST: u16 = 100;
    }
    /// `SPICE_MSG_MAIN_*`
    pub mod main {
        pub const MIGRATE_BEGIN: u16 = 101;
        pub const MIGRATE_CANCEL: u16 = 102;
        pub const INIT: u16 = 103;
        pub const CHANNELS_LIST: u16 = 104;
        pub const MOUSE_MODE: u16 = 105;
        pub const MULTI_MEDIA_TIME: u16 = 106;
        pub const AGENT_CONNECTED: u16 = 107;
        pub const AGENT_DISCONNECTED: u16 = 108;
        pub const AGENT_DATA: u16 = 109;
        pub const AGENT_TOKEN: u16 = 110;
        pub const MIGRATE_SWITCH_HOST: u16 = 111;
        pub const MIGRATE_END: u16 = 112;
        pub const NAME: u16 = 113;
        pub const UUID: u16 = 114;
        pub const AGENT_CONNECTED_TOKENS: u16 = 115;
        pub const MIGRATE_BEGIN_SEAMLESS: u16 = 116;
        pub const MIGRATE_DST_SEAMLESS_ACK: u16 = 117;
        pub const MIGRATE_DST_SEAMLESS_NACK: u16 = 118;
    }
    /// `SPICE_MSG_DISPLAY_*`
    pub mod display {
        pub const MODE: u16 = 101;
        pub const MARK: u16 = 102;
        pub const RESET: u16 = 103;
        pub const COPY_BITS: u16 = 104;
        pub const INVAL_LIST: u16 = 105;
        pub const INVAL_ALL_PIXMAPS: u16 = 106;
        pub const INVAL_PALETTE: u16 = 107;
        pub const INVAL_ALL_PALETTES: u16 = 108;
        pub const STREAM_CREATE: u16 = 122;
        pub const STREAM_DATA: u16 = 123;
        pub const STREAM_CLIP: u16 = 124;
        pub const STREAM_DESTROY: u16 = 125;
        pub const STREAM_DESTROY_ALL: u16 = 126;
        pub const DRAW_FILL: u16 = 302;
        pub const DRAW_OPAQUE: u16 = 303;
        pub const DRAW_COPY: u16 = 304;
        pub const DRAW_BLEND: u16 = 305;
        pub const DRAW_BLACKNESS: u16 = 306;
        pub const DRAW_WHITENESS: u16 = 307;
        pub const DRAW_INVERS: u16 = 308;
        pub const DRAW_ROP3: u16 = 309;
        pub const DRAW_STROKE: u16 = 310;
        pub const DRAW_TEXT: u16 = 311;
        pub const DRAW_TRANSPARENT: u16 = 312;
        pub const DRAW_ALPHA_BLEND: u16 = 313;
        pub const SURFACE_CREATE: u16 = 314;
        pub const SURFACE_DESTROY: u16 = 315;
        pub const STREAM_DATA_SIZED: u16 = 316;
        pub const MONITORS_CONFIG: u16 = 317;
        pub const DRAW_COMPOSITE: u16 = 318;
        pub const STREAM_ACTIVATE_REPORT: u16 = 319;
        pub const GL_SCANOUT_UNIX: u16 = 320;
        pub const GL_DRAW: u16 = 321;
        pub const QUALITY_INDICATOR: u16 = 322;
        pub const GL_SCANOUT2_UNIX: u16 = 323;
    }
    /// `SPICE_MSG_INPUTS_*`
    pub mod inputs {
        pub const INIT: u16 = 101;
        pub const KEY_MODIFIERS: u16 = 102;
        pub const MOUSE_MOTION_ACK: u16 = 111;
    }
    /// `SPICE_MSG_CURSOR_*`
    pub mod cursor {
        pub const INIT: u16 = 101;
        pub const RESET: u16 = 102;
        pub const SET: u16 = 103;
        pub const MOVE: u16 = 104;
        pub const HIDE: u16 = 105;
        pub const TRAIL: u16 = 106;
        pub const INVAL_ONE: u16 = 107;
        pub const INVAL_ALL: u16 = 108;
    }
    /// `SPICE_MSG_PLAYBACK_*`
    pub mod playback {
        pub const DATA: u16 = 101;
        pub const MODE: u16 = 102;
        pub const START: u16 = 103;
        pub const STOP: u16 = 104;
        pub const VOLUME: u16 = 105;
        pub const MUTE: u16 = 106;
        pub const LATENCY: u16 = 107;
    }
    /// `SPICE_MSG_RECORD_*`
    pub mod record {
        pub const START: u16 = 101;
        pub const STOP: u16 = 102;
        pub const VOLUME: u16 = 103;
        pub const MUTE: u16 = 104;
    }
}

/// Client → server message types (`SPICE_MSGC_*`).
pub mod msgc {
    pub mod base {
        pub const ACK_SYNC: u16 = 1;
        pub const ACK: u16 = 2;
        pub const PONG: u16 = 3;
        pub const MIGRATE_FLUSH_MARK: u16 = 4;
        pub const MIGRATE_DATA: u16 = 5;
        pub const DISCONNECTING: u16 = 6;
    }
    /// `SPICE_MSGC_MAIN_*`
    pub mod main {
        pub const CLIENT_INFO: u16 = 101;
        pub const MIGRATE_CONNECTED: u16 = 102;
        pub const MIGRATE_CONNECT_ERROR: u16 = 103;
        pub const ATTACH_CHANNELS: u16 = 104;
        pub const MOUSE_MODE_REQUEST: u16 = 105;
        pub const AGENT_START: u16 = 106;
        pub const AGENT_DATA: u16 = 107;
        pub const AGENT_TOKEN: u16 = 108;
        pub const MIGRATE_END: u16 = 109;
        pub const MIGRATE_DST_DO_SEAMLESS: u16 = 110;
        pub const MIGRATE_CONNECTED_SEAMLESS: u16 = 111;
        pub const QUALITY_INDICATOR: u16 = 112;
    }
    /// `SPICE_MSGC_DISPLAY_*`
    pub mod display {
        pub const INIT: u16 = 101;
        pub const STREAM_REPORT: u16 = 102;
        pub const PREFERRED_COMPRESSION: u16 = 103;
        pub const GL_DRAW_DONE: u16 = 104;
        pub const PREFERRED_VIDEO_CODEC_TYPE: u16 = 105;
    }
    /// `SPICE_MSGC_INPUTS_*`
    pub mod inputs {
        pub const KEY_DOWN: u16 = 101;
        pub const KEY_UP: u16 = 102;
        pub const KEY_MODIFIERS: u16 = 103;
        pub const KEY_SCANCODE: u16 = 104;
        pub const MOUSE_MOTION: u16 = 111;
        pub const MOUSE_POSITION: u16 = 112;
        pub const MOUSE_PRESS: u16 = 113;
        pub const MOUSE_RELEASE: u16 = 114;
    }
    /// `SPICE_MSGC_RECORD_*`
    pub mod record {
        pub const DATA: u16 = 101;
        pub const MODE: u16 = 102;
        pub const START_MARK: u16 = 103;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn magic_is_redq() {
        assert_eq!(SPICE_MAGIC, 0x5144_4552);
        assert_eq!(SPICE_TICKET_PUBKEY_BYTES, 162);
    }

    #[test]
    fn channel_names_round_trip() {
        for t in channel_type::MAIN..=channel_type::WEBDAV {
            assert_eq!(channel_type::from_name(channel_type::name(t)), Some(t));
        }
        assert_eq!(channel_type::from_name("nope"), None);
    }
}
