#![no_main]
libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    let _ = spice_decode::jpeg::decode(data);
});
