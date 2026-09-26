//! Variable paints: PaintVar* at default coords, ItemVariationStore
//! delta application and DeltaSetIndexMap indirection.

use sigilbuzz::Face;
use sigilbuzz_paint::{evaluate_at_coords, DrawCmd, PaintSource};

use crate::fixtures::{build_cpal_v0, build_face_bytes, build_v1_header, f2dot14};

// =========================================================================
// 7. PaintVar* under default coords matches the static path.
// =========================================================================

#[test]
fn var_solid_with_empty_coords_is_identity() {
    // PaintVarSolid: format=3, paletteIndex, alpha, varIndexBase.
    // With empty coords no deltas apply. Output must match the
    // PaintSolid case byte-for-byte aside from variant.
    let mut colr = build_v1_header(7);
    colr.push(3); // PaintVarSolid
    colr.extend_from_slice(&0u16.to_be_bytes()); // paletteIndex
    colr.extend_from_slice(&f2dot14(1.0));
    colr.extend_from_slice(&0xFFFF_FFFFu32.to_be_bytes()); // var sentinel

    let cpal = build_cpal_v0(&[(64, 128, 192, 255)]);
    let bytes = build_face_bytes(&colr, &cpal);
    let face = Face::parse_bytes(&bytes, 0).unwrap();
    let cmds = evaluate_at_coords(&face, 7, &[]);
    assert_eq!(cmds.len(), 1);
    match &cmds[0] {
        DrawCmd::FillGlyph {
            paint: PaintSource::Solid { color: c, .. },
            ..
        } => {
            assert!((c.r - 64.0 / 255.0).abs() < 1e-6);
            assert!((c.g - 128.0 / 255.0).abs() < 1e-6);
            assert!((c.b - 192.0 / 255.0).abs() < 1e-6);
            assert!((c.a - 1.0).abs() < 1e-6);
        }
        other => panic!("unexpected {other:?}"),
    }
}

// =========================================================================
// IVS / DeltaSetIndexMap fixture helpers (used by tests 12+).
// =========================================================================

extern crate alloc;

/// Builds an `ItemVariationStore` with `axis_count` axes, the supplied
/// regions, and a *single* outer subtable carrying `delta_sets`. Each
/// inner row is `regions.len()` deltas wide, packed as int16 (no
/// LONG_WORDS). Returns the full IVS byte blob. The caller embeds it
/// at the COLR `varStoreOffset` it picks (or in GDEF's `itemVarStore`).
fn build_ivs(
    axis_count: u16,
    regions: &[Vec<(f32, f32, f32)>],
    delta_sets: &[Vec<i16>],
) -> Vec<u8> {
    let region_count = regions.len() as u16;
    let item_count = delta_sets.len() as u16;
    // Header: u16 format + u32 regionListOff + u16 subtableCount +
    // u32 subtableOffsets[1] = 12 bytes.
    let header_len: u32 = 12;
    let region_list_size = 4 + region_count as u32 * axis_count as u32 * 6;
    let region_list_off = header_len;
    let subtable_off = header_len + region_list_size;

    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // format
    out.extend_from_slice(&region_list_off.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes()); // subtable count
    out.extend_from_slice(&subtable_off.to_be_bytes());

    // Region list.
    out.extend_from_slice(&axis_count.to_be_bytes());
    out.extend_from_slice(&region_count.to_be_bytes());
    for region in regions {
        assert_eq!(region.len(), axis_count as usize);
        for (s, p, e) in region {
            out.extend_from_slice(&f2dot14(*s));
            out.extend_from_slice(&f2dot14(*p));
            out.extend_from_slice(&f2dot14(*e));
        }
    }

    // Subtable: itemCount, wordDeltaCount = region_count (every delta
    // is a wide short-word so the writer is uniform), regionIndexCount,
    // regionIndexes, then the delta rows packed as int16.
    out.extend_from_slice(&item_count.to_be_bytes());
    out.extend_from_slice(&region_count.to_be_bytes());
    out.extend_from_slice(&region_count.to_be_bytes());
    for ri in 0..region_count {
        out.extend_from_slice(&ri.to_be_bytes());
    }
    for set in delta_sets {
        assert_eq!(set.len(), region_count as usize);
        for d in set {
            out.extend_from_slice(&d.to_be_bytes());
        }
    }
    out
}

/// Builds a multi-base-glyph COLRv1 table. Each entry of `paints` is
/// the raw bytes of one paint subtree; the helper places them
/// contiguously after the BaseGlyphList and patches the
/// `BaseGlyphPaintRecord` offsets. `var_store` and `index_map` are
/// appended to the COLR data when non-empty and their absolute offsets
/// recorded as `itemVariationStoreOffset` and `varIndexMapOffset` in
/// the v1 header.
fn build_v1_multi_colr(paints: &[(u16, Vec<u8>)], var_store: &[u8]) -> Vec<u8> {
    build_v1_multi_colr_with_map(paints, var_store, &[])
}

fn build_v1_multi_colr_with_map(
    paints: &[(u16, Vec<u8>)],
    var_store: &[u8],
    index_map: &[u8],
) -> Vec<u8> {
    let header_len: u32 = 34; // 14 (v0) + 20 (v1 appendix, 5 u32)

    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // version
    out.extend_from_slice(&0u16.to_be_bytes()); // v0 numBase
    out.extend_from_slice(&header_len.to_be_bytes()); // baseGlyphRecordsOff
    out.extend_from_slice(&header_len.to_be_bytes()); // layerRecordsOff
    out.extend_from_slice(&0u16.to_be_bytes()); // numLayer
    out.extend_from_slice(&header_len.to_be_bytes()); // baseGlyphListOff
    out.extend_from_slice(&0u32.to_be_bytes()); // layerListOff
    out.extend_from_slice(&0u32.to_be_bytes()); // clipListOff
    let index_map_slot = out.len();
    out.extend_from_slice(&0u32.to_be_bytes()); // varIndexMapOff (filled in below)
    let var_store_slot = out.len();
    out.extend_from_slice(&0u32.to_be_bytes()); // varStoreOff (filled in below)

    // BaseGlyphList header.
    out.extend_from_slice(&(paints.len() as u32).to_be_bytes());
    let record_slots = out.len();
    for (gid, _) in paints {
        out.extend_from_slice(&gid.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes()); // patched below
    }

    for (i, (_, bytes)) in paints.iter().enumerate() {
        let rel = (out.len() as u32) - header_len;
        let slot = record_slots + i * 6 + 2;
        out[slot..slot + 4].copy_from_slice(&rel.to_be_bytes());
        out.extend_from_slice(bytes);
    }

    if !var_store.is_empty() {
        let off = out.len() as u32;
        out[var_store_slot..var_store_slot + 4].copy_from_slice(&off.to_be_bytes());
        out.extend_from_slice(var_store);
    }
    if !index_map.is_empty() {
        let off = out.len() as u32;
        out[index_map_slot..index_map_slot + 4].copy_from_slice(&off.to_be_bytes());
        out.extend_from_slice(index_map);
    }
    out
}

// =========================================================================
// 12. ItemVariationStore: full delta-application path. Synthesizes an
//     IVS embedded at COLR's varStoreOffset and asserts the three
//     PaintVar* nodes we care about (Solid alpha, LinearGradient stop
//     offset, Translate dx/dy) interpolate linearly across the axis.
// =========================================================================

/// Encodes the test's three paint blobs at the `(gid, paint_bytes)`
/// pairs the multi-glyph COLR builder expects. All three reference
/// the same IVS subtable (outer = 0); inner indices partition the
/// subtable rows by paint:
///   inner 0   -> PaintVarSolid alpha
///   inner 1-2 -> PaintVarLinearGradient stop[1] offset / alpha
///   inner 3-4 -> PaintVarTranslate dx / dy
fn build_ivs_test_paints() -> Vec<(u16, Vec<u8>)> {
    // Glyph 1: PaintVarSolid (format 3), palette 0, alpha 1.0,
    // var_index_base = 0 (outer 0, inner 0).
    let mut p_solid = Vec::new();
    p_solid.push(3u8);
    p_solid.extend_from_slice(&0u16.to_be_bytes()); // palette index
    p_solid.extend_from_slice(&f2dot14(1.0)); // alpha base
    p_solid.extend_from_slice(&0x0000_0000u32.to_be_bytes()); // var_index_base

    // Glyph 2: PaintVarLinearGradient (format 5) wrapping a VarColorLine
    // with two stops; stop[1] carries varIndexBase = 1 so the stop's
    // offset/alpha pull from inner 1 / inner 2 of the IVS.
    let mut p_lin = Vec::new();
    p_lin.push(5u8);
    p_lin.extend_from_slice(&[0, 0, 0]); // Offset24 colorLine, patched below.
    p_lin.extend_from_slice(&0i16.to_be_bytes()); // x0
    p_lin.extend_from_slice(&0i16.to_be_bytes()); // y0
    p_lin.extend_from_slice(&100i16.to_be_bytes()); // x1
    p_lin.extend_from_slice(&0i16.to_be_bytes()); // y1
    p_lin.extend_from_slice(&0i16.to_be_bytes()); // x2
    p_lin.extend_from_slice(&100i16.to_be_bytes()); // y2
    p_lin.extend_from_slice(&u32::MAX.to_be_bytes()); // paint var_index_base = none
    let cl_rel = p_lin.len() as u32;
    p_lin[1] = ((cl_rel >> 16) & 0xff) as u8;
    p_lin[2] = ((cl_rel >> 8) & 0xff) as u8;
    p_lin[3] = (cl_rel & 0xff) as u8;
    // VarColorLine: u8 extend, u16 numStops, VarColorStop[2] (10 bytes each).
    p_lin.push(0u8); // extend = Pad
    p_lin.extend_from_slice(&2u16.to_be_bytes());
    // Stop 0: offset 0.0, palette 0, alpha 1.0, no variation.
    p_lin.extend_from_slice(&f2dot14(0.0));
    p_lin.extend_from_slice(&0u16.to_be_bytes());
    p_lin.extend_from_slice(&f2dot14(1.0));
    p_lin.extend_from_slice(&u32::MAX.to_be_bytes());
    // Stop 1: offset 1.0, palette 1, alpha 1.0, varIndexBase = 1.
    p_lin.extend_from_slice(&f2dot14(1.0));
    p_lin.extend_from_slice(&1u16.to_be_bytes());
    p_lin.extend_from_slice(&f2dot14(1.0));
    p_lin.extend_from_slice(&0x0000_0001u32.to_be_bytes());

    // Glyph 3: PaintVarTranslate (format 15) over a PaintSolid leaf.
    // Translate base = (10, 20); inner 3/4 carry dx/dy deltas.
    let mut p_tr = Vec::new();
    p_tr.push(15u8);
    p_tr.extend_from_slice(&[0, 0, 0]); // Offset24 child paint, patched below.
    p_tr.extend_from_slice(&10i16.to_be_bytes()); // dx base
    p_tr.extend_from_slice(&20i16.to_be_bytes()); // dy base
    p_tr.extend_from_slice(&0x0000_0003u32.to_be_bytes()); // var_index_base = 3
    let child_rel = p_tr.len() as u32;
    p_tr[1] = ((child_rel >> 16) & 0xff) as u8;
    p_tr[2] = ((child_rel >> 8) & 0xff) as u8;
    p_tr[3] = (child_rel & 0xff) as u8;
    p_tr.push(2u8); // child = PaintSolid
    p_tr.extend_from_slice(&2u16.to_be_bytes()); // palette 2
    p_tr.extend_from_slice(&f2dot14(1.0));

    alloc::vec![(1u16, p_solid), (2u16, p_lin), (3u16, p_tr)]
}

/// Builds the IVS used by every IVS test in this file. One axis, two
/// regions:
///   region 0 -> always-1 ("default" / always-on bias row)
///   region 1 -> triangular (0, 1, 1), peaks at axis = 1
/// Five delta sets, indexed by inner index. Region 0 always carries 0
/// so only region 1 contributes; this means the delta is exactly
/// `region_1_value * coord` for any coord in `[0, 1]`.
fn build_ivs_test_store() -> Vec<u8> {
    build_ivs(
        1,
        &[
            alloc::vec![(0.0, 0.0, 0.0)], // axis-not-used -> scalar 1
            alloc::vec![(0.0, 1.0, 1.0)], // peaks at coord = 1
        ],
        &[
            alloc::vec![0, -8192], // inner 0: VarSolid alpha (F2DOT14 -0.5)
            alloc::vec![0, 4096],  // inner 1: VarLinGrad stop[1] offset (+0.25)
            alloc::vec![0, 0],     // inner 2: VarLinGrad stop[1] alpha (+0.0)
            alloc::vec![0, 5],     // inner 3: VarTranslate dx (+5 design units)
            alloc::vec![0, -3],    // inner 4: VarTranslate dy (-3 design units)
        ],
    )
}

/// Helper: build the IVS face once, exercise three paint variants
/// at a given axis coord, and return `(solid_alpha, stop1_offset,
/// translated_origin)`. Each tuple element is what the evaluator
/// emitted for the corresponding paint subtree. Failures therefore
/// pinpoint which Var* path stopped applying its delta.
fn evaluate_ivs_at(coord_input: &[f32]) -> (f32, f32, (f32, f32)) {
    let var_store = build_ivs_test_store();
    let paints = build_ivs_test_paints();
    let colr = build_v1_multi_colr(&paints, &var_store);
    let cpal = build_cpal_v0(&[(255, 255, 255, 255), (255, 0, 0, 255), (0, 255, 0, 255)]);
    let bytes = build_face_bytes(&colr, &cpal);
    let face = Face::parse_bytes(&bytes, 0).expect("face parses");

    let solid_cmds = evaluate_at_coords(&face, 1, coord_input);
    let solid_alpha = match solid_cmds.as_slice() {
        [DrawCmd::FillGlyph {
            paint: PaintSource::Solid { color: c, .. },
            ..
        }] => c.a,
        other => panic!("solid: unexpected {other:?}"),
    };

    let lin_cmds = evaluate_at_coords(&face, 2, coord_input);
    let stop1_offset = match lin_cmds.as_slice() {
        [DrawCmd::FillGlyph {
            paint: PaintSource::Gradient(g),
            ..
        }] => {
            assert_eq!(g.stops.len(), 2);
            g.stops[1].offset
        }
        other => panic!("lin: unexpected {other:?}"),
    };

    let tr_cmds = evaluate_at_coords(&face, 3, coord_input);
    let translate = match tr_cmds.as_slice() {
        [DrawCmd::FillGlyph { transform, .. }] => transform.apply(0.0, 0.0),
        other => panic!("tr: unexpected {other:?}"),
    };

    (solid_alpha, stop1_offset, translate)
}

#[test]
fn ivs_at_axis_zero_returns_unmodified_base_values() {
    // Coord 0.0 -> region 1 scalar = 0 -> no deltas applied.
    let (alpha, offset, (dx, dy)) = evaluate_ivs_at(&[0.0]);
    assert!((alpha - 1.0).abs() < 1e-4, "alpha was {alpha}");
    assert!((offset - 1.0).abs() < 1e-4, "offset was {offset}");
    assert!((dx - 10.0).abs() < 1e-4, "dx was {dx}");
    assert!((dy - 20.0).abs() < 1e-4, "dy was {dy}");
}

#[test]
fn ivs_at_axis_one_applies_full_deltas() {
    // Coord 1.0 -> region 1 scalar = 1 -> full deltas applied.
    let (alpha, offset, (dx, dy)) = evaluate_ivs_at(&[1.0]);
    // Alpha base 1.0 + delta of -0.5 (raw -8192 / 16384) = 0.5.
    assert!((alpha - 0.5).abs() < 1e-3, "alpha was {alpha}");
    // Stop offset base 1.0 + delta of 0.25 (raw 4096 / 16384) = 1.25.
    assert!((offset - 1.25).abs() < 1e-3, "offset was {offset}");
    // Translate base (10, 20) + delta (5, -3) = (15, 17).
    assert!((dx - 15.0).abs() < 1e-3, "dx was {dx}");
    assert!((dy - 17.0).abs() < 1e-3, "dy was {dy}");
}

#[test]
fn ivs_at_half_axis_interpolates_linearly() {
    // Coord 0.5 -> region 1 scalar = 0.5 -> half deltas applied.
    let (alpha, offset, (dx, dy)) = evaluate_ivs_at(&[0.5]);
    // Alpha 1.0 + (-0.5 * 0.5) = 0.75.
    assert!((alpha - 0.75).abs() < 1e-3, "alpha was {alpha}");
    // Stop offset 1.0 + (0.25 * 0.5) = 1.125.
    assert!((offset - 1.125).abs() < 1e-3, "offset was {offset}");
    // Translate (10, 20) + (5 * 0.5, -3 * 0.5) = (12.5, 18.5).
    assert!((dx - 12.5).abs() < 1e-3, "dx was {dx}");
    assert!((dy - 18.5).abs() < 1e-3, "dy was {dy}");
}

// =========================================================================
// 13. DeltaSetIndexMap indirection. Builds the same paint set as the
//     IVS test but moves every paint's `var_index_base` to a flat
//     index that resolves through the COLR header's DeltaSetIndexMap.
//     The map permutes the IVS rows so that asserting on the output
//     values proves the indirection actually fired.
// =========================================================================

/// Builds a v1.3 GDEF whose only populated subtable is `ivs`, and
/// whose header is followed by `trailer` (bytes a lenient reader
/// might take for more header fields).
fn build_gdef_v13(ivs: &[u8], trailer: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // major
    out.extend_from_slice(&3u16.to_be_bytes()); // minor
    out.extend_from_slice(&0u16.to_be_bytes()); // glyphClassDefOff
    out.extend_from_slice(&0u16.to_be_bytes()); // attachListOff
    out.extend_from_slice(&0u16.to_be_bytes()); // ligCaretListOff
    out.extend_from_slice(&0u16.to_be_bytes()); // markAttachClassDefOff
    out.extend_from_slice(&0u16.to_be_bytes()); // markGlyphSetsDefOff
    let ivs_slot = out.len();
    out.extend_from_slice(&0u32.to_be_bytes()); // itemVarStoreOff (patched)
    out.extend_from_slice(trailer);
    let ivs_off = out.len() as u32;
    out[ivs_slot..ivs_slot + 4].copy_from_slice(&ivs_off.to_be_bytes());
    out.extend_from_slice(ivs);
    out
}

/// Three-table SFNT (COLR + CPAL + GDEF). Records ordered alphabetically
/// by tag: 'C' < 'G' so COLR < CPAL < GDEF.
fn build_face_bytes_with_gdef(colr: &[u8], cpal: &[u8], gdef: &[u8]) -> Vec<u8> {
    let dir_len = 12 + 3 * 16;
    let cpal_off = dir_len;
    let colr_off = cpal_off + cpal.len();
    let gdef_off = colr_off + colr.len();

    let mut out = Vec::new();
    out.extend_from_slice(&0x00010000u32.to_be_bytes());
    out.extend_from_slice(&3u16.to_be_bytes());
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

    out.extend_from_slice(b"GDEF");
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&(gdef_off as u32).to_be_bytes());
    out.extend_from_slice(&(gdef.len() as u32).to_be_bytes());

    out.extend_from_slice(cpal);
    out.extend_from_slice(colr);
    out.extend_from_slice(gdef);
    out
}

/// Builds the indirection-test paint suite. Every variable field uses
/// `var_index_base + field_index` as a *flat* index into the COLR
/// DeltaSetIndexMap, which then yields the real `(outer, inner)`
/// pair. The map below is laid out so the same per-paint flat indices
/// (100 / 200+201 / 300+301) hit the same IVS rows the previous test
/// used. When this test passes the indirection round-trip is
/// correct end to end.
fn build_indirection_test_paints() -> Vec<(u16, Vec<u8>)> {
    // PaintVarSolid: var_index_base = 100 (flat). Field 0 goes to map
    // entry 100, which we'll point at IVS (0, 0), the VarSolid alpha
    // row.
    let mut p_solid = Vec::new();
    p_solid.push(3u8);
    p_solid.extend_from_slice(&0u16.to_be_bytes());
    p_solid.extend_from_slice(&f2dot14(1.0));
    p_solid.extend_from_slice(&100u32.to_be_bytes());

    // PaintVarLinearGradient with a VarColorLine. The paint itself has
    // no variation (var_index_base = MAX); the *stop's* varIndexBase
    // = 200 routes through the map at flat index 200/201 -> IVS rows
    // (0, 1) and (0, 2).
    let mut p_lin = Vec::new();
    p_lin.push(5u8);
    p_lin.extend_from_slice(&[0, 0, 0]);
    p_lin.extend_from_slice(&0i16.to_be_bytes());
    p_lin.extend_from_slice(&0i16.to_be_bytes());
    p_lin.extend_from_slice(&100i16.to_be_bytes());
    p_lin.extend_from_slice(&0i16.to_be_bytes());
    p_lin.extend_from_slice(&0i16.to_be_bytes());
    p_lin.extend_from_slice(&100i16.to_be_bytes());
    p_lin.extend_from_slice(&u32::MAX.to_be_bytes());
    let cl_rel = p_lin.len() as u32;
    p_lin[1] = ((cl_rel >> 16) & 0xff) as u8;
    p_lin[2] = ((cl_rel >> 8) & 0xff) as u8;
    p_lin[3] = (cl_rel & 0xff) as u8;
    p_lin.push(0u8);
    p_lin.extend_from_slice(&2u16.to_be_bytes());
    p_lin.extend_from_slice(&f2dot14(0.0));
    p_lin.extend_from_slice(&0u16.to_be_bytes());
    p_lin.extend_from_slice(&f2dot14(1.0));
    p_lin.extend_from_slice(&u32::MAX.to_be_bytes());
    p_lin.extend_from_slice(&f2dot14(1.0));
    p_lin.extend_from_slice(&1u16.to_be_bytes());
    p_lin.extend_from_slice(&f2dot14(1.0));
    p_lin.extend_from_slice(&200u32.to_be_bytes());

    // PaintVarTranslate over a Solid. var_index_base = 300; fields
    // 0/1 (dx/dy) hit map entries 300/301 -> IVS rows (0, 3) / (0, 4).
    let mut p_tr = Vec::new();
    p_tr.push(15u8);
    p_tr.extend_from_slice(&[0, 0, 0]);
    p_tr.extend_from_slice(&10i16.to_be_bytes());
    p_tr.extend_from_slice(&20i16.to_be_bytes());
    p_tr.extend_from_slice(&300u32.to_be_bytes());
    let child_rel = p_tr.len() as u32;
    p_tr[1] = ((child_rel >> 16) & 0xff) as u8;
    p_tr[2] = ((child_rel >> 8) & 0xff) as u8;
    p_tr[3] = (child_rel & 0xff) as u8;
    p_tr.push(2u8);
    p_tr.extend_from_slice(&2u16.to_be_bytes());
    p_tr.extend_from_slice(&f2dot14(1.0));

    alloc::vec![(1u16, p_solid), (2u16, p_lin), (3u16, p_tr)]
}

/// Builds a DeltaSetIndexMap (format 1, u32 mapCount) that maps the
/// flat indices our paints reference (100, 200, 201, 300, 301) into
/// IVS `(outer, inner)` pairs. Padding entries before the first one
/// we care about resolve to `(0, 0)`, which is harmless because
/// they're never consulted.
fn build_indirection_index_map() -> Vec<u8> {
    // Format 1 (u32 mapCount), entryFormat: 1 byte per entry, inner
    // bits = 4 (so outer occupies the upper 4 bits, sufficient for
    // outer = 0 and inner up to 15). entryFormat = 0b0000_0011.
    let map_count = 302u32;
    let mut out = Vec::new();
    out.push(1u8);
    out.push(0b0000_0011u8);
    out.extend_from_slice(&map_count.to_be_bytes());
    let mut entries = alloc::vec![0u8; map_count as usize];
    let pack = |outer: u8, inner: u8| -> u8 { (outer << 4) | (inner & 0x0F) };
    entries[100] = pack(0, 0); // VarSolid alpha -> IVS (0, 0)
    entries[200] = pack(0, 1); // stop[1] offset -> IVS (0, 1)
    entries[201] = pack(0, 2); // stop[1] alpha  -> IVS (0, 2)
    entries[300] = pack(0, 3); // VarTranslate dx -> IVS (0, 3)
    entries[301] = pack(0, 4); // VarTranslate dy -> IVS (0, 4)
    out.extend_from_slice(&entries);
    out
}

/// Reads the three indirection-test values at `coords`: the solid's
/// alpha, stop 1's offset, and the translated origin.
fn indirection_values(face: &Face<'_>, coords: &[f32]) -> (f32, f32, (f32, f32)) {
    let solid_alpha = match evaluate_at_coords(face, 1, coords).as_slice() {
        [DrawCmd::FillGlyph {
            paint: PaintSource::Solid { color: c, .. },
            ..
        }] => c.a,
        other => panic!("solid: unexpected {other:?}"),
    };
    let stop1_offset = match evaluate_at_coords(face, 2, coords).as_slice() {
        [DrawCmd::FillGlyph {
            paint: PaintSource::Gradient(g),
            ..
        }] => g.stops[1].offset,
        other => panic!("lin: unexpected {other:?}"),
    };
    let origin = match evaluate_at_coords(face, 3, coords).as_slice() {
        [DrawCmd::FillGlyph { transform, .. }] => transform.apply(0.0, 0.0),
        other => panic!("tr: unexpected {other:?}"),
    };
    (solid_alpha, stop1_offset, origin)
}

#[test]
fn delta_set_index_map_redirects_var_index_base_through_colr() {
    // Same IVS rows as the IVS test, but every variable field's
    // `var_index_base` is now a flat index that *only* resolves
    // through the COLR DeltaSetIndexMap. Without the indirection the
    // evaluator either returns a zero delta (raw flat index >>16 is
    // outer 0, inner = flat % 65536, which has no IVS row) or pulls
    // the wrong row entirely. Either way the assertions below fail.
    let var_store = build_ivs_test_store();
    let paints = build_indirection_test_paints();
    let index_map = build_indirection_index_map();
    let colr = build_v1_multi_colr_with_map(&paints, &var_store, &index_map);
    let cpal = build_cpal_v0(&[(255, 255, 255, 255), (255, 0, 0, 255), (0, 255, 0, 255)]);
    let bytes = build_face_bytes(&colr, &cpal);
    let face = Face::parse_bytes(&bytes, 0).expect("face parses");

    let (alpha, offset, (dx, dy)) = indirection_values(&face, &[1.0]);
    assert!(
        (alpha - 0.5).abs() < 1e-3,
        "expected indirection to land on alpha 0.5, got {alpha}"
    );
    assert!(
        (offset - 1.25).abs() < 1e-3,
        "expected stop offset 1.25 after indirection, got {offset}"
    );
    assert!((dx - 15.0).abs() < 1e-3, "indirected dx was {dx}");
    assert!((dy - 17.0).abs() < 1e-3, "indirected dy was {dy}");

    // Half way along the axis every delta halves.
    let (alpha, offset, (dx, dy)) = indirection_values(&face, &[0.5]);
    assert!((alpha - 0.75).abs() < 1e-3, "alpha was {alpha}");
    assert!((offset - 1.125).abs() < 1e-3, "offset was {offset}");
    assert!((dx - 12.5).abs() < 1e-3 && (dy - 18.5).abs() < 1e-3);
}

#[test]
fn gdef_variation_data_is_never_read_for_colr() {
    // COLR without a variation store of its own, next to a GDEF that
    // carries the IVS and, right after its header, what an old
    // paint-crate convention read as a DeltaSetIndexMap offset. HarfBuzz
    // reads COLR deltas only from COLR, so nothing varies.
    let var_store = build_ivs_test_store();
    let cpal = build_cpal_v0(&[(255, 255, 255, 255), (255, 0, 0, 255), (0, 255, 0, 255)]);
    let colr = build_v1_multi_colr(&build_indirection_test_paints(), &[]);
    // The trailer holds an offset to the map, which follows the IVS.
    let map_off = (18 + 4 + var_store.len()) as u32;
    let mut gdef = build_gdef_v13(&var_store, &map_off.to_be_bytes());
    gdef.extend_from_slice(&build_indirection_index_map());
    let bytes = build_face_bytes_with_gdef(&colr, &cpal, &gdef);
    let face = Face::parse_bytes(&bytes, 0).expect("face parses");
    assert_eq!(
        face.table_bytes(*b"GDEF").map(<[u8]>::len).ok(),
        Some(gdef.len())
    );
    let (alpha, offset, (dx, dy)) = indirection_values(&face, &[1.0]);
    assert!((alpha - 1.0).abs() < 1e-6, "alpha was {alpha}");
    assert!((offset - 1.0).abs() < 1e-6, "offset was {offset}");
    assert!((dx - 10.0).abs() < 1e-6 && (dy - 20.0).abs() < 1e-6);

    // The plain IVS paints, whose indices need no map, stay static too.
    let colr = build_v1_multi_colr(&build_ivs_test_paints(), &[]);
    let gdef = build_gdef_v13(&var_store, &[]);
    let bytes = build_face_bytes_with_gdef(&colr, &cpal, &gdef);
    let face = Face::parse_bytes(&bytes, 0).expect("face parses");
    let (alpha, offset, (dx, dy)) = indirection_values(&face, &[1.0]);
    assert!((alpha - 1.0).abs() < 1e-6, "alpha was {alpha}");
    assert!((offset - 1.0).abs() < 1e-6, "offset was {offset}");
    assert!((dx - 10.0).abs() < 1e-6 && (dy - 20.0).abs() < 1e-6);
}
