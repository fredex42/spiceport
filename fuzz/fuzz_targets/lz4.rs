#![no_main]
libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    if data.len() < 2 {
        return;
    }
    let _ = spice_decode::lz4::decode(&data[2..], data[0] as u32, data[1] as u32);
});
