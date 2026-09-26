//! Decodes arbitrary bytes as PNG, JPEG, and TIFF, and round-trips any image
//! that decodes through the PNG encoder.
#![no_main]

use libfuzzer_sys::fuzz_target;
use sigilbuzz_render::{decode_jpeg, decode_png, decode_tiff, encode_png, rescale_bilinear};

fuzz_target!(|data: &[u8]| {
    for decoded in [decode_png(data), decode_jpeg(data), decode_tiff(data)].into_iter().flatten() {
        let png = encode_png(&decoded);
        let _ = decode_png(&png);
        let _ = rescale_bilinear(&decoded, 7, 5);
    }
});
