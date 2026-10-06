#![no_main]
libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    let palette: Vec<u32> = (0..16).collect();
    let _ = spice_decode::lz::decode(data, Some(&palette));
    let _ = spice_decode::lz::decode(data, None);
});
