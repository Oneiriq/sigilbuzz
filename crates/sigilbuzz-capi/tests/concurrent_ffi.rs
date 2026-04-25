//! Concurrency regression for sigilbuzz-capi (#172).
//!
//! When #172 was filed the cdylib build-lock race surfaced as
//! intermittent C-link failures, but it also raised the question
//! of whether the FFI surface itself is safe under genuine
//! parallel use. This test pins that down: four threads run an
//! end-to-end shape pipeline (`hb_blob_create_from_file` →
//! `hb_face_create` → `hb_font_create` → `hb_shape` → glyph
//! readout) over the same Open Sans fixture, repeating each loop
//! enough times to flush out any non-deterministic lifetime bug
//! that a single-threaded test would miss.
//!
//! The goal is *not* shared state between threads — every thread
//! owns its own blob/face/font/buffer chain — but to prove that
//! independent FFI clients don't trip over each other's
//! allocations, drop one another's resources, or cause the test
//! harness to UB-trap. The whole exercise is a fast in-process
//! check (it consumes sigilbuzz_capi as an rlib, no cdylib
//! involvement), so it costs nothing in CI and is valuable as a
//! permanent guard now that we expect concurrent test execution
//! to be the default.

#![cfg(unix)]

use core::ffi::{c_char, c_uint};
use core::ptr;
use core::slice;
use std::thread;

use sigilbuzz_capi::{
    hb_blob_create, hb_blob_destroy, hb_buffer_add_utf8, hb_buffer_create, hb_buffer_destroy,
    hb_buffer_get_glyph_infos, hb_buffer_get_glyph_positions, hb_buffer_set_direction,
    hb_buffer_set_script, hb_face_create, hb_face_destroy, hb_face_get_upem, hb_font_create,
    hb_font_destroy, hb_shape_full, HB_DIRECTION_LTR, HB_MEMORY_MODE_READONLY, HB_SCRIPT_LATIN,
};

/// Open Sans Regular — the same fixture every other capi
/// integration test reaches for. Embedded at compile time so each
/// thread gets a static, read-only byte slice with no I/O on the
/// hot path.
const OPEN_SANS: &[u8] = include_bytes!("../../../tests/fixtures/opensans_regular.ttf");

/// Drives one end-to-end shape on a private blob/face/font/buffer
/// chain. Returns the count of glyphs the shaper emitted so the
/// caller can sanity-check the result — any value above zero is
/// proof the FFI round-trip survived.
fn shape_once(text: &[u8]) -> usize {
    unsafe {
        let blob = hb_blob_create(
            OPEN_SANS.as_ptr().cast::<c_char>(),
            OPEN_SANS.len() as c_uint,
            HB_MEMORY_MODE_READONLY,
            ptr::null_mut(),
            None,
        );
        assert!(!blob.is_null(), "blob create");
        let face = hb_face_create(blob, 0);
        assert!(!face.is_null(), "face create");
        let upem = hb_face_get_upem(face);
        assert!(upem > 0, "upem must be positive");
        let font = hb_font_create(face);
        assert!(!font.is_null(), "font create");

        let buffer = hb_buffer_create();
        assert!(!buffer.is_null(), "buffer create");
        hb_buffer_add_utf8(
            buffer,
            text.as_ptr().cast::<c_char>(),
            text.len() as i32,
            0,
            -1,
        );
        hb_buffer_set_direction(buffer, HB_DIRECTION_LTR);
        hb_buffer_set_script(buffer, HB_SCRIPT_LATIN);

        let ok = hb_shape_full(font, buffer, ptr::null(), 0, ptr::null());
        assert_eq!(ok, 1, "shape must succeed");

        let mut len: c_uint = 0;
        let infos = hb_buffer_get_glyph_infos(buffer, &mut len);
        assert!(!infos.is_null(), "glyph infos pointer");
        let glyphs = slice::from_raw_parts(infos, len as usize).len();

        // Touch the positions array too so any cross-thread
        // aliasing would surface here as well.
        let mut plen: c_uint = 0;
        let positions = hb_buffer_get_glyph_positions(buffer, &mut plen);
        assert_eq!(plen, len, "positions/info count must agree");
        assert!(!positions.is_null(), "positions pointer");
        let _ = slice::from_raw_parts(positions, plen as usize);

        hb_buffer_destroy(buffer);
        hb_font_destroy(font);
        hb_face_destroy(face);
        hb_blob_destroy(blob);

        glyphs
    }
}

/// Four threads, each shaping the same string repeatedly. Any
/// inter-thread interference — a double-free, a torn pointer, a
/// shared-state mutation — manifests as a panic in `shape_once`
/// or a thread-join failure here. We use plain `b"Hello"` because
/// the assertion is on *survival under concurrency*, not on
/// shaping correctness, which is already covered by the inline
/// `lib.rs` tests and `paint_bridge.rs`.
#[test]
fn ffi_pipeline_is_safe_across_four_threads() {
    const THREADS: usize = 4;
    const ITERATIONS: usize = 16;

    let mut handles = Vec::with_capacity(THREADS);
    for _ in 0..THREADS {
        handles.push(thread::spawn(|| {
            let mut total = 0usize;
            for _ in 0..ITERATIONS {
                total += shape_once(b"Hello");
            }
            total
        }));
    }

    let mut grand_total = 0usize;
    for h in handles {
        grand_total += h.join().expect("worker thread panicked");
    }
    // Five-glyph minimum per "Hello" (latin script, no contextual
    // forms collapse it shorter), times THREADS * ITERATIONS, is a
    // safe lower bound that doesn't constrain shaper output.
    assert!(
        grand_total >= 5 * THREADS * ITERATIONS,
        "expected at least {} glyphs total, got {grand_total}",
        5 * THREADS * ITERATIONS
    );
}
