#![no_main]
use spice_decode::glz::GlzWindow;

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    // A fresh window, so references to other images fail fast instead of
    // waiting for the timeout: feed two images so cross-image matches run.
    let w = GlzWindow::new();
    let (a, b) = data.split_at(data.len() / 2);
    let _ = spice_decode::glz::decode(&w, a, None);
    let _ = spice_decode::glz::decode(&w, b, None);
    let _ = spice_decode::glz::decode_zlib(&w, data, 4096, None);
});
