//! GSUB chained context (type 6) round trips on Amiri.

use super::*;

// ===== GSUB type 5 / 6 / 8 round-trip coverage =====
//
// Type 6 (Chained Context) is what `calt`, `clig`, and Arabic `rlig`
// ride on. Amiri uses type-6 chained-context lookups extensively for
// `rlig` / `calt`; subsetting down to a small Arabic stem and re-
// shaping verifies the rewriter keeps the chained-context lookups
// parseable and the GSUB driver correctly remaps nested
// `SubstLookupRecord` indices through the renumber map.
//
// Type 5 and Type 8 are rarer in the wild; the unit tests in `gsub.rs`
// cover them with hand-built fixtures because finding a real font that
// uses both Type 5 *and* survives a tiny subset is harder than the
// integration is worth. The Amiri walks below exercise the only
// integration path that materially matters today.

#[test]
fn amiri_subset_keeps_gsub_with_type6_lookups() {
    // Amiri's `rlig` / `calt` use type-6 chained-context lookups; a
    // subset that retains a common Arabic letter exercises the
    // rewriter's coverage-array filter path.
    let face = amiri_face();
    // U+0644 (Arabic Lam) participates in many chained rules.
    let ch = char::from_u32(0x0644).unwrap();
    let Some(gid) = face.cmap().unwrap().glyph_id(ch) else {
        return;
    };
    let input = SubsetInput {
        gids: vec![gid],
        retain_hints: false,
        drop_unhandled: true,
        retain_layout: true,
        retain_variations: false,
    };
    let out = subset(&face, &input).unwrap();
    let blob = Blob::from_vec(out.bytes);
    let subset_face = Face::parse(&blob, 0).unwrap();
    assert!(
        subset_face.record(tag::GSUB).is_some(),
        "GSUB must survive an Amiri subset that retains an Arabic letter",
    );
    let gsub = subset_face.gsub().unwrap().expect("GSUB must parse");
    let lookups = gsub.lookup_list();

    // Walk surviving lookups; for any type-6 (or extension wrapping
    // type-6) lookup we verify the subtables parse cleanly.
    let mut found_type6 = false;
    for li in 0..lookups.len() {
        let Some(lk) = lookups.get(li) else { continue };
        let lt = lk.lookup_type();
        let effective = if lt == sigilbuzz::tables::gsub::lookup_type::EXTENSION {
            lk.subtable_bytes(0).and_then(|s| {
                if s.len() >= 8 {
                    Some(u16::from_be_bytes([s[2], s[3]]))
                } else {
                    None
                }
            })
        } else {
            Some(lt)
        };
        if effective != Some(sigilbuzz::tables::gsub::lookup_type::CHAINED_CONTEXT) {
            continue;
        }
        for si in 0..lk.subtable_count() {
            let Some(sub) = lk.subtable_bytes(si) else {
                continue;
            };
            let inner = if lt == sigilbuzz::tables::gsub::lookup_type::EXTENSION {
                if sub.len() < 8 {
                    continue;
                }
                let off = u32::from_be_bytes([sub[4], sub[5], sub[6], sub[7]]) as usize;
                match sub.get(off..) {
                    Some(s) => s,
                    None => continue,
                }
            } else {
                sub
            };
            let parsed = sigilbuzz::tables::gsub::ChainContextAny::parse(inner);
            assert!(
                parsed.is_ok(),
                "rewritten type-6 subtable must parse: {:?}",
                parsed.err(),
            );
            found_type6 = true;
        }
    }
    assert!(
        found_type6,
        "expected at least one surviving type-6 chained-context lookup in the rewritten Amiri GSUB",
    );
}

#[test]
fn amiri_subset_with_type6_is_byte_deterministic() {
    let face = amiri_face();
    let ch = char::from_u32(0x0644).unwrap();
    let Some(gid) = face.cmap().unwrap().glyph_id(ch) else {
        return;
    };
    let input = SubsetInput {
        gids: vec![gid],
        retain_hints: false,
        drop_unhandled: true,
        retain_layout: true,
        retain_variations: false,
    };
    let a = subset(&face, &input).unwrap();
    let b = subset(&face, &input).unwrap();
    assert_eq!(a.bytes, b.bytes, "type-6 path must be byte-deterministic");
}

#[test]
fn amiri_arabic_subset_round_trips_through_shape() {
    // End-to-end: shape a short Arabic stem through both source and
    // subset. The subset's GSUB carries chained-context lookups
    // rewritten through the renumber map; if any nested
    // `SubstLookupRecord` lost its target lookup, shaping would either
    // crash or produce silently different output.
    let face = amiri_face();
    let cmap = face.cmap().unwrap();
    // alef + lam + meem + dal: common letters Amiri may rewrite via
    // `rlig` / `calt`.
    let chars = ['\u{0627}', '\u{0644}', '\u{0645}', '\u{062F}'];
    let mut gids: Vec<u16> = Vec::new();
    for &c in &chars {
        if let Some(g) = cmap.glyph_id(c) {
            gids.push(g);
        }
    }
    if gids.len() != chars.len() {
        return;
    }

    use sigilbuzz::{shape, Buffer, Font};
    let src_text: String = chars.iter().collect();
    let font_src = Font::new(face.clone(), 16.0);
    let mut buf = Buffer::new();
    buf.set_text(&src_text);
    let run_src = shape(&font_src, &buf, &[]).unwrap();
    let src_glyph_count = run_src.glyphs.len();

    let input = SubsetInput {
        gids,
        retain_hints: false,
        drop_unhandled: true,
        retain_layout: true,
        retain_variations: false,
    };
    let out = subset(&face, &input).unwrap();
    let blob = Blob::from_vec(out.bytes);
    let subset_face = Face::parse(&blob, 0).unwrap();
    let font_subset = Font::new(subset_face, 16.0);
    let mut buf = Buffer::new();
    buf.set_text(&src_text);
    let run_subset = shape(&font_subset, &buf, &[]).unwrap();

    // Sanity check: the subset shaping must produce the same glyph
    // count as the source. Chained-context lookups should still fire.
    assert_eq!(
        run_subset.glyphs.len(),
        src_glyph_count,
        "shape on Amiri subset should produce {} glyphs (same as source); got {}",
        src_glyph_count,
        run_subset.glyphs.len(),
    );
}
