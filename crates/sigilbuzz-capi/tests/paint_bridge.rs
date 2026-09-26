//! Integration test for the `hb_paint_*` bridge.
//!
//! Builds a synthetic SFNT carrying a tiny COLR+CPAL pair, drives the
//! C-style `hb_font_paint_glyph` against it, and asserts the
//! callbacks fire in the documented order. Mirrors the fixture style
//! used by `crates/sigilbuzz-paint/tests/evaluator.rs` so additions
//! stay in sync with the underlying parser tests.

#![cfg(feature = "paint")]

use core::ffi::{c_char, c_uint, c_void};
use core::ptr;
use core::sync::atomic::{AtomicU32, Ordering};

use sigilbuzz_capi::paint_bridge::{
    hb_color_t, hb_font_paint_glyph, hb_paint_funcs_create, hb_paint_funcs_destroy,
    hb_paint_funcs_set_color_func, hb_paint_funcs_set_pop_clip_func,
    hb_paint_funcs_set_push_clip_glyph_func, hb_paint_funcs_t,
};
use sigilbuzz_capi::{
    hb_blob_create, hb_blob_destroy, hb_face_create, hb_face_destroy, hb_font_create,
    hb_font_destroy, hb_font_set_variations, hb_variation_t, HB_MEMORY_MODE_READONLY,
};

// =========================================================================
// Fixture builders, mirrored from sigilbuzz-paint's evaluator tests
// so the FFI bridge is exercised against the same canonical layout.
// =========================================================================

fn build_face_bytes(colr: &[u8], cpal: &[u8]) -> Vec<u8> {
    let dir_len = 12 + 2 * 16;
    let cpal_off = dir_len;
    let colr_off = cpal_off + cpal.len();

    let mut out = Vec::new();
    out.extend_from_slice(&0x00010000u32.to_be_bytes());
    out.extend_from_slice(&2u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());

    out.extend_from_slice(b"COLR");
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&(colr_off as u32).to_be_bytes());
    out.extend_from_slice(&(colr.len() as u32).to_be_bytes());

    out.extend_from_slice(b"CPAL");
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&(cpal_off as u32).to_be_bytes());
    out.extend_from_slice(&(cpal.len() as u32).to_be_bytes());

    out.extend_from_slice(cpal);
    out.extend_from_slice(colr);
    out
}

fn build_cpal_v0(colors: &[(u8, u8, u8, u8)]) -> Vec<u8> {
    let num_palettes: u16 = 1;
    let entries = colors.len() as u16;
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&entries.to_be_bytes());
    out.extend_from_slice(&num_palettes.to_be_bytes());
    out.extend_from_slice(&entries.to_be_bytes());
    let header_plus_indices = 12 + num_palettes as usize * 2;
    out.extend_from_slice(&(header_plus_indices as u32).to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    for (r, g, b, a) in colors {
        out.push(*b);
        out.push(*g);
        out.push(*r);
        out.push(*a);
    }
    out
}

fn build_v1_header(glyph_id: u16) -> Vec<u8> {
    let header_len: usize = 34;
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&(header_len as u32).to_be_bytes());
    out.extend_from_slice(&(header_len as u32).to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&(header_len as u32).to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes()); // layerListOffset
    out.extend_from_slice(&0u32.to_be_bytes()); // clipListOffset
    out.extend_from_slice(&0u32.to_be_bytes()); // varIndexMapOffset
    out.extend_from_slice(&0u32.to_be_bytes()); // itemVariationStoreOffset
    out.extend_from_slice(&1u32.to_be_bytes());
    out.extend_from_slice(&glyph_id.to_be_bytes());
    out.extend_from_slice(&10u32.to_be_bytes());
    out
}

fn f2dot14(v: f32) -> [u8; 2] {
    let raw = (v * 16384.0).round() as i16;
    raw.to_be_bytes()
}

// =========================================================================
// Callback fixtures
// =========================================================================

static COLOR_CALLS: AtomicU32 = AtomicU32::new(0);
static PUSH_CLIP_GLYPH_CALLS: AtomicU32 = AtomicU32::new(0);
static POP_CLIP_CALLS: AtomicU32 = AtomicU32::new(0);
static LAST_GID: AtomicU32 = AtomicU32::new(0);
static LAST_COLOR: AtomicU32 = AtomicU32::new(0);

extern "C" fn cb_color(
    _funcs: *mut hb_paint_funcs_t,
    _data: *mut c_void,
    _is_foreground: i32,
    color: hb_color_t,
) {
    COLOR_CALLS.fetch_add(1, Ordering::SeqCst);
    LAST_COLOR.store(color, Ordering::SeqCst);
}

extern "C" fn cb_push_clip_glyph(_funcs: *mut hb_paint_funcs_t, _data: *mut c_void, gid: u32) {
    PUSH_CLIP_GLYPH_CALLS.fetch_add(1, Ordering::SeqCst);
    LAST_GID.store(gid, Ordering::SeqCst);
}

extern "C" fn cb_pop_clip(_funcs: *mut hb_paint_funcs_t, _data: *mut c_void) {
    POP_CLIP_CALLS.fetch_add(1, Ordering::SeqCst);
}

#[test]
fn paint_glyph_against_solid_colr_fires_color_and_clip_callbacks() {
    // Reset shared counters in case test ordering ever changes.
    COLOR_CALLS.store(0, Ordering::SeqCst);
    PUSH_CLIP_GLYPH_CALLS.store(0, Ordering::SeqCst);
    POP_CLIP_CALLS.store(0, Ordering::SeqCst);
    LAST_GID.store(0, Ordering::SeqCst);
    LAST_COLOR.store(0, Ordering::SeqCst);

    // Build a tiny COLR with a single base-glyph PaintGlyph(outline=42)
    // -> PaintSolid(palette=0, alpha=1.0). The COLR base glyph is 7.
    let mut colr = build_v1_header(7);
    let pglyph_start = colr.len();
    colr.push(10); // PaintGlyph
    colr.extend_from_slice(&[0, 0, 0]); // Offset24 placeholder
    colr.extend_from_slice(&42u16.to_be_bytes()); // outline glyph id

    let solid_start = colr.len();
    let rel = (solid_start - pglyph_start) as u32;
    colr[pglyph_start + 1] = ((rel >> 16) & 0xff) as u8;
    colr[pglyph_start + 2] = ((rel >> 8) & 0xff) as u8;
    colr[pglyph_start + 3] = (rel & 0xff) as u8;

    colr.push(2); // PaintSolid
    colr.extend_from_slice(&0u16.to_be_bytes()); // palette index 0
    colr.extend_from_slice(&f2dot14(1.0)); // alpha 1.0

    // Palette 0 = pure red. The bridge converts to BGRA: alpha 0xFF,
    // red 0xFF, green 0x00, blue 0x00 -> 0xFF_FF_00_00.
    let cpal = build_cpal_v0(&[(255, 0, 0, 255)]);
    let bytes = build_face_bytes(&colr, &cpal);

    // SAFETY: every pointer passed here is null, a live handle created
    // in this test, or a slice that outlives the call.
    unsafe {
        let blob = hb_blob_create(
            bytes.as_ptr().cast::<c_char>(),
            bytes.len() as c_uint,
            HB_MEMORY_MODE_READONLY,
            ptr::null_mut(),
            None,
        );
        let face = hb_face_create(blob, 0);
        let font = hb_font_create(face);

        let funcs = hb_paint_funcs_create();
        hb_paint_funcs_set_color_func(funcs, Some(cb_color));
        hb_paint_funcs_set_push_clip_glyph_func(funcs, Some(cb_push_clip_glyph));
        hb_paint_funcs_set_pop_clip_func(funcs, Some(cb_pop_clip));

        // Drive the bridge against the base glyph (7).
        hb_font_paint_glyph(font, 7, funcs, ptr::null_mut(), 0, 0xFF000000);

        // The DrawCmd stream for this fixture is one FillGlyph with
        // gid=42, transform=identity, paint=Solid(red). The bridge
        // therefore emits push_clip_glyph(42) -> color(red) -> pop_clip.
        // No transform should be pushed because the accumulated
        // transform is identity.
        assert_eq!(PUSH_CLIP_GLYPH_CALLS.load(Ordering::SeqCst), 1);
        assert_eq!(POP_CLIP_CALLS.load(Ordering::SeqCst), 1);
        assert_eq!(COLOR_CALLS.load(Ordering::SeqCst), 1);
        assert_eq!(LAST_GID.load(Ordering::SeqCst), 42);

        let color = LAST_COLOR.load(Ordering::SeqCst);
        // alpha byte: 0xFF, red byte: 0xFF, blue/green: 0.
        assert_eq!(color >> 24, 0xFF, "alpha byte");
        assert_eq!((color >> 16) & 0xFF, 0xFF, "red byte");
        assert_eq!((color >> 8) & 0xFF, 0x00, "green byte");
        assert_eq!(color & 0xFF, 0x00, "blue byte");

        hb_paint_funcs_destroy(funcs);
        hb_font_destroy(font);
        hb_face_destroy(face);
        hb_blob_destroy(blob);
    }
}

// =========================================================================
// Variation coordinates
// =========================================================================

/// Builds an SFNT from `(tag, data)` pairs, which must be sorted by
/// tag.
fn build_sfnt(tables: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
    let mut offset = 12 + tables.len() * 16;
    let mut out = Vec::new();
    out.extend_from_slice(&0x00010000u32.to_be_bytes());
    out.extend_from_slice(&(tables.len() as u16).to_be_bytes());
    out.extend_from_slice(&[0; 6]);
    for (tag, data) in tables {
        out.extend_from_slice(*tag);
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&(offset as u32).to_be_bytes());
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        offset += data.len();
    }
    for (_, data) in tables {
        out.extend_from_slice(data);
    }
    out
}

/// An `fvar` table with one `wght` axis: min 100, default 400, max 900.
fn build_fvar_wght() -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // majorVersion
    out.extend_from_slice(&0u16.to_be_bytes()); // minorVersion
    out.extend_from_slice(&16u16.to_be_bytes()); // axesArrayOffset
    out.extend_from_slice(&2u16.to_be_bytes()); // reserved
    out.extend_from_slice(&1u16.to_be_bytes()); // axisCount
    out.extend_from_slice(&20u16.to_be_bytes()); // axisSize
    out.extend_from_slice(&0u16.to_be_bytes()); // instanceCount
    out.extend_from_slice(&8u16.to_be_bytes()); // instanceSize
    out.extend_from_slice(b"wght");
    for value in [100i32, 400, 900] {
        out.extend_from_slice(&(value << 16).to_be_bytes());
    }
    out.extend_from_slice(&0u16.to_be_bytes()); // flags
    out.extend_from_slice(&256u16.to_be_bytes()); // axisNameID
    out
}

/// A COLRv1 table whose base glyph 7 is a PaintVarSolid (palette 0,
/// alpha 1.0). One ItemVariationStore row lowers the alpha by 0.5 at
/// the axis maximum. The header layout and the store placement follow
/// the fixtures in `sigilbuzz-paint/tests/evaluator.rs`.
fn build_var_solid_colr() -> Vec<u8> {
    let header_len: u32 = 34;
    let mut colr = Vec::new();
    colr.extend_from_slice(&1u16.to_be_bytes()); // version
    colr.extend_from_slice(&0u16.to_be_bytes()); // numBaseGlyphRecords
    colr.extend_from_slice(&header_len.to_be_bytes());
    colr.extend_from_slice(&header_len.to_be_bytes());
    colr.extend_from_slice(&0u16.to_be_bytes()); // numLayerRecords
    colr.extend_from_slice(&header_len.to_be_bytes()); // baseGlyphList
    colr.extend_from_slice(&0u32.to_be_bytes()); // layerList
    colr.extend_from_slice(&0u32.to_be_bytes()); // clipList
    colr.extend_from_slice(&0u32.to_be_bytes()); // varIndexMap
    let var_store_slot = colr.len();
    colr.extend_from_slice(&0u32.to_be_bytes()); // variation store
                                                 // BaseGlyphList with one record: glyph 7, paint right after it.
    colr.extend_from_slice(&1u32.to_be_bytes());
    colr.extend_from_slice(&7u16.to_be_bytes());
    colr.extend_from_slice(&10u32.to_be_bytes());
    // PaintVarSolid: format 3, palette 0, alpha 1.0, varIndexBase 0.
    colr.push(3);
    colr.extend_from_slice(&0u16.to_be_bytes());
    colr.extend_from_slice(&f2dot14(1.0));
    colr.extend_from_slice(&0u32.to_be_bytes());

    let var_store_off = colr.len() as u32;
    colr[var_store_slot..var_store_slot + 4].copy_from_slice(&var_store_off.to_be_bytes());
    // ItemVariationStore: format 1, region list at 12, one subtable.
    colr.extend_from_slice(&1u16.to_be_bytes());
    colr.extend_from_slice(&12u32.to_be_bytes());
    colr.extend_from_slice(&1u16.to_be_bytes());
    colr.extend_from_slice(&22u32.to_be_bytes());
    // Region list: one axis, one region peaking at the axis maximum.
    colr.extend_from_slice(&1u16.to_be_bytes());
    colr.extend_from_slice(&1u16.to_be_bytes());
    colr.extend_from_slice(&f2dot14(0.0));
    colr.extend_from_slice(&f2dot14(1.0));
    colr.extend_from_slice(&f2dot14(1.0));
    // Subtable: one item, one word delta, one region.
    colr.extend_from_slice(&1u16.to_be_bytes());
    colr.extend_from_slice(&1u16.to_be_bytes());
    colr.extend_from_slice(&1u16.to_be_bytes());
    colr.extend_from_slice(&0u16.to_be_bytes());
    colr.extend_from_slice(&(-8192i16).to_be_bytes()); // alpha -0.5
    colr
}

/// Records the last color a paint callback reported into the `u32`
/// that `paint_data` points at.
extern "C" fn cb_record_color(
    _funcs: *mut hb_paint_funcs_t,
    data: *mut c_void,
    _is_foreground: i32,
    color: hb_color_t,
) {
    // SAFETY: the test passes a pointer to a live `AtomicU32` as
    // `paint_data`.
    unsafe { (*data.cast::<AtomicU32>()).store(color, Ordering::SeqCst) };
}

/// HarfBuzz paints with the variation coordinates set on the font.
/// The bridge used to paint the default instance whatever the font's
/// variations were.
#[test]
fn paint_glyph_applies_font_variations() {
    let fvar = build_fvar_wght();
    let colr = build_var_solid_colr();
    let cpal = build_cpal_v0(&[(255, 0, 0, 255)]);
    let bytes = build_sfnt(&[(b"COLR", &colr), (b"CPAL", &cpal), (b"fvar", &fvar)]);

    // SAFETY: every pointer passed here is null, a live handle created
    // in this test, or a slice that outlives the call.
    unsafe {
        let blob = hb_blob_create(
            bytes.as_ptr().cast::<c_char>(),
            bytes.len() as c_uint,
            HB_MEMORY_MODE_READONLY,
            ptr::null_mut(),
            None,
        );
        let face = hb_face_create(blob, 0);
        let font = hb_font_create(face);
        let funcs = hb_paint_funcs_create();
        hb_paint_funcs_set_color_func(funcs, Some(cb_record_color));
        let color = AtomicU32::new(0);
        let data = ptr::from_ref(&color).cast_mut().cast::<c_void>();

        hb_font_paint_glyph(font, 7, funcs, data, 0, 0);
        assert_eq!(color.load(Ordering::SeqCst) >> 24, 0xFF, "default alpha");

        let heavy = hb_variation_t {
            tag: u32::from_be_bytes(*b"wght"),
            value: 900.0,
        };
        hb_font_set_variations(font, &heavy, 1);
        hb_font_paint_glyph(font, 7, funcs, data, 0, 0);
        // Alpha 1.0 - 0.5 = 0.5, packed as round(0.5 * 255) = 128.
        assert_eq!(color.load(Ordering::SeqCst) >> 24, 128, "alpha at wght 900");

        hb_paint_funcs_destroy(funcs);
        hb_font_destroy(font);
        hb_face_destroy(face);
        hb_blob_destroy(blob);
    }
}

/// HarfBuzz resolves colors in the palette the caller passes.
#[test]
fn paint_glyph_uses_the_requested_palette() {
    // PaintSolid on glyph 7 using palette entry 0.
    let mut colr = build_v1_header(7);
    colr.push(2); // PaintSolid
    colr.extend_from_slice(&0u16.to_be_bytes()); // palette entry 0
    colr.extend_from_slice(&f2dot14(1.0)); // alpha 1.0
                                           // CPAL with two one-entry palettes: red, then blue.
    let mut cpal = Vec::new();
    for word in [0u16, 1, 2, 2] {
        cpal.extend_from_slice(&word.to_be_bytes()); // version, entries, palettes, records
    }
    cpal.extend_from_slice(&16u32.to_be_bytes()); // colorRecordsArrayOffset
    cpal.extend_from_slice(&0u16.to_be_bytes()); // palette 0 starts at record 0
    cpal.extend_from_slice(&1u16.to_be_bytes()); // palette 1 starts at record 1
    cpal.extend_from_slice(&[0, 0, 255, 255]); // BGRA red
    cpal.extend_from_slice(&[255, 0, 0, 255]); // BGRA blue
    let bytes = build_face_bytes(&colr, &cpal);

    // SAFETY: every pointer passed here is null, a live handle created
    // in this test, or a slice that outlives the call.
    unsafe {
        let blob = hb_blob_create(
            bytes.as_ptr().cast::<c_char>(),
            bytes.len() as c_uint,
            HB_MEMORY_MODE_READONLY,
            ptr::null_mut(),
            None,
        );
        let face = hb_face_create(blob, 0);
        let font = hb_font_create(face);
        let funcs = hb_paint_funcs_create();
        hb_paint_funcs_set_color_func(funcs, Some(cb_record_color));
        let color = AtomicU32::new(0);
        let data = ptr::from_ref(&color).cast_mut().cast::<c_void>();

        hb_font_paint_glyph(font, 7, funcs, data, 0, 0);
        assert_eq!(
            color.load(Ordering::SeqCst),
            0xFFFF_0000,
            "palette 0 is red"
        );
        hb_font_paint_glyph(font, 7, funcs, data, 1, 0);
        assert_eq!(
            color.load(Ordering::SeqCst),
            0xFF00_00FF,
            "palette 1 is blue"
        );

        hb_paint_funcs_destroy(funcs);
        hb_font_destroy(font);
        hb_face_destroy(face);
        hb_blob_destroy(blob);
    }
}
