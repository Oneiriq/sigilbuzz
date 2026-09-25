//! Unwraps arbitrary bytes as WOFF1 and WOFF2, and wraps arbitrary bytes as
//! SFNT in both directions.
#![no_main]

use libfuzzer_sys::fuzz_target;
use sigilbuzz_woff::{unwrap_woff1, unwrap_woff2, wrap_woff1, wrap_woff2};

fuzz_target!(|data: &[u8]| {
    if let Ok(sfnt) = unwrap_woff1(data) {
        let _ = wrap_woff1(&sfnt);
    }
    if let Ok(sfnt) = unwrap_woff2(data) {
        let _ = wrap_woff2(&sfnt);
    }
    if let Ok(woff) = wrap_woff1(data) {
        let _ = unwrap_woff1(&woff);
    }
    if let Ok(woff2) = wrap_woff2(data) {
        let _ = unwrap_woff2(&woff2);
    }
});
