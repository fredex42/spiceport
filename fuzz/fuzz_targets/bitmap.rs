#![no_main]
libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    if data.len() < 6 {
        return;
    }
    let (h, rest) = data.split_at(6);
    let pal: Vec<u32> = (0..256).collect();
    let (fmt, flags, w, ht, stride) = (
        h[0] % 12,
        h[1],
        h[2] as u32,
        h[3] as u32,
        h[4] as u32 + h[5] as u32,
    );
    let _ = spice_decode::bitmap::decode(fmt, flags, w, ht, stride, rest, Some(&pal));
    let _ = spice_decode::bitmap::decode_mask(fmt, flags, w, ht, stride, rest, h[5] & 1 != 0);
});
