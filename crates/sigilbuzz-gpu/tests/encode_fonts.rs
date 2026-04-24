//! End-to-end encoding sanity sweeps over vendored fonts.
//!
//! These tests load Open Sans (TrueType / glyf) and Amiri (CFF) and
//! drive [`sigilbuzz_gpu::encode_glyph`] over a representative slice
//! of glyphs. They assert structural invariants of the encoded
//! output rather than exact numeric matches — the latter belongs in
//! the unit tests.

use sigilbuzz::Face;
use sigilbuzz_gpu::{encode_glyph, SlugGlyph, SlugOptions};

/// Open Sans is a TrueType font (glyf-based, all-quadratic outlines).
const OPENSANS_BYTES: &[u8] = include_bytes!("../../../tests/fixtures/opensans_regular.ttf");

/// Amiri is a CFF font: every glyph outline goes through the cubic
/// flattening path.
const AMIRI_BYTES: &[u8] = include_bytes!("../../../tests/fixtures/amiri_regular.ttf");

/// Common structural checks every encoded glyph must satisfy.
fn assert_glyph_invariants(g: &SlugGlyph) {
    // 1. Every segment coordinate is finite.
    for s in &g.segments {
        for c in [s.p0.x, s.p0.y, s.p1.x, s.p1.y, s.p2.x, s.p2.y] {
            assert!(c.is_finite(), "non-finite coord in segment: {c}");
        }
    }

    // 2. Sum of band segment counts equals total pool size.
    let total: u64 = g.bands.iter().map(|b| u64::from(b.segment_count)).sum();
    assert_eq!(
        total as usize,
        g.segments.len(),
        "band segment counts sum to {total} but pool has {}",
        g.segments.len()
    );

    // 3. Each band's segments stay within its y-range. We use the
    //    convex-hull approximation of segment y-extent (min/max of
    //    p0.y, p1.y, p2.y); a band may include any segment whose
    //    hull-y range overlaps the band.
    let band_count = g.bands.len() as f32;
    let band_h = if g.bbox.height() > 0.0 {
        g.bbox.height() / band_count
    } else {
        0.0
    };
    for (i, band) in g.bands.iter().enumerate() {
        let y0 = g.bbox.ymin + i as f32 * band_h;
        let y1 = y0 + band_h;
        let off = band.segment_offset as usize;
        let count = band.segment_count as usize;
        // Slice bounds must fit inside the pool.
        assert!(
            off + count <= g.segments.len(),
            "band {i} slice {off}..{} exceeds pool len {}",
            off + count,
            g.segments.len()
        );
        for s in &g.segments[off..off + count] {
            let s_lo = s.p0.y.min(s.p1.y).min(s.p2.y);
            let s_hi = s.p0.y.max(s.p1.y).max(s.p2.y);
            // 1e-2 slack absorbs single-band-height degeneracies for
            // horizontal segments lying exactly on a band boundary.
            assert!(
                s_hi >= y0 - 1e-2 && s_lo <= y1 + 1e-2,
                "band {i} y=[{y0}, {y1}] holds segment with y=[{s_lo}, {s_hi}]"
            );
        }
    }

    // 4. The bbox derived from the segments matches the bbox the
    //    encoder reported. (The encoder builds its bbox from the
    //    same segments, so this is a self-consistency check.)
    let mut recon = sigilbuzz_gpu::Bbox::empty();
    for s in &g.segments {
        recon.expand(s.p0.x, s.p0.y);
        recon.expand(s.p1.x, s.p1.y);
        recon.expand(s.p2.x, s.p2.y);
    }
    assert!(
        (recon.xmin - g.bbox.xmin).abs() < 1.0
            && (recon.ymin - g.bbox.ymin).abs() < 1.0
            && (recon.xmax - g.bbox.xmax).abs() < 1.0
            && (recon.ymax - g.bbox.ymax).abs() < 1.0,
        "reconstructed bbox {recon:?} disagrees with reported {:?}",
        g.bbox
    );
}

/// Counts encoder invocations that returned `Some` and stayed
/// invariant-clean.
fn sweep<F>(face: &Face<'_>, ids: impl IntoIterator<Item = u16>, mut on_glyph: F) -> u32
where
    F: FnMut(u16, &SlugGlyph),
{
    let opts = SlugOptions::default();
    let mut clean = 0_u32;
    for gid in ids {
        if let Some(g) = encode_glyph(face, gid, &opts) {
            assert_glyph_invariants(&g);
            on_glyph(gid, &g);
            clean += 1;
        }
    }
    clean
}

#[test]
fn open_sans_ascii_glyphs_encode_cleanly() {
    let face = Face::parse_bytes(OPENSANS_BYTES, 0).expect("parse Open Sans");
    let cmap = face.cmap().expect("cmap");

    // Walk printable ASCII via cmap so we hit real glyph ids, not
    // arbitrary indices that may correspond to .notdef etc.
    let mut clean = 0;
    let mut total_segments = 0_u64;
    let mut total_bands = 0_u64;
    for ch in 0x21_u32..=0x7E_u32 {
        let Some(c) = char::from_u32(ch) else { continue };
        let Some(gid) = cmap.glyph_id(c) else { continue };
        if let Some(g) = encode_glyph(&face, gid, &SlugOptions::default()) {
            assert_glyph_invariants(&g);
            total_segments += g.segments.len() as u64;
            total_bands += g.bands.len() as u64;
            clean += 1;
        }
    }
    // ~94 printable ASCII chars; each non-whitespace one should encode.
    // Open Sans actually encodes all 94 cleanly (every printable
    // ASCII glyph carries an outline).
    assert!(
        clean >= 90,
        "expected >=90 ASCII glyphs to encode cleanly, got {clean}"
    );
    eprintln!("Open Sans printable-ASCII encode count: {clean}");
    // Sanity: encoder produced *something*.
    assert!(total_segments > 0);
    assert!(total_bands > 0);
}

#[test]
fn open_sans_first_two_hundred_gids_encode_cleanly() {
    let face = Face::parse_bytes(OPENSANS_BYTES, 0).expect("parse Open Sans");
    // Dense gid sweep — exercises path cases beyond the cmap-mapped
    // ASCII range (e.g. ligatures, alternates).
    let clean = sweep(&face, 0_u16..200, |_, _| {});
    // Most low gids in Open Sans have outlines; .notdef + a handful
    // of empty glyphs may return None. Demand a strong majority.
    assert!(clean >= 150, "only {clean} of first 200 gids encoded");
}

#[test]
fn amiri_cff_glyphs_encode_cleanly() {
    let face = Face::parse_bytes(AMIRI_BYTES, 0).expect("parse Amiri");
    // Amiri is CFF; encoding any glyph here exercises the cubic
    // flattening path. We sweep a modest range to keep the test fast.
    let mut had_segment = false;
    let clean = sweep(&face, 0_u16..200, |_, g| {
        if !g.segments.is_empty() {
            had_segment = true;
        }
    });
    assert!(clean >= 100, "only {clean} of first 200 Amiri gids encoded");
    assert!(had_segment, "Amiri sweep produced no segments");
    eprintln!("Amiri (CFF) encode count over first 200 gids: {clean}");
}

#[test]
fn band_count_override_round_trips() {
    let face = Face::parse_bytes(OPENSANS_BYTES, 0).expect("parse Open Sans");
    let cmap = face.cmap().expect("cmap");
    let gid = cmap.glyph_id('A').expect("'A' in Open Sans");
    let opts = SlugOptions {
        band_count: Some(12),
        ..SlugOptions::default()
    };
    let g = encode_glyph(&face, gid, &opts).expect("'A' encodes");
    assert_eq!(g.bands.len(), 12);
    assert_glyph_invariants(&g);
}

#[test]
fn encoding_is_deterministic() {
    let face = Face::parse_bytes(OPENSANS_BYTES, 0).expect("parse Open Sans");
    let cmap = face.cmap().expect("cmap");
    let gid = cmap.glyph_id('g').expect("'g' in Open Sans");
    let opts = SlugOptions::default();
    let a = encode_glyph(&face, gid, &opts).unwrap();
    let b = encode_glyph(&face, gid, &opts).unwrap();
    assert_eq!(a, b);
}
