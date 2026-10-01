//! Tests for text ingest, cluster mapping, and segment properties,
//! driven through the exported `hb_*` functions.

use alloc::vec::Vec;
use core::ffi::{c_char, c_int, c_uint};
use core::ptr;

use sigilbuzz::{shape, Blob, Buffer, Face, Font, Language, UnicodeScript};

use super::*;
use crate::{
    hb_blob_create, hb_blob_destroy, hb_buffer_add_codepoints, hb_buffer_add_latin1,
    hb_buffer_add_utf16, hb_buffer_add_utf32, hb_buffer_add_utf8, hb_buffer_clear_contents,
    hb_buffer_create, hb_buffer_destroy, hb_buffer_get_glyph_infos,
    hb_buffer_guess_segment_properties, hb_buffer_reset, hb_buffer_set_direction,
    hb_buffer_set_language, hb_buffer_set_script, hb_buffer_t, hb_face_create, hb_face_destroy,
    hb_font_create, hb_font_destroy, hb_font_t, hb_shape, HB_DIRECTION_RTL,
    HB_MEMORY_MODE_READONLY, HB_SCRIPT_ARABIC, HB_SCRIPT_COMMON, HB_SCRIPT_HEBREW, HB_SCRIPT_LATIN,
};

const OPEN_SANS: &[u8] = include_bytes!("../../../../tests/fixtures/opensans_regular.ttf");
const AMIRI: &[u8] = include_bytes!("../../../../tests/fixtures/amiri_regular.ttf");

/// Runs `f` with the buffer's locked state.
fn with_state<R>(buffer: *mut hb_buffer_t, f: impl FnOnce(&BufferState) -> R) -> R {
    // SAFETY: tests only pass live buffers from hb_buffer_create.
    let inner = unsafe { &(*buffer).inner };
    let state = inner.state.lock();
    f(&state)
}

fn add_utf8(buffer: *mut hb_buffer_t, text: &[u8], item_offset: u32, item_length: c_int) {
    let len = c_int::try_from(text.len()).unwrap();
    // SAFETY: (text, len) is a live byte slice.
    unsafe {
        hb_buffer_add_utf8(
            buffer,
            text.as_ptr().cast::<c_char>(),
            len,
            item_offset,
            item_length,
        );
    }
}

fn add_utf16(buffer: *mut hb_buffer_t, text: &[u16], item_offset: u32, item_length: c_int) {
    let len = c_int::try_from(text.len()).unwrap();
    // SAFETY: (text, len) is a live u16 slice.
    unsafe { hb_buffer_add_utf16(buffer, text.as_ptr(), len, item_offset, item_length) };
}

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

    /// Shapes and returns `(glyph id, cluster)` pairs.
    fn shape(&self, buffer: *mut hb_buffer_t) -> Vec<(u32, u32)> {
        // SAFETY: both handles are live.
        unsafe {
            hb_shape(self.0, buffer, ptr::null(), 0);
            let mut len: c_uint = 0;
            let infos = hb_buffer_get_glyph_infos(buffer, &mut len);
            core::slice::from_raw_parts(infos, len as usize)
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

/// Core-API glyph ids for comparison.
fn core_ids(data: &[u8], setup: impl FnOnce(&mut Buffer)) -> Vec<u32> {
    let blob = Blob::new(data);
    let face = Face::parse(&blob, 0).unwrap();
    let font = Font::new(face, 1000.0);
    let mut buffer = Buffer::new();
    setup(&mut buffer);
    shape(&font, &buffer, &[])
        .unwrap()
        .glyphs
        .iter()
        .map(|g| g.glyph_id)
        .collect()
}

fn ids(shaped: &[(u32, u32)]) -> Vec<u32> {
    shaped.iter().map(|(id, _)| *id).collect()
}

fn sorted_clusters(shaped: &[(u32, u32)]) -> Vec<u32> {
    let mut clusters: Vec<u32> = shaped.iter().map(|(_, c)| *c).collect();
    clusters.sort_unstable();
    clusters
}

// --- Decoders ---------------------------------------------------------------

fn decode<E: Encoding>(text: &[E::Unit]) -> Vec<(char, usize)> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < text.len() {
        let (ch, next) = E::next(text, at, text.len());
        out.push((ch, at));
        at = next;
    }
    out
}

#[test]
fn utf8_decoding_matches_harfbuzz() {
    let valid = "a\u{E9}\u{20AC}\u{1F600}".as_bytes();
    assert_eq!(
        decode::<Utf8>(valid),
        [('a', 0), ('\u{E9}', 1), ('\u{20AC}', 3), ('\u{1F600}', 6)]
    );
    // Each malformed sequence is one U+FFFD per consumed byte: a lone
    // continuation byte, an overlong C0 lead, and a truncated
    // three-byte sequence (lead and continuation each replaced).
    let bad = b"a\x80\xC0\xAFb\xE2\x82c";
    let chars: Vec<char> = decode::<Utf8>(bad).iter().map(|(c, _)| *c).collect();
    assert_eq!(
        chars,
        [
            'a',
            REPLACEMENT,
            REPLACEMENT,
            REPLACEMENT,
            'b',
            REPLACEMENT,
            REPLACEMENT,
            'c'
        ]
    );
    // Encoded surrogates and values past U+10FFFF are malformed.
    assert_eq!(Utf8::next(b"\xED\xA0\x80", 0, 3), (REPLACEMENT, 1));
    assert_eq!(Utf8::next(b"\xF4\x90\x80\x80", 0, 4), (REPLACEMENT, 1));
    // A sequence cut by `end` is malformed even if the bytes follow.
    assert_eq!(Utf8::next("\u{E9}".as_bytes(), 0, 1), (REPLACEMENT, 1));
}

#[test]
fn utf8_prev_walks_back_one_character() {
    let text = "a\u{E9}\u{1F600}".as_bytes();
    assert_eq!(Utf8::prev(text, 0, text.len()), ('\u{1F600}', 3));
    assert_eq!(Utf8::prev(text, 0, 3), ('\u{E9}', 1));
    assert_eq!(Utf8::prev(text, 0, 1), ('a', 0));
    // A stray continuation byte steps back one byte.
    assert_eq!(Utf8::prev(b"a\x80", 0, 2), (REPLACEMENT, 1));
}

#[test]
fn utf16_decoding_matches_harfbuzz() {
    let text: Vec<u16> = "a\u{1F600}b".encode_utf16().collect();
    assert_eq!(
        decode::<Utf16>(&text),
        [('a', 0), ('\u{1F600}', 1), ('b', 3)]
    );
    assert_eq!(Utf16::prev(&text, 0, 3), ('\u{1F600}', 1));
    // Lone and out-of-order surrogates become U+FFFD.
    let bad = [0xDC00u16, 0x41, 0xD800, 0x42, 0xD800];
    let chars: Vec<char> = decode::<Utf16>(&bad).iter().map(|(c, _)| *c).collect();
    assert_eq!(chars, [REPLACEMENT, 'A', REPLACEMENT, 'B', REPLACEMENT]);
    assert_eq!(Utf16::prev(&[0x41, 0xDC00], 0, 2), (REPLACEMENT, 1));
}

#[test]
fn utf32_decoding_matches_harfbuzz() {
    let text = [0x61u32, 0x1F600, 0xD800, 0xDFFF, 0x11_0000, 0x10_FFFF];
    let chars: Vec<char> = decode::<Utf32>(&text).iter().map(|(c, _)| *c).collect();
    assert_eq!(
        chars,
        [
            'a',
            '\u{1F600}',
            REPLACEMENT,
            REPLACEMENT,
            REPLACEMENT,
            '\u{10FFFF}'
        ]
    );
    assert_eq!(Utf32::prev(&text, 0, 2), ('\u{1F600}', 1));
    assert_eq!(Utf32::prev(&text, 0, 3), (REPLACEMENT, 2));
}

#[test]
fn latin1_decoding_maps_bytes_to_the_first_256_code_points() {
    let text = [0x41u8, 0xE9, 0xFF, 0x80];
    assert_eq!(
        decode::<Latin1>(&text),
        [('A', 0), ('\u{E9}', 1), ('\u{FF}', 2), ('\u{80}', 3)]
    );
    assert_eq!(Latin1::prev(&text, 0, 2), ('\u{E9}', 1));
}

#[test]
fn item_length_follows_harfbuzz() {
    assert_eq!(ItemLength::from_c(-1), ItemLength::ToEnd);
    assert_eq!(ItemLength::from_c(0), ItemLength::Units(0));
    assert_eq!(ItemLength::from_c(7), ItemLength::Units(7));
    // HarfBuzz clamps other negative lengths to zero.
    assert_eq!(ItemLength::from_c(-2), ItemLength::Units(0));
    assert_eq!(ItemLength::from_c(c_int::MIN), ItemLength::Units(0));
}

#[test]
fn a_negative_item_length_adds_no_text_but_sets_the_context() {
    let buffer = hb_buffer_create();
    add_utf8(buffer, b"abc|def", 3, -5);
    with_state(buffer, |s| {
        assert_eq!(s.buffer.text(), "");
        assert_eq!(s.buffer.pre_context(), "abc");
        assert_eq!(s.buffer.post_context(), "|def");
    });
    // A later item still sees an empty buffer, so it takes its own
    // pre-context, as in HarfBuzz.
    add_utf8(buffer, b"xy|z", 2, -1);
    with_state(buffer, |s| {
        assert_eq!(s.buffer.text(), "|z");
        assert_eq!(s.buffer.pre_context(), "xy");
        assert_eq!(s.buffer.post_context(), "");
    });
    // SAFETY: created above.
    unsafe { hb_buffer_destroy(buffer) };
}

#[test]
fn an_item_offset_past_the_end_is_clamped() {
    let buffer = hb_buffer_create();
    add_utf8(buffer, b"abc", 10, 2);
    with_state(buffer, |s| {
        assert_eq!(s.buffer.text(), "");
        assert_eq!(s.buffer.pre_context(), "abc");
        assert_eq!(s.buffer.post_context(), "");
    });
    // SAFETY: created above.
    unsafe { hb_buffer_destroy(buffer) };
}

// --- Ingest -----------------------------------------------------------------

#[test]
fn utf8_item_sets_text_and_context() {
    let buffer = hb_buffer_create();
    add_utf8(buffer, "abc\u{0628}\u{0628}def".as_bytes(), 3, 4);
    with_state(buffer, |s| {
        assert_eq!(s.buffer.text(), "\u{0628}\u{0628}");
        assert_eq!(s.buffer.pre_context(), "abc");
        assert_eq!(s.buffer.post_context(), "def");
        assert_eq!(s.clusters.entries, [(0, 3), (2, 5)]);
    });
    // SAFETY: created above.
    unsafe { hb_buffer_destroy(buffer) };
}

#[test]
fn context_keeps_five_characters_each_side() {
    let buffer = hb_buffer_create();
    add_utf8(buffer, b"0123456789XY0123456789", 10, 2);
    with_state(buffer, |s| {
        assert_eq!(s.buffer.text(), "XY");
        assert_eq!(s.buffer.pre_context(), "56789");
        assert_eq!(s.buffer.post_context(), "01234");
    });
    // SAFETY: created above.
    unsafe { hb_buffer_destroy(buffer) };
}

#[test]
fn pre_context_is_only_taken_by_an_empty_buffer() {
    let buffer = hb_buffer_create();
    add_utf8(buffer, b"pre|one|post", 4, 3);
    add_utf8(buffer, b"xyz|two|end", 4, 3);
    with_state(buffer, |s| {
        assert_eq!(s.buffer.text(), "onetwo");
        // The second call saw a non-empty buffer: pre-context stays.
        assert_eq!(s.buffer.pre_context(), "pre|");
        // Post-context always comes from the latest call.
        assert_eq!(s.buffer.post_context(), "|end");
        // Each call's clusters index its own array.
        let clusters: Vec<u32> = s.clusters.entries.iter().map(|e| e.1).collect();
        assert_eq!(clusters, [4, 5, 6, 4, 5, 6]);
    });
    // SAFETY: created above.
    unsafe { hb_buffer_destroy(buffer) };
}

#[test]
fn utf16_item_clusters_count_code_units() {
    let text: Vec<u16> = "x\u{1F600}\u{E9}\u{1F601}y".encode_utf16().collect();
    let buffer = hb_buffer_create();
    // Item: the first emoji, e-acute, and the second emoji (units 1..6).
    add_utf16(buffer, &text, 1, 5);
    with_state(buffer, |s| {
        assert_eq!(s.buffer.text(), "\u{1F600}\u{E9}\u{1F601}");
        assert_eq!(s.buffer.pre_context(), "x");
        assert_eq!(s.buffer.post_context(), "y");
        let clusters: Vec<u32> = s.clusters.entries.iter().map(|e| e.1).collect();
        assert_eq!(clusters, [1, 3, 4]);
    });
    // SAFETY: created above.
    unsafe { hb_buffer_destroy(buffer) };
}

fn add_utf32(buffer: *mut hb_buffer_t, text: &[u32], item_offset: u32, item_length: c_int) {
    let len = c_int::try_from(text.len()).unwrap();
    // SAFETY: (text, len) is a live u32 slice.
    unsafe { hb_buffer_add_utf32(buffer, text.as_ptr(), len, item_offset, item_length) };
}

fn add_codepoints(buffer: *mut hb_buffer_t, text: &[u32], item_offset: u32, item_length: c_int) {
    let len = c_int::try_from(text.len()).unwrap();
    // SAFETY: (text, len) is a live u32 slice.
    unsafe { hb_buffer_add_codepoints(buffer, text.as_ptr(), len, item_offset, item_length) };
}

fn add_latin1(buffer: *mut hb_buffer_t, text: &[u8], item_offset: u32, item_length: c_int) {
    let len = c_int::try_from(text.len()).unwrap();
    // SAFETY: (text, len) is a live byte slice.
    unsafe { hb_buffer_add_latin1(buffer, text.as_ptr(), len, item_offset, item_length) };
}

fn code_points(text: &str) -> Vec<u32> {
    text.chars().map(u32::from).collect()
}

#[test]
fn utf32_item_clusters_count_code_points() {
    let text = code_points("x\u{1F600}\u{E9}\u{1F601}y");
    for add in [add_utf32, add_codepoints] {
        let buffer = hb_buffer_create();
        add(buffer, &text, 1, 3);
        with_state(buffer, |s| {
            assert_eq!(s.buffer.text(), "\u{1F600}\u{E9}\u{1F601}");
            assert_eq!(s.buffer.pre_context(), "x");
            assert_eq!(s.buffer.post_context(), "y");
            let clusters: Vec<u32> = s.clusters.entries.iter().map(|e| e.1).collect();
            assert_eq!(clusters, [1, 2, 3]);
        });
        // SAFETY: created above.
        unsafe { hb_buffer_destroy(buffer) };
    }
}

#[test]
fn invalid_code_points_become_replacement_characters() {
    let text = [0x61u32, 0xD800, 0x11_0000, 0x62];
    for add in [add_utf32, add_codepoints] {
        let buffer = hb_buffer_create();
        add(buffer, &text, 0, -1);
        with_state(buffer, |s| {
            assert_eq!(s.buffer.text(), "a\u{FFFD}\u{FFFD}b");
            let clusters: Vec<u32> = s.clusters.entries.iter().map(|e| e.1).collect();
            assert_eq!(clusters, [0, 1, 2, 3]);
        });
        // SAFETY: created above.
        unsafe { hb_buffer_destroy(buffer) };
    }
}

#[test]
fn latin1_item_sets_text_context_and_byte_clusters() {
    let buffer = hb_buffer_create();
    add_latin1(buffer, b"ab\xE9t\xE9cd", 2, 3);
    with_state(buffer, |s| {
        assert_eq!(s.buffer.text(), "\u{E9}t\u{E9}");
        assert_eq!(s.buffer.pre_context(), "ab");
        assert_eq!(s.buffer.post_context(), "cd");
        let clusters: Vec<u32> = s.clusters.entries.iter().map(|e| e.1).collect();
        assert_eq!(clusters, [2, 3, 4]);
    });
    // SAFETY: created above.
    unsafe { hb_buffer_destroy(buffer) };
}

#[test]
fn zero_terminated_utf32_and_latin1_read_to_the_terminator() {
    let buffer = hb_buffer_create();
    let utf32 = [0x48u32, 0x69, 0, 0x78];
    let latin1 = b"\xE9!\0z";
    // SAFETY: both arrays are zero-terminated and the buffer is live.
    unsafe {
        hb_buffer_add_utf32(buffer, utf32.as_ptr(), -1, 0, -1);
        hb_buffer_add_latin1(buffer, latin1.as_ptr(), -1, 0, -1);
    }
    with_state(buffer, |s| assert_eq!(s.buffer.text(), "Hi\u{E9}!"));
    // SAFETY: created above.
    unsafe { hb_buffer_destroy(buffer) };
}

#[test]
fn null_text_or_buffer_adds_nothing() {
    let buffer = hb_buffer_create();
    // SAFETY: null pointers are rejected before any read.
    unsafe {
        hb_buffer_add_utf32(buffer, ptr::null(), 3, 0, -1);
        hb_buffer_add_codepoints(buffer, ptr::null(), 3, 0, -1);
        hb_buffer_add_latin1(buffer, ptr::null(), 3, 0, -1);
        hb_buffer_add_utf32(ptr::null_mut(), [0x41u32].as_ptr(), 1, 0, -1);
    }
    with_state(buffer, |s| assert!(s.buffer.is_empty()));
    // SAFETY: created above.
    unsafe { hb_buffer_destroy(buffer) };
}

#[test]
fn every_add_function_shapes_like_utf8() {
    let font = TestFont::new(OPEN_SANS);
    // Twelve characters after two of pre-context; the e-acute takes
    // two UTF-8 bytes but one unit everywhere else.
    let text = "--caf\u{E9} au lait--";
    let (offset, len) = (2u32, 12);
    let reference = hb_buffer_create();
    add_utf8(reference, text.as_bytes(), offset, 13);
    let expected = ids(&font.shape(reference));
    // SAFETY: created above.
    unsafe { hb_buffer_destroy(reference) };

    let utf32 = code_points(text);
    let latin1: Vec<u8> = text.chars().map(|c| u8::try_from(c).unwrap()).collect();
    let cases: [(&str, &dyn Fn(*mut hb_buffer_t)); 3] = [
        ("utf32", &|b| add_utf32(b, &utf32, offset, len)),
        ("codepoints", &|b| add_codepoints(b, &utf32, offset, len)),
        ("latin1", &|b| add_latin1(b, &latin1, offset, len)),
    ];
    for (name, add) in cases {
        let buffer = hb_buffer_create();
        add(buffer);
        let shaped = font.shape(buffer);
        assert_eq!(ids(&shaped), expected, "{name}");
        // One character per code unit: clusters count characters.
        assert_eq!(
            sorted_clusters(&shaped),
            (2..14).collect::<Vec<u32>>(),
            "{name}"
        );
        // SAFETY: created above.
        unsafe { hb_buffer_destroy(buffer) };
    }
}

#[test]
fn malformed_input_is_replaced_not_dropped() {
    let buffer = hb_buffer_create();
    add_utf8(buffer, b"a\xFFb", 0, -1);
    add_utf16(buffer, &[0x63, 0xD800], 0, -1);
    with_state(buffer, |s| {
        assert_eq!(s.buffer.text(), "a\u{FFFD}bc\u{FFFD}");
    });
    // SAFETY: created above.
    unsafe { hb_buffer_destroy(buffer) };
}

#[test]
fn bad_item_ranges_add_nothing_or_clamp() {
    let buffer = hb_buffer_create();
    add_utf8(buffer, b"abc", 4, -1);
    add_utf8(buffer, b"abc", 0, -2);
    with_state(buffer, |s| assert!(s.buffer.is_empty()));
    add_utf8(buffer, b"abc", 1, 99);
    with_state(buffer, |s| assert_eq!(s.buffer.text(), "bc"));
    // SAFETY: created above.
    unsafe { hb_buffer_destroy(buffer) };
}

// --- Shaping through the C API ----------------------------------------------

#[test]
fn shaped_clusters_use_utf8_offsets_with_item_offset() {
    let font = TestFont::new(OPEN_SANS);
    let buffer = hb_buffer_create();
    add_utf8(buffer, "xx\u{E9}t\u{E9}yy".as_bytes(), 2, 5);
    let shaped = font.shape(buffer);
    assert_eq!(sorted_clusters(&shaped), [2, 4, 5]);
    // SAFETY: created above.
    unsafe { hb_buffer_destroy(buffer) };
}

#[test]
fn shaped_clusters_use_utf16_offsets_with_item_offset() {
    let font = TestFont::new(OPEN_SANS);
    let text: Vec<u16> = "ab\u{E9}\u{1D400}c".encode_utf16().collect();
    let buffer = hb_buffer_create();
    add_utf16(buffer, &text, 1, -1);
    let shaped = font.shape(buffer);
    assert_eq!(sorted_clusters(&shaped), [1, 2, 3, 5]);
    // SAFETY: created above.
    unsafe { hb_buffer_destroy(buffer) };
}

#[test]
fn clusters_restart_after_reset() {
    let font = TestFont::new(OPEN_SANS);
    let buffer = hb_buffer_create();
    add_utf8(buffer, b"abcdef", 3, -1);
    // SAFETY: created above.
    unsafe { hb_buffer_reset(buffer) };
    add_utf8(buffer, b"xy", 0, -1);
    assert_eq!(sorted_clusters(&font.shape(buffer)), [0, 1]);
    // SAFETY: created above.
    unsafe { hb_buffer_destroy(buffer) };
}

#[test]
fn item_offset_context_reaches_arabic_joining() {
    let font = TestFont::new(AMIRI);
    let beh = "\u{0628}";
    let text = beh.repeat(4);
    let buffer = hb_buffer_create();
    // The middle two behs, with a beh of context on each side.
    add_utf8(buffer, text.as_bytes(), 2, 4);
    // SAFETY: created above.
    unsafe { hb_buffer_set_direction(buffer, HB_DIRECTION_RTL) };
    let shaped = font.shape(buffer);
    let expected = core_ids(AMIRI, |b| {
        b.set_pre_context(beh);
        b.push_str(&beh.repeat(2));
        b.set_post_context(beh);
        b.set_direction(sigilbuzz::Direction::Rtl);
    });
    let without_context = core_ids(AMIRI, |b| {
        b.push_str(&beh.repeat(2));
        b.set_direction(sigilbuzz::Direction::Rtl);
    });
    assert_eq!(ids(&shaped), expected);
    assert_ne!(ids(&shaped), without_context);
    assert_eq!(sorted_clusters(&shaped), [2, 4]);
    // SAFETY: created above.
    unsafe { hb_buffer_destroy(buffer) };
}

// --- Segment properties -----------------------------------------------------

#[test]
fn set_script_reaches_the_core_buffer() {
    let buffer = hb_buffer_create();
    // SAFETY: created above.
    unsafe { hb_buffer_set_script(buffer, HB_SCRIPT_ARABIC) };
    with_state(buffer, |s| {
        assert_eq!(s.script, HB_SCRIPT_ARABIC);
        assert_eq!(s.buffer.script(), Some(UnicodeScript::Arabic));
    });
    // Common and scripts without a bucket keep per-run segmentation.
    for script in [HB_SCRIPT_COMMON, u32::from_be_bytes(*b"Thaa"), 0] {
        // SAFETY: created above.
        unsafe { hb_buffer_set_script(buffer, script) };
        with_state(buffer, |s| assert_eq!(s.buffer.script(), None));
    }
    // SAFETY: created above.
    unsafe { hb_buffer_destroy(buffer) };
}

#[test]
fn script_override_through_the_c_api_matches_core() {
    let font = TestFont::new(AMIRI);
    let text = "Hi \u{0628}\u{0628}";
    let buffer = hb_buffer_create();
    add_utf8(buffer, text.as_bytes(), 0, -1);
    // SAFETY: created above.
    unsafe { hb_buffer_set_script(buffer, HB_SCRIPT_LATIN) };
    let expected = core_ids(AMIRI, |b| {
        b.push_str(text);
        b.set_script(Some(UnicodeScript::Latin));
    });
    assert_eq!(ids(&font.shape(buffer)), expected);
    // SAFETY: created above.
    unsafe { hb_buffer_destroy(buffer) };
}

#[test]
fn arabic_script_over_decomposed_vowels_shapes() {
    // Thai sara am and Khmer U+17C4 decompose into two code points
    // each; a whole-buffer Arabic script used to index the joining
    // forms past their end and abort inside hb_shape.
    let font = TestFont::new(AMIRI);
    let text = "\u{0645}\u{0631}\u{062D}\u{0628}\u{0627} \u{0E2A}\u{0E33}\u{17C4}";
    for guess in [false, true] {
        let buffer = hb_buffer_create();
        add_utf8(buffer, text.as_bytes(), 0, -1);
        // SAFETY: created above.
        unsafe {
            if guess {
                hb_buffer_guess_segment_properties(buffer);
            } else {
                hb_buffer_set_script(buffer, HB_SCRIPT_ARABIC);
            }
        }
        let shaped = font.shape(buffer);
        assert!(shaped.len() >= 9, "{shaped:?}");
        // SAFETY: created above.
        unsafe { hb_buffer_destroy(buffer) };
    }
}

#[cfg(feature = "std")]
#[test]
fn set_language_reaches_the_core_buffer_and_shaping() {
    let font = TestFont::new(OPEN_SANS);
    let text = "\u{0218}\u{0219}";
    let buffer = hb_buffer_create();
    add_utf8(buffer, text.as_bytes(), 0, -1);
    // SAFETY: a NUL-terminated literal and a live buffer.
    unsafe {
        let ro = crate::hb_language_from_string(c"ro_RO".as_ptr(), -1);
        hb_buffer_set_language(buffer, ro);
    }
    with_state(buffer, |s| {
        assert_eq!(s.buffer.language().map(Language::as_str), Some("ro-ro"));
    });
    let romanian = core_ids(OPEN_SANS, |b| {
        b.push_str(text);
        b.set_language(Language::new("ro"));
    });
    assert_eq!(ids(&font.shape(buffer)), romanian);
    assert_ne!(romanian, core_ids(OPEN_SANS, |b| b.push_str(text)));
    // SAFETY: null clears the language.
    unsafe { hb_buffer_set_language(buffer, ptr::null()) };
    with_state(buffer, |s| assert!(s.buffer.language().is_none()));
    // SAFETY: created above.
    unsafe { hb_buffer_destroy(buffer) };
}

#[test]
fn clear_contents_resets_properties_and_context() {
    let buffer = hb_buffer_create();
    add_utf8(buffer, b"ab-cd-ef", 3, 2);
    // SAFETY: created above.
    unsafe {
        hb_buffer_set_direction(buffer, HB_DIRECTION_RTL);
        hb_buffer_set_script(buffer, HB_SCRIPT_HEBREW);
        hb_buffer_set_language(buffer, crate::lang_und());
        hb_buffer_clear_contents(buffer);
    }
    with_state(buffer, |s| {
        assert!(s.buffer.is_empty());
        assert_eq!(s.direction, crate::HB_DIRECTION_INVALID);
        assert_eq!(s.script, crate::HB_SCRIPT_INVALID);
        assert!(s.language.is_null());
        assert_eq!(s.buffer.direction(), sigilbuzz::Direction::Ltr);
        assert_eq!(s.buffer.script(), None);
        assert!(s.buffer.language().is_none());
        assert_eq!(s.buffer.pre_context(), "");
        assert_eq!(s.buffer.post_context(), "");
    });
    // SAFETY: created above.
    unsafe { hb_buffer_destroy(buffer) };
}

#[test]
fn invalid_direction_unsets_the_core_direction() {
    let buffer = hb_buffer_create();
    add_utf8(buffer, b"abc", 0, -1);
    // SAFETY: created above.
    unsafe { hb_buffer_set_direction(buffer, HB_DIRECTION_RTL) };
    with_state(buffer, |s| {
        assert_eq!(s.direction, HB_DIRECTION_RTL);
        assert!(s.buffer.has_explicit_direction());
    });
    // INVALID, and any other value HarfBuzz does not accept as a
    // direction, returns to the unset default instead of forcing LTR.
    for invalid in [crate::HB_DIRECTION_INVALID, 1, 8] {
        // SAFETY: created above.
        unsafe {
            hb_buffer_set_direction(buffer, HB_DIRECTION_RTL);
            hb_buffer_set_direction(buffer, invalid);
        }
        with_state(buffer, |s| {
            assert_eq!(s.direction, crate::HB_DIRECTION_INVALID);
            assert!(!s.buffer.has_explicit_direction());
            assert_eq!(s.buffer.direction(), sigilbuzz::Direction::Ltr);
            assert_eq!(s.buffer.text(), "abc");
        });
    }
    // An unset direction is filled in by guessing again.
    // SAFETY: created above.
    unsafe { hb_buffer_guess_segment_properties(buffer) };
    with_state(buffer, |s| {
        assert_eq!(s.direction, crate::HB_DIRECTION_LTR);
        assert!(s.buffer.has_explicit_direction());
    });
    // SAFETY: created above.
    unsafe { hb_buffer_destroy(buffer) };
}

#[test]
fn invalid_direction_restores_mongolian_auto_vertical() {
    const MONGOLIAN: &[u8] =
        include_bytes!("../../../../tests/fonts/NotoSansMongolian-Regular.ttf");
    let font = TestFont::new(MONGOLIAN);
    let buffer = hb_buffer_create();
    add_utf8(buffer, "\u{1820}".as_bytes(), 0, -1);
    let y_advance = |buffer: *mut hb_buffer_t| {
        // SAFETY: both handles are live.
        unsafe {
            hb_shape(font.0, buffer, ptr::null(), 0);
            let mut len: c_uint = 0;
            let pos = crate::hb_buffer_get_glyph_positions(buffer, &mut len);
            core::slice::from_raw_parts(pos, len as usize)[0].y_advance
        }
    };
    // SAFETY: created above.
    unsafe { hb_buffer_set_direction(buffer, crate::HB_DIRECTION_LTR) };
    assert_eq!(y_advance(buffer), 0);
    // SAFETY: created above.
    unsafe { hb_buffer_set_direction(buffer, crate::HB_DIRECTION_INVALID) };
    assert_ne!(y_advance(buffer), 0);
    // SAFETY: created above.
    unsafe { hb_buffer_destroy(buffer) };
}

fn guessed(text: &str, script: Option<u32>) -> (u32, u32) {
    let buffer = hb_buffer_create();
    add_utf8(buffer, text.as_bytes(), 0, -1);
    // SAFETY: created above.
    unsafe {
        if let Some(script) = script {
            hb_buffer_set_script(buffer, script);
        }
        hb_buffer_guess_segment_properties(buffer);
    }
    let out = with_state(buffer, |s| (s.script, s.direction));
    // SAFETY: created above.
    unsafe { hb_buffer_destroy(buffer) };
    out
}

#[test]
fn guess_takes_direction_from_the_script() {
    use crate::{HB_DIRECTION_LTR, HB_SCRIPT_INVALID};
    assert_eq!(guessed("Hello", None), (HB_SCRIPT_LATIN, HB_DIRECTION_LTR));
    assert_eq!(
        guessed("\u{0645}\u{0631}\u{062D}\u{0628}\u{0627}", None),
        (HB_SCRIPT_ARABIC, HB_DIRECTION_RTL)
    );
    assert_eq!(
        guessed("\u{05E9}\u{05DC}\u{05D5}\u{05DD}", None),
        (HB_SCRIPT_HEBREW, HB_DIRECTION_RTL)
    );
    // Leading digits and punctuation do not decide the script.
    assert_eq!(
        guessed("(123) \u{05E9}\u{05DC}", None),
        (HB_SCRIPT_HEBREW, HB_DIRECTION_RTL)
    );
    // No script-bearing character: the script stays invalid.
    assert_eq!(
        guessed("12:30", None),
        (HB_SCRIPT_INVALID, HB_DIRECTION_LTR)
    );
    // An explicit script without a sigilbuzz bucket still sets the
    // direction.
    let thaana = u32::from_be_bytes(*b"Thaa");
    assert_eq!(guessed("abc", Some(thaana)), (thaana, HB_DIRECTION_RTL));
}

#[test]
fn unicode_script_reads_the_script_property() {
    assert_eq!(&unicode_script('a'), b"Latn");
    assert_eq!(&unicode_script('\u{0627}'), b"Arab");
    assert_eq!(&unicode_script('0'), b"Zyyy");
    assert_eq!(&unicode_script('\u{2014}'), b"Zyyy");
    assert_eq!(&unicode_script('\u{0301}'), b"Zinh");
    assert_eq!(&unicode_script('\u{0378}'), b"Zzzz");
    assert_eq!(&unicode_script('\u{10FFFF}'), b"Zzzz");
    assert_eq!(&unicode_script('\u{3042}'), b"Hira");
    assert_eq!(&unicode_script('\u{30A2}'), b"Kana");
    assert_eq!(&unicode_script('\u{0710}'), b"Syrc");
    assert_eq!(&unicode_script('\u{1E900}'), b"Adlm");
}

#[test]
fn guess_uses_the_full_script_property() {
    use crate::HB_DIRECTION_LTR;
    let tag = |t: &[u8; 4]| u32::from_be_bytes(*t);
    // Right-to-left scripts get their script and direction, with a
    // shaping bucket (Syriac, Adlam, Mandaic, Hanifi Rohingya) or
    // without one (Thaana, Samaritan).
    for (text, script) in [
        ("\u{0710}\u{0712}", b"Syrc"),
        ("\u{0780}\u{0781}", b"Thaa"),
        ("\u{1E900}\u{1E901}", b"Adlm"),
        ("\u{0800}\u{0801}", b"Samr"),
        ("\u{0840}\u{0841}", b"Mand"),
        ("\u{10D00}\u{10D01}", b"Rohg"),
    ] {
        assert_eq!(
            guessed(text, None),
            (tag(script), HB_DIRECTION_RTL),
            "{text:?}"
        );
    }
    // Leading Common punctuation outside ASCII and Latin-1, and
    // Inherited marks, are skipped as in HarfBuzz.
    assert_eq!(
        guessed("\u{2014}\u{201C}\u{05D0}", None),
        (HB_SCRIPT_HEBREW, HB_DIRECTION_RTL)
    );
    assert_eq!(
        guessed("\u{0301}\u{0627}", None),
        (HB_SCRIPT_ARABIC, HB_DIRECTION_RTL)
    );
    assert_eq!(guessed("\u{3042}", None), (tag(b"Hira"), HB_DIRECTION_LTR));
}

#[test]
fn guess_keeps_explicit_direction_and_sets_core_state() {
    let buffer = hb_buffer_create();
    add_utf8(buffer, "\u{0628}\u{0628}".as_bytes(), 0, -1);
    // SAFETY: created above.
    unsafe {
        hb_buffer_set_direction(buffer, crate::HB_DIRECTION_TTB);
        hb_buffer_guess_segment_properties(buffer);
    }
    with_state(buffer, |s| {
        assert_eq!(s.direction, crate::HB_DIRECTION_TTB);
        assert_eq!(s.buffer.direction(), sigilbuzz::Direction::Ttb);
        assert_eq!(s.buffer.script(), Some(UnicodeScript::Arabic));
        assert!(!s.language.is_null());
        assert_eq!(s.buffer.language().map(Language::as_str), Some("und"));
    });
    // SAFETY: created above.
    unsafe { hb_buffer_destroy(buffer) };
}
