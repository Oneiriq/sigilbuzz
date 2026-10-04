//! A partial instance at its new default, read the way renderers read
//! it: with no variations applied. HarfBuzz, FreeType and ttf-parser
//! skip the variation tables when every coordinate is at its default,
//! so a region left on the pinned axes only (peak 0 on every kept
//! axis) applies everywhere except there. Its deltas belong in the
//! default values, as HarfBuzz's and fontTools' instancers put them.
//!
//! Instancing the partial font again applies those regions at every
//! kept coordinate, the default included, so it cannot tell; these
//! tests shape the partial with rustybuzz instead.

use super::*;
use crate::sfnt;
use alloc::vec;
use rustybuzz::ttf_parser::Tag;
use rustybuzz::{Face as RbFace, UnicodeBuffer, Variation};

fn push(out: &mut Vec<u8>, values: &[u16]) {
    for v in values {
        out.extend_from_slice(&v.to_be_bytes());
    }
}

/// An ItemVariationStore over (wght, wdth) with region 0 on wght alone
/// and region 1 on wdth alone, and one subtable whose rows are `rows`
/// (one delta per region).
fn store(rows: &[[i16; 2]]) -> Vec<u8> {
    let mut out = Vec::new();
    push(&mut out, &[1, 0, 12, 1, 0, 40]);
    // Region list at 12: two axes, two regions.
    push(&mut out, &[2, 2]);
    push(&mut out, &[0, 0x4000, 0x4000, 0, 0, 0]);
    push(&mut out, &[0, 0, 0, 0, 0x4000, 0x4000]);
    // The subtable at 40: word deltas for both regions.
    push(&mut out, &[rows.len() as u16, 2, 2, 0, 1]);
    for row in rows {
        for &d in row {
            out.extend_from_slice(&d.to_be_bytes());
        }
    }
    out
}

/// A two-axis TrueType font without `gvar`: `wght` 100 to 900 (default
/// 400) and `wdth` 50 to 200 (default 100), glyph 1 mapped from `A`
/// with an advance of 500. `HVAR` adds 20 at wght 900 and 10 at wdth
/// 200; a `kern` SinglePos adds an x advance of 40 and 6 through the
/// `GDEF` store; `MVAR` moves the x height (500) by 30 and 4.
fn font() -> Vec<u8> {
    font_with_mvar(&[*b"xhgt"], &[[30, 4]])
}

/// The font of [`font`] (with an `hhea` caretSlopeRise of 1 and
/// caretOffset of -114) whose `MVAR` record `i` names `tags[i]` and
/// reads row `i` of `rows`: its deltas at wght 900 and at wdth 200.
fn font_with_mvar(tags: &[[u8; 4]], rows: &[[i16; 2]]) -> Vec<u8> {
    let mut head = vec![0u8; 54];
    head[0..4].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    head[12..16].copy_from_slice(&0x5F0F_3CF5u32.to_be_bytes());
    head[18..20].copy_from_slice(&1000u16.to_be_bytes());
    head[50..52].copy_from_slice(&1u16.to_be_bytes());
    let mut hhea = vec![0u8; 36];
    hhea[0..4].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    hhea[4..6].copy_from_slice(&800u16.to_be_bytes());
    hhea[18..20].copy_from_slice(&1u16.to_be_bytes());
    hhea[22..24].copy_from_slice(&(-114i16).to_be_bytes());
    hhea[34..36].copy_from_slice(&2u16.to_be_bytes());
    let mut maxp = 0x0000_5000u32.to_be_bytes().to_vec();
    push(&mut maxp, &[2]);
    let mut hmtx = Vec::new();
    push(&mut hmtx, &[500, 0, 500, 0]);
    let mut loca = Vec::new();
    for _ in 0..3 {
        loca.extend_from_slice(&0u32.to_be_bytes());
    }
    let glyf = vec![0u8; 4];

    // cmap: format 4, A -> glyph 1.
    let mut sub = Vec::new();
    push(&mut sub, &[4, 32, 0, 4, 4, 1, 0]);
    push(&mut sub, &[0x41, 0xFFFF, 0, 0x41, 0xFFFF]);
    push(&mut sub, &[1u16.wrapping_sub(0x41), 1, 0, 0]);
    let mut cmap = Vec::new();
    push(&mut cmap, &[0, 1, 3, 1, 0, 12]);
    cmap.extend_from_slice(&sub);

    let mut fvar = Vec::new();
    push(&mut fvar, &[1, 0, 16, 2, 2, 20, 0, 0]);
    for (tag, min, default, max) in [(*b"wght", 100i32, 400, 900), (*b"wdth", 50, 100, 200)] {
        fvar.extend_from_slice(&tag);
        for v in [min, default, max] {
            fvar.extend_from_slice(&(v << 16).to_be_bytes());
        }
        push(&mut fvar, &[0, 256]);
    }

    // HVAR: no maps, so glyph g reads row (0, g).
    let mut hvar = Vec::new();
    push(&mut hvar, &[1, 0, 0, 20, 0, 0, 0, 0, 0, 0]);
    hvar.extend_from_slice(&store(&[[0, 0], [20, 10]]));

    // OS/2 version 2 with sxHeight 500, and MVAR moving it.
    let mut os2 = vec![0u8; 96];
    os2[0..2].copy_from_slice(&2u16.to_be_bytes());
    os2[86..88].copy_from_slice(&500u16.to_be_bytes());
    let mut mvar = Vec::new();
    let count = tags.len() as u16;
    push(&mut mvar, &[1, 0, 0, 8, count, 12 + 8 * count]);
    for (i, tag) in tags.iter().enumerate() {
        mvar.extend_from_slice(tag);
        push(&mut mvar, &[0, i as u16]);
    }
    mvar.extend_from_slice(&store(rows));

    // GDEF 1.3 with only the store.
    let mut gdef = Vec::new();
    push(&mut gdef, &[1, 3, 0, 0, 0, 0, 0, 0, 18]);
    gdef.extend_from_slice(&store(&[[40, 6]]));

    // GPOS: DFLT -> kern -> one SinglePos format 1 on glyph 1 whose
    // x advance (0) varies through row (0, 0).
    let mut gpos = Vec::new();
    push(&mut gpos, &[1, 0, 10, 30, 44]);
    // ScriptList at 10: DFLT at +8; Script: default LangSys at +4.
    push(&mut gpos, &[1]);
    gpos.extend_from_slice(b"DFLT");
    push(&mut gpos, &[8, 4, 0, 0, 0xFFFF, 1, 0]);
    // FeatureList at 30: kern at +8; Feature: one lookup.
    push(&mut gpos, &[1]);
    gpos.extend_from_slice(b"kern");
    push(&mut gpos, &[8, 0, 1, 0]);
    // LookupList at 44: one lookup at +4, type 1, one subtable at +8.
    push(&mut gpos, &[1, 4, 1, 0, 1, 8]);
    // SinglePos format 1 at 56: coverage at +12, valueFormat XAdvance |
    // XAdvDevice, XAdvance 0, device at +18.
    push(&mut gpos, &[1, 12, 0x0044, 0, 18, 0]);
    push(&mut gpos, &[1, 1, 1]);
    push(&mut gpos, &[0, 0, 0x8000]);

    sfnt::build(
        0x0001_0000,
        &[
            (tag::HEAD, head),
            (tag::HHEA, hhea),
            (tag::MAXP, maxp),
            (tag::HMTX, hmtx),
            (tag::LOCA, loca),
            (tag::GLYF, glyf),
            (*b"cmap", cmap),
            (*b"OS/2", os2),
            (tag::FVAR, fvar),
            (tag::HVAR, hvar),
            (tag::MVAR, mvar),
            (tag::GDEF, gdef),
            (tag::GPOS, gpos),
        ],
    )
}

/// The x advance rustybuzz shapes `A` with, at the user coordinates
/// `axes` (none: the font's default, with no variations applied).
fn advance(font: &[u8], axes: &[(&[u8; 4], f32)]) -> i32 {
    let mut face = RbFace::from_slice(font, 0).expect("rustybuzz parses");
    let variations: Vec<Variation> = axes
        .iter()
        .map(|(tag, value)| Variation {
            tag: Tag::from_bytes(tag),
            value: *value,
        })
        .collect();
    face.set_variations(&variations);
    let mut buffer = UnicodeBuffer::new();
    buffer.push_str("A");
    let out = rustybuzz::shape(&face, &[], buffer);
    out.glyph_positions()[0].x_advance
}

/// Pins wght at 900 and keeps wdth.
fn pinned() -> Vec<u8> {
    let source = font();
    let face = Face::parse_bytes(&source, 0).unwrap();
    let input = InstanceInput {
        coords: vec![1.0, 0.0],
        drop_var_tables: true,
        axis_pins: vec![AxisPin::Pin, AxisPin::Keep],
    };
    instance(&face, &input).expect("partial instance").bytes
}

#[test]
fn the_source_varies_as_built() {
    let source = font();
    assert_eq!(advance(&source, &[]), 500);
    assert_eq!(advance(&source, &[(b"wght", 900.0)]), 560);
    assert_eq!(advance(&source, &[(b"wght", 900.0), (b"wdth", 200.0)]), 576);
}

#[test]
fn a_partial_instance_shapes_at_its_default_like_the_source_at_the_pin() {
    let out = pinned();
    // No variations applied: hmtx and the GPOS value hold the wght
    // deltas.
    assert_eq!(
        advance(&out, &[]),
        560,
        "HVAR 20 and GPOS 40 at the default"
    );
    // The kept axis still adds its own.
    assert_eq!(advance(&out, &[(b"wdth", 200.0)]), 576);
    assert_eq!(advance(&out, &[(b"wdth", 150.0)]), 568);
}

#[test]
fn a_partial_instance_moves_mvar_fields_to_the_pin() {
    let out = pinned();
    let face = Face::parse_bytes(&out, 0).unwrap();
    let os2 = face.table_bytes(*b"OS/2").unwrap();
    assert_eq!(u16::from_be_bytes([os2[86], os2[87]]), 530, "500 + 30");
    // MVAR keeps the wdth region alone.
    let mvar = face.mvar().unwrap().expect("MVAR stays");
    let store = mvar.variation_store().unwrap();
    assert_eq!(store.region_count(), 1);
    assert_eq!(store.delta(0, 0, &[1.0]), 4.0);
}

#[test]
fn instances_move_the_caret_fields_mvar_varies() {
    // MVAR varies hhea's caretOffset (-114), caretSlopeRun (0) and
    // caretSlopeRise (1): by -3, 500 and 1998 at wght 900, and by 5, 7
    // and 0 at wdth 200.
    let source = font_with_mvar(
        &[*b"hcof", *b"hcrn", *b"hcrs", *b"xhgt"],
        &[[-3, 5], [500, 7], [1998, 0], [30, 4]],
    );
    let face = Face::parse_bytes(&source, 0).unwrap();
    let carets = |bytes: &[u8]| {
        let face = Face::parse_bytes(bytes, 0).unwrap();
        let hhea = face.table_bytes(tag::HHEA).unwrap();
        [18, 20, 22].map(|off| i16::from_be_bytes([hhea[off], hhea[off + 1]]))
    };

    // Partial, wght pinned at 650 (0.5), wdth kept: the wght deltas,
    // halved, move into the defaults; -1.5 rounds half up to -1.
    let input = InstanceInput {
        coords: vec![0.5, 0.0],
        drop_var_tables: true,
        axis_pins: vec![AxisPin::Pin, AxisPin::Keep],
    };
    let out = instance(&face, &input).expect("partial instance").bytes;
    assert_eq!(carets(&out), [1 + 999, 250, -114 - 1]);
    // MVAR keeps the wdth deltas.
    let partial = Face::parse_bytes(&out, 0).unwrap();
    let mvar = partial.mvar().unwrap().expect("MVAR stays");
    assert_eq!(mvar.metric_delta(*b"hcrn", &[1.0]), Some(7.0));
    assert_eq!(mvar.metric_delta(*b"hcof", &[1.0]), Some(5.0));

    // Full, at wght 650 and wdth 200: 3.5 (-1.5 + 5) rounds to 4.
    let input = InstanceInput {
        coords: vec![0.5, 1.0],
        drop_var_tables: true,
        axis_pins: Vec::new(),
    };
    let out = instance(&face, &input).expect("full instance").bytes;
    assert_eq!(carets(&out), [1 + 999, 250 + 7, -114 + 4]);
}

#[test]
fn no_store_keeps_a_region_on_the_pinned_axes_only() {
    // A region with no peak on any kept axis would apply at every kept
    // coordinate but the default: every store drops it.
    let out = pinned();
    let face = Face::parse_bytes(&out, 0).unwrap();
    let hvar = face.table_bytes(tag::HVAR).unwrap();
    let gdef = face.table_bytes(tag::GDEF).unwrap();
    let mvar = face.table_bytes(tag::MVAR).unwrap();
    let at = |t: &[u8], slot: usize| {
        u32::from_be_bytes([t[slot], t[slot + 1], t[slot + 2], t[slot + 3]]) as usize
    };
    let stores = [
        &hvar[at(hvar, 4)..],
        &gdef[at(gdef, 14)..],
        &mvar[usize::from(u16::from_be_bytes([mvar[10], mvar[11]]))..],
    ];
    for store in stores {
        let parsed = sigilbuzz::tables::variation_store::ItemVariationStore::parse(store).unwrap();
        assert_eq!(parsed.axis_count(), 1);
        for r in 0..parsed.region_count() {
            // Every region left peaks on the kept axis.
            assert_eq!(parsed.region_scalar(r, &[0.0]), Some(0.0), "region {r}");
        }
    }
}
