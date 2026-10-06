#![no_main]
//! Every server message demarshaller in spice-proto.
use spice_proto::msg::{base, cursor, display, inputs, main};

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    if data.len() < 2 {
        return;
    }
    let (ty, b) = (u16::from_le_bytes([data[0], data[1]]), &data[2..]);
    let _ = display::decode_draw(302 + ty % 17, b);
    let _ = display::CopyBits::decode(b);
    let _ = display::SurfaceCreate::decode(b);
    let _ = display::MonitorsConfig::decode(b);
    let _ = display::StreamCreate::decode(b);
    let _ = display::StreamData::decode(b, ty & 1 != 0);
    let _ = display::decode_stream_clip(b);
    let _ = display::decode_inval_list(b);
    let _ = base::WaitForChannels::decode(b);
    let _ = base::Notify::decode(b);
    let _ = base::Ping::decode(b);
    let _ = main::Init::decode(b);
    let _ = main::ChannelsList::decode(b);
    let _ = main::decode_name(b);
    let _ = cursor::Init::decode(b);
    let _ = cursor::Set::decode(b);
    let _ = inputs::decode_modifiers(b);
    let _ = spice_proto::header::LinkReply::decode(b);
    let _ = spice_proto::header::sub_messages(b, ty as u32);
});
