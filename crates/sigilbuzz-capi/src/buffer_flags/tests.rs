//! Tests for the buffer flags and cluster level, driven through the
//! exported `hb_*` functions.

use alloc::vec::Vec;
use core::ffi::{c_char, c_int, c_uint};
use core::ptr;

use sigilbuzz::{BufferFlags, ClusterLevel};

use super::*;
use crate::{
    hb_blob_create, hb_blob_destroy, hb_buffer_add_utf8, hb_buffer_clear_contents,
    hb_buffer_create, hb_buffer_destroy, hb_buffer_get_glyph_infos, hb_buffer_reset,
    hb_buffer_set_direction, hb_face_create, hb_face_destroy, hb_font_create, hb_font_destroy,
    hb_font_t, hb_shape, HB_DIRECTION_LTR, HB_MEMORY_MODE_READONLY,
};

const OPEN_SANS: &[u8] = include_bytes!("../../../../tests/fixtures/opensans_regular.ttf");
const DEVANAGARI: &[u8] = include_bytes!("../../../../tests/fonts/NotoSansDevanagari-Regular.ttf");

/// Runs `f` with the buffer's locked state.
fn with_state<R>(buffer: *mut hb_buffer_t, f: impl FnOnce(&BufferState) -> R) -> R {
    // SAFETY: tests only pass live buffers from hb_buffer_create.
    let inner = unsafe { &(*buffer).inner };
    let state = inner.state.lock();
    f(&state)
}

/// A font on static bytes, destroyed on drop.
struct TestFont(*mut hb_font_t);

impl TestFont {
    fn new(data: &'static [u8]) -> Self {
        // SAFETY: `data` is a live static byte slice.
        unsafe {
            let blob = hb_blob_create(
                data.as_ptr().cast::<c_char>(),
                data.len() as c_uint,
                HB_MEMORY_MODE_READONLY,
                ptr::null_mut(),
                None,
            );
            let face = hb_face_create(blob, 0);
            let font = hb_font_create(face);
            hb_face_destroy(face);
            hb_blob_destroy(blob);
            Self(font)
        }
    }

    /// Clears `buffer`'s contents, adds `text` left to right, shapes,
    /// and returns `(glyph id, cluster)` pairs.
    fn shape(&self, buffer: *mut hb_buffer_t, text: &str) -> Vec<(u32, u32)> {
        let len = c_int::try_from(text.len()).unwrap();
        // SAFETY: both handles are live and (text, len) is a byte slice.
        unsafe {
            hb_buffer_clear_contents(buffer);
            hb_buffer_add_utf8(buffer, text.as_ptr().cast::<c_char>(), len, 0, -1);
            hb_buffer_set_direction(buffer, HB_DIRECTION_LTR);
            hb_shape(self.0, buffer, ptr::null(), 0);
            let mut n: c_uint = 0;
            let infos = hb_buffer_get_glyph_infos(buffer, &mut n);
            core::slice::from_raw_parts(infos, n as usize)
                .iter()
                .map(|g| (g.codepoint, g.cluster))
                .collect()
        }
    }
}

impl Drop for TestFont {
    fn drop(&mut self) {
        // SAFETY: created by hb_font_create.
        unsafe { hb_font_destroy(self.0) };
    }
}

fn clusters(rows: &[(u32, u32)]) -> Vec<u32> {
    rows.iter().map(|&(_, c)| c).collect()
}

#[test]
fn constants_match_harfbuzz() {
    assert_eq!(HB_BUFFER_FLAG_DEFAULT, 0x00);
    assert_eq!(HB_BUFFER_FLAG_BOT, 0x01);
    assert_eq!(HB_BUFFER_FLAG_EOT, 0x02);
    assert_eq!(HB_BUFFER_FLAG_PRESERVE_DEFAULT_IGNORABLES, 0x04);
    assert_eq!(HB_BUFFER_FLAG_REMOVE_DEFAULT_IGNORABLES, 0x08);
    assert_eq!(HB_BUFFER_FLAG_DO_NOT_INSERT_DOTTED_CIRCLE, 0x10);
    assert_eq!(HB_BUFFER_CLUSTER_LEVEL_MONOTONE_GRAPHEMES, 0);
    assert_eq!(HB_BUFFER_CLUSTER_LEVEL_MONOTONE_CHARACTERS, 1);
    assert_eq!(HB_BUFFER_CLUSTER_LEVEL_CHARACTERS, 2);
    assert_eq!(HB_BUFFER_CLUSTER_LEVEL_GRAPHEMES, 3);
    assert_eq!(
        HB_BUFFER_CLUSTER_LEVEL_DEFAULT,
        HB_BUFFER_CLUSTER_LEVEL_MONOTONE_GRAPHEMES
    );
    // The C constants and the Rust flags agree bit for bit.
    assert_eq!(HB_BUFFER_FLAG_BOT, BufferFlags::BOT.bits());
    assert_eq!(
        HB_BUFFER_FLAG_DO_NOT_INSERT_DOTTED_CIRCLE,
        BufferFlags::DO_NOT_INSERT_DOTTED_CIRCLE.bits()
    );
}

#[test]
fn a_new_buffer_has_harfbuzz_defaults() {
    let buffer = hb_buffer_create();
    // SAFETY: created above.
    unsafe {
        assert_eq!(hb_buffer_get_flags(buffer), HB_BUFFER_FLAG_DEFAULT);
        assert_eq!(
            hb_buffer_get_cluster_level(buffer),
            HB_BUFFER_CLUSTER_LEVEL_MONOTONE_GRAPHEMES
        );
    }
    with_state(buffer, |s| {
        assert_eq!(s.buffer.flags(), BufferFlags::DEFAULT);
        assert_eq!(s.buffer.cluster_level(), ClusterLevel::MonotoneGraphemes);
    });
    // SAFETY: created above.
    unsafe { hb_buffer_destroy(buffer) };
}

#[test]
fn flags_round_trip_and_reach_the_core_buffer() {
    let buffer = hb_buffer_create();
    // BOT plus HarfBuzz's VERIFY bit, which sigilbuzz has no behavior
    // for: stored and returned, but not passed on.
    // SAFETY: created above.
    unsafe { hb_buffer_set_flags(buffer, HB_BUFFER_FLAG_BOT | 0x20) };
    // SAFETY: created above.
    assert_eq!(unsafe { hb_buffer_get_flags(buffer) }, 0x21);
    with_state(buffer, |s| assert_eq!(s.buffer.flags(), BufferFlags::BOT));
    // SAFETY: created above.
    unsafe { hb_buffer_set_cluster_level(buffer, HB_BUFFER_CLUSTER_LEVEL_GRAPHEMES) };
    with_state(buffer, |s| {
        assert_eq!(s.buffer.cluster_level(), ClusterLevel::Graphemes);
    });
    // A level outside the enum is kept as given and shapes like
    // CHARACTERS (neither monotone nor grapheme-forming in HarfBuzz).
    // SAFETY: created above.
    unsafe { hb_buffer_set_cluster_level(buffer, 7) };
    // SAFETY: created above.
    assert_eq!(unsafe { hb_buffer_get_cluster_level(buffer) }, 7);
    with_state(buffer, |s| {
        assert_eq!(s.buffer.cluster_level(), ClusterLevel::Characters);
    });
    // SAFETY: created above.
    unsafe { hb_buffer_destroy(buffer) };
}

#[test]
fn clear_contents_keeps_the_settings_and_reset_restores_them() {
    let buffer = hb_buffer_create();
    // SAFETY: created above.
    unsafe {
        hb_buffer_set_flags(buffer, HB_BUFFER_FLAG_REMOVE_DEFAULT_IGNORABLES);
        hb_buffer_set_cluster_level(buffer, HB_BUFFER_CLUSTER_LEVEL_CHARACTERS);
        hb_buffer_add_utf8(buffer, c"abc".as_ptr(), -1, 0, -1);
        hb_buffer_clear_contents(buffer);
        assert_eq!(
            hb_buffer_get_flags(buffer),
            HB_BUFFER_FLAG_REMOVE_DEFAULT_IGNORABLES
        );
        assert_eq!(
            hb_buffer_get_cluster_level(buffer),
            HB_BUFFER_CLUSTER_LEVEL_CHARACTERS
        );
    }
    with_state(buffer, |s| {
        assert_eq!(s.buffer.text(), "");
        assert_eq!(s.buffer.flags(), BufferFlags::REMOVE_DEFAULT_IGNORABLES);
        assert_eq!(s.buffer.cluster_level(), ClusterLevel::Characters);
    });
    // SAFETY: created above.
    unsafe {
        hb_buffer_add_utf8(buffer, c"abc".as_ptr(), -1, 0, -1);
        hb_buffer_reset(buffer);
        assert_eq!(hb_buffer_get_flags(buffer), HB_BUFFER_FLAG_DEFAULT);
        assert_eq!(
            hb_buffer_get_cluster_level(buffer),
            HB_BUFFER_CLUSTER_LEVEL_DEFAULT
        );
    }
    with_state(buffer, |s| {
        assert_eq!(s.buffer.text(), "");
        assert_eq!(s.buffer.flags(), BufferFlags::DEFAULT);
        assert_eq!(s.buffer.cluster_level(), ClusterLevel::MonotoneGraphemes);
    });
    // SAFETY: created above.
    unsafe { hb_buffer_destroy(buffer) };
}

#[test]
fn null_buffers_are_harmless() {
    // SAFETY: null is allowed.
    unsafe {
        hb_buffer_set_flags(ptr::null_mut(), HB_BUFFER_FLAG_BOT);
        hb_buffer_set_cluster_level(ptr::null_mut(), HB_BUFFER_CLUSTER_LEVEL_CHARACTERS);
        assert_eq!(hb_buffer_get_flags(ptr::null()), HB_BUFFER_FLAG_DEFAULT);
        assert_eq!(
            hb_buffer_get_cluster_level(ptr::null()),
            HB_BUFFER_CLUSTER_LEVEL_DEFAULT
        );
    }
}

#[test]
fn the_cluster_level_changes_shaped_clusters() {
    let font = TestFont::new(OPEN_SANS);
    let buffer = hb_buffer_create();
    // The default groups the combining acute with its base (q has no
    // precomposed form, so normalization keeps both glyphs) ...
    assert_eq!(clusters(&font.shape(buffer, "q\u{0301}")), [0, 0]);
    // ... and MONOTONE_CHARACTERS keeps it apart.
    // SAFETY: created above.
    unsafe { hb_buffer_set_cluster_level(buffer, HB_BUFFER_CLUSTER_LEVEL_MONOTONE_CHARACTERS) };
    assert_eq!(clusters(&font.shape(buffer, "q\u{0301}")), [0, 1]);
    // SAFETY: created above.
    unsafe { hb_buffer_destroy(buffer) };
}

#[test]
fn flags_change_the_shaped_glyphs() {
    let font = TestFont::new(OPEN_SANS);
    let buffer = hb_buffer_create();
    let text = "a\u{200B}b";
    let hidden = font.shape(buffer, text);
    assert_eq!(hidden.len(), 3);
    // SAFETY: created above.
    unsafe { hb_buffer_set_flags(buffer, HB_BUFFER_FLAG_REMOVE_DEFAULT_IGNORABLES) };
    let removed = font.shape(buffer, text);
    assert_eq!(removed.len(), 2);
    assert_eq!(removed[0].0, hidden[0].0);
    assert_eq!(removed[1].0, hidden[2].0);

    let font = TestFont::new(DEVANAGARI);
    // SAFETY: created above.
    unsafe { hb_buffer_set_flags(buffer, HB_BUFFER_FLAG_DEFAULT) };
    let plain = font.shape(buffer, "\u{0301}a");
    // SAFETY: created above.
    unsafe { hb_buffer_set_flags(buffer, HB_BUFFER_FLAG_BOT) };
    let circled = font.shape(buffer, "\u{0301}a");
    assert_eq!(circled.len(), plain.len() + 1);
    // SAFETY: created above.
    unsafe {
        hb_buffer_set_flags(
            buffer,
            HB_BUFFER_FLAG_BOT | HB_BUFFER_FLAG_DO_NOT_INSERT_DOTTED_CIRCLE,
        );
    }
    assert_eq!(font.shape(buffer, "\u{0301}a").len(), plain.len());
    // SAFETY: created above.
    unsafe { hb_buffer_destroy(buffer) };
}

#[test]
fn the_not_found_variation_selector_glyph_is_a_setting() {
    const NOTO_CJK_UVS: &[u8] =
        include_bytes!("../../../../tests/fixtures/noto_sans_cjk_jp_uvs_subset.otf");
    let font = TestFont::new(NOTO_CJK_UVS);
    let buffer = hb_buffer_create();
    // SAFETY: created above.
    unsafe {
        assert_eq!(
            hb_buffer_get_not_found_variation_selector_glyph(buffer),
            HB_CODEPOINT_INVALID
        );
    }
    // HarfBuzz 14.5.0 at its default MONOTONE_GRAPHEMES: the font has no
    // glyph for "a" with VS1, so the selector is hidden (the space
    // glyph, 1) until a glyph is set.
    assert_eq!(font.shape(buffer, "a\u{FE00}"), [(4, 0), (1, 0)]);
    // SAFETY: created above.
    unsafe { hb_buffer_set_not_found_variation_selector_glyph(buffer, 5) };
    assert_eq!(font.shape(buffer, "a\u{FE00}"), [(4, 0), (5, 0)]);
    // SAFETY: created above.
    unsafe {
        assert_eq!(hb_buffer_get_not_found_variation_selector_glyph(buffer), 5);
        hb_buffer_clear_contents(buffer);
        assert_eq!(hb_buffer_get_not_found_variation_selector_glyph(buffer), 5);
        hb_buffer_reset(buffer);
        assert_eq!(
            hb_buffer_get_not_found_variation_selector_glyph(buffer),
            HB_CODEPOINT_INVALID
        );
        hb_buffer_set_not_found_variation_selector_glyph(buffer, 5);
        hb_buffer_set_not_found_variation_selector_glyph(buffer, HB_CODEPOINT_INVALID);
    }
    with_state(buffer, |s| {
        assert_eq!(s.buffer.not_found_variation_selector_glyph(), None);
    });
    // SAFETY: null is allowed, and `buffer` was created above.
    unsafe {
        hb_buffer_set_not_found_variation_selector_glyph(ptr::null_mut(), 5);
        assert_eq!(
            hb_buffer_get_not_found_variation_selector_glyph(ptr::null()),
            HB_CODEPOINT_INVALID
        );
        hb_buffer_destroy(buffer);
    }
}
