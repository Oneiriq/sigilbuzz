//! Tests for the `COLR` parser: the header, v0 layer records, the
//! clip list lookup and the v1 paint tree.

use super::*;
use alloc::vec::Vec;

fn build_colr_v0(base: &[(u16, u16, u16)], layers: &[(u16, u16)]) -> Vec<u8> {
    // Header is 14 bytes (v0). Then base records, then layer records.
    let base_records_off: u32 = 14;
    let base_bytes = (base.len() * 6) as u32;
    let layer_records_off = base_records_off + base_bytes;
    let mut b = Vec::new();
    b.extend_from_slice(&0u16.to_be_bytes()); // version
    b.extend_from_slice(&(base.len() as u16).to_be_bytes());
    b.extend_from_slice(&base_records_off.to_be_bytes());
    b.extend_from_slice(&layer_records_off.to_be_bytes());
    b.extend_from_slice(&(layers.len() as u16).to_be_bytes());
    for (gid, first, count) in base {
        b.extend_from_slice(&gid.to_be_bytes());
        b.extend_from_slice(&first.to_be_bytes());
        b.extend_from_slice(&count.to_be_bytes());
    }
    for (gid, pal) in layers {
        b.extend_from_slice(&gid.to_be_bytes());
        b.extend_from_slice(&pal.to_be_bytes());
    }
    b
}

#[test]
fn v0_layer_lookup_round_trip() {
    let bytes = build_colr_v0(
        &[(3, 0, 2), (7, 2, 3)],
        &[(10, 0), (11, 1), (20, 0), (21, 1), (22, 2)],
    );
    let colr = Colr::parse(&bytes).unwrap();
    assert_eq!(colr.version(), 0);
    assert_eq!(colr.num_base_glyph_records(), 2);
    assert_eq!(colr.num_layer_records(), 5);

    let layers = colr.v0_layers(3).unwrap();
    assert_eq!(layers.len(), 2);
    let collected: Vec<V0Layer> = layers.iter().collect();
    assert_eq!(
        collected,
        alloc::vec![
            V0Layer {
                glyph_id: 10,
                palette_index: 0
            },
            V0Layer {
                glyph_id: 11,
                palette_index: 1
            },
        ]
    );

    let l7 = colr.v0_layers(7).unwrap();
    assert_eq!(l7.len(), 3);
    assert_eq!(l7.get(2).unwrap().glyph_id, 22);

    assert!(colr.v0_layers(99).is_none());
}

#[test]
fn rejects_unknown_colr_version() {
    let mut bytes = build_colr_v0(&[], &[]);
    bytes[0..2].copy_from_slice(&9u16.to_be_bytes());
    assert!(matches!(Colr::parse(&bytes), Err(Error::Malformed { .. })));
}

// ---------------- v1 paint-tree tests ----------------

/// Size of a v1 header: the 14-byte v0 header plus five Offset32
/// fields.
const V1_HEADER_LEN: u32 = 14 + 5 * 4;

/// Offset of the `varIndexMapOffset` field in a v1 header.
const VAR_INDEX_MAP_SLOT: usize = 26;

/// Offset of the `itemVariationStoreOffset` field in a v1 header.
const VAR_STORE_SLOT: usize = 30;

/// Starts a v1 COLR with empty v0 arrays and the BaseGlyphList
/// right after the header. The layer list, clip list, index map,
/// and variation store offsets are zero. Callers append the
/// BaseGlyphList.
fn v1_header() -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // version
    out.extend_from_slice(&0u16.to_be_bytes()); // numBaseGlyphRecords
    out.extend_from_slice(&V1_HEADER_LEN.to_be_bytes()); // baseGlyphRecordsOffset
    out.extend_from_slice(&V1_HEADER_LEN.to_be_bytes()); // layerRecordsOffset
    out.extend_from_slice(&0u16.to_be_bytes()); // numLayerRecords
    out.extend_from_slice(&V1_HEADER_LEN.to_be_bytes()); // baseGlyphListOffset
    out.extend_from_slice(&0u32.to_be_bytes()); // layerListOffset
    out.extend_from_slice(&0u32.to_be_bytes()); // clipListOffset
    out.extend_from_slice(&0u32.to_be_bytes()); // varIndexMapOffset
    out.extend_from_slice(&0u32.to_be_bytes()); // itemVariationStoreOffset
    assert_eq!(out.len(), V1_HEADER_LEN as usize);
    out
}

#[test]
fn v1_header_reads_index_map_and_store_offsets() {
    let mut out = build_colr_v1_solid(5, 0.5);
    out[VAR_INDEX_MAP_SLOT..VAR_INDEX_MAP_SLOT + 4].copy_from_slice(&0x0102_0304u32.to_be_bytes());
    out[VAR_STORE_SLOT..VAR_STORE_SLOT + 4].copy_from_slice(&0x0506_0708u32.to_be_bytes());
    let colr = Colr::parse(&out).unwrap();
    assert_eq!(colr.var_index_map_offset(), Some(0x0102_0304));
    assert_eq!(colr.var_store_offset(), Some(0x0506_0708));
    // The BaseGlyphList still starts after the full 34-byte header.
    assert!(matches!(
        colr.paint(42),
        Some(ColrPaint::Solid {
            palette_index: 5,
            ..
        })
    ));
}

#[test]
fn v1_header_without_variations_reports_none() {
    let colr_bytes = build_colr_v1_solid(5, 0.5);
    let colr = Colr::parse(&colr_bytes).unwrap();
    assert_eq!(colr.var_index_map_offset(), None);
    assert_eq!(colr.var_store_offset(), None);
}

#[test]
fn v1_header_missing_store_offset_is_truncated() {
    // A four-offset header (30 bytes) is one Offset32 short. The
    // empty v0 arrays point inside the buffer, so only the missing
    // itemVariationStoreOffset can fail.
    let mut short = build_colr_v1_solid(5, 0.5)[..VAR_STORE_SLOT].to_vec();
    short[4..8].copy_from_slice(&14u32.to_be_bytes());
    short[8..12].copy_from_slice(&14u32.to_be_bytes());
    assert!(matches!(Colr::parse(&short), Err(Error::Truncated { .. })));
}

/// Build a minimal v1 COLR with a single base glyph whose paint
/// tree is a `PaintSolid`. Returns the full table bytes.
fn build_colr_v1_solid(palette_index: u16, alpha: f32) -> Vec<u8> {
    let mut out = v1_header();

    // BaseGlyphList: { u32 numRecords; BaseGlyphPaintRecord[...] }.
    // BaseGlyphPaintRecord = { u16 glyphID; Offset32 paintOffset (relative to BaseGlyphList) }.
    out.extend_from_slice(&1u32.to_be_bytes()); // numRecords
    out.extend_from_slice(&42u16.to_be_bytes()); // glyphID
                                                 // paintOffset relative to BaseGlyphList start. The list header
                                                 // is 4 bytes + 6 for the single record = 10 bytes.
    out.extend_from_slice(&10u32.to_be_bytes());

    // Paint body: format=2 (PaintSolid) + u16 palette + F2Dot14 alpha.
    out.push(2);
    out.extend_from_slice(&palette_index.to_be_bytes());
    let alpha_raw = (alpha * 16384.0) as i16;
    out.extend_from_slice(&alpha_raw.to_be_bytes());
    out
}

#[test]
fn v1_solid_paint_round_trip() {
    let bytes = build_colr_v1_solid(5, 0.5);
    let colr = Colr::parse(&bytes).unwrap();
    assert_eq!(colr.version(), 1);
    assert!(colr.has_v1());

    let paint = colr.paint(42).expect("glyph 42 has a paint");
    match paint {
        ColrPaint::Solid {
            palette_index,
            alpha,
        } => {
            assert_eq!(palette_index, 5);
            assert!((alpha - 0.5).abs() < 1e-3);
        }
        _ => panic!("expected Solid, got {paint:?}"),
    }
}

/// The five v1 offsets land in their own fields: the fourth is the
/// DeltaSetIndexMap, the fifth the ItemVariationStore.
#[test]
fn v1_header_reads_all_five_offsets() {
    let mut bytes = build_colr_v1_solid(5, 0.5);
    bytes[22..26].copy_from_slice(&0x60u32.to_be_bytes()); // clipListOffset
    bytes[26..30].copy_from_slice(&0x70u32.to_be_bytes()); // varIndexMapOffset
    bytes[30..34].copy_from_slice(&0x80u32.to_be_bytes()); // varStoreOffset
    let colr = Colr::parse(&bytes).unwrap();
    assert_eq!(colr.clip_list_offset(), Some(0x60));
    assert_eq!(colr.var_index_map_offset(), Some(0x70));
    assert_eq!(colr.var_store_offset(), Some(0x80));
    // The clip list offset points past the table, so there is none.
    assert!(colr.clip_list().is_none());
    assert!(colr.clip_box(42).is_none());

    let zeroed = build_colr_v1_solid(5, 0.5);
    let colr = Colr::parse(&zeroed).unwrap();
    assert_eq!(colr.clip_list_offset(), None);
    assert_eq!(colr.var_index_map_offset(), None);
    assert_eq!(colr.var_store_offset(), None);
}

/// A v1 header cut off before its last offset is rejected, the way
/// HarfBuzz's sanitizer drops the table.
#[test]
fn v1_header_truncated_before_the_store_offset_is_rejected() {
    let mut bytes = build_colr_v1_solid(5, 0.5);
    // Empty v0 arrays at offset 14, so only the v1 fields can run
    // out of bytes.
    bytes[4..8].copy_from_slice(&14u32.to_be_bytes());
    bytes[8..12].copy_from_slice(&14u32.to_be_bytes());
    for len in [14, 18, 30, 33] {
        assert!(
            matches!(Colr::parse(&bytes[..len]), Err(Error::Truncated { .. })),
            "len {len}"
        );
    }
    // The error names the offset where the missing field starts.
    assert!(matches!(
        Colr::parse(&bytes[..30]),
        Err(Error::Truncated { offset: 30, .. })
    ));
    // A v0 table needs only the 14-byte header.
    let mut v0 = bytes[..14].to_vec();
    v0[0..2].copy_from_slice(&0u16.to_be_bytes());
    assert!(Colr::parse(&v0).is_ok());
}

/// `Colr::clip_box` finds a glyph's box through the ClipList.
#[test]
fn clip_box_resolves_through_the_clip_list() {
    let mut bytes = build_colr_v1_solid(5, 0.5);
    let list = bytes.len() as u32;
    bytes[22..26].copy_from_slice(&list.to_be_bytes());
    bytes.push(1); // ClipList format
    bytes.extend_from_slice(&1u32.to_be_bytes());
    bytes.extend_from_slice(&40u16.to_be_bytes());
    bytes.extend_from_slice(&42u16.to_be_bytes());
    bytes.extend_from_slice(&[0, 0, 12]); // box right after the record
    bytes.push(1); // ClipBoxFormat1
    for v in [-10i16, -20, 500, 600] {
        bytes.extend_from_slice(&v.to_be_bytes());
    }
    let colr = Colr::parse(&bytes).unwrap();
    assert_eq!(colr.clip_list().map(|l| l.len()), Some(1));
    assert_eq!(
        colr.clip_box(42),
        Some(ClipBox {
            x_min: -10,
            y_min: -20,
            x_max: 500,
            y_max: 600,
            var_index_base: None,
        })
    );
    assert_eq!(colr.clip_box(43), None);
}

/// PaintColrLayers at the root of a base glyph.
#[test]
fn v1_colr_layers_round_trip() {
    let mut out = v1_header();

    // BaseGlyphList.
    out.extend_from_slice(&1u32.to_be_bytes());
    out.extend_from_slice(&9u16.to_be_bytes());
    out.extend_from_slice(&10u32.to_be_bytes());

    // Paint body: format=1 (ColrLayers), numLayers=3, firstLayerIndex=7.
    out.push(1);
    out.push(3);
    out.extend_from_slice(&7u32.to_be_bytes());

    let colr = Colr::parse(&out).unwrap();
    let paint = colr.paint(9).unwrap();
    match paint {
        ColrPaint::ColrLayers {
            num_layers,
            first_layer_index,
        } => {
            assert_eq!(num_layers, 3);
            assert_eq!(first_layer_index, 7);
        }
        _ => panic!("expected ColrLayers"),
    }
}

/// PaintGlyph wraps a PaintSolid, exercising Offset24 child
/// resolution + depth-one traversal via `paint_at`.
#[test]
fn v1_glyph_paint_chains_to_solid() {
    let mut out = v1_header();

    // BaseGlyphList: one record pointing at paint at offset 10
    // (relative to list start).
    out.extend_from_slice(&1u32.to_be_bytes());
    out.extend_from_slice(&100u16.to_be_bytes());
    out.extend_from_slice(&10u32.to_be_bytes());

    // PaintGlyph: format=10, Offset24 to child, u16 glyph.
    let paint_glyph_start = out.len();
    out.push(10);
    // Reserve 3 bytes for the Offset24, fill later.
    out.extend_from_slice(&[0, 0, 0]);
    out.extend_from_slice(&200u16.to_be_bytes()); // child glyph id

    // Pad so the next paint starts aligned.
    let solid_start = out.len();
    let offset24 = (solid_start - paint_glyph_start) as u32;
    out[paint_glyph_start + 1] = ((offset24 >> 16) & 0xff) as u8;
    out[paint_glyph_start + 2] = ((offset24 >> 8) & 0xff) as u8;
    out[paint_glyph_start + 3] = (offset24 & 0xff) as u8;

    // Child PaintSolid.
    out.push(2);
    out.extend_from_slice(&8u16.to_be_bytes());
    out.extend_from_slice(&16384i16.to_be_bytes()); // 1.0

    let colr = Colr::parse(&out).unwrap();
    let root = colr.paint(100).unwrap();
    let child_offset = match root {
        ColrPaint::Glyph {
            paint_offset,
            glyph_id,
        } => {
            assert_eq!(glyph_id, 200);
            paint_offset
        }
        _ => panic!("expected Glyph"),
    };
    let child = colr.paint_at(child_offset).unwrap();
    match child {
        ColrPaint::Solid {
            palette_index,
            alpha,
        } => {
            assert_eq!(palette_index, 8);
            assert!((alpha - 1.0).abs() < 1e-3);
        }
        _ => panic!("expected Solid child"),
    }

    // Verify the child-offset iteration helper sees exactly
    // the one child.
    let kids = root.child_paint_offsets();
    assert_eq!(kids.len(), 1);
    assert_eq!(kids[0], child_offset);
}

/// LinearGradient exercises the ColorLine sub-offset + FWORD
/// coordinate parsing. Coordinates and stop values are handed
/// back byte-for-byte.
#[test]
fn v1_linear_gradient_parses_colorline_and_coords() {
    let mut out = v1_header();

    out.extend_from_slice(&1u32.to_be_bytes()); // base-glyph count
    out.extend_from_slice(&33u16.to_be_bytes());
    out.extend_from_slice(&10u32.to_be_bytes()); // paint offset rel to list

    // PaintLinearGradient body:
    //   u8 format=4
    //   Offset24 colorLineOffset (relative to this paint)
    //   6x i16 (x0..y2)
    let paint_start = out.len();
    out.push(4);
    // Offset24 placeholder, patched after we know ColorLine offset.
    out.extend_from_slice(&[0, 0, 0]);
    out.extend_from_slice(&10i16.to_be_bytes()); // x0
    out.extend_from_slice(&20i16.to_be_bytes()); // y0
    out.extend_from_slice(&30i16.to_be_bytes()); // x1
    out.extend_from_slice(&40i16.to_be_bytes()); // y1
    out.extend_from_slice(&50i16.to_be_bytes()); // x2
    out.extend_from_slice(&60i16.to_be_bytes()); // y2

    let cl_start = out.len();
    let cl_rel = (cl_start - paint_start) as u32;
    out[paint_start + 1] = ((cl_rel >> 16) & 0xff) as u8;
    out[paint_start + 2] = ((cl_rel >> 8) & 0xff) as u8;
    out[paint_start + 3] = (cl_rel & 0xff) as u8;

    // ColorLine: u8 extend=0 (pad), u16 numStops=2, 2 stops (6 bytes each).
    out.push(0);
    out.extend_from_slice(&2u16.to_be_bytes());
    // Stop 0: offset=0.0, palette=1, alpha=1.0.
    out.extend_from_slice(&0i16.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&16384i16.to_be_bytes());
    // Stop 1: offset=1.0, palette=2, alpha=1.0.
    out.extend_from_slice(&16384i16.to_be_bytes());
    out.extend_from_slice(&2u16.to_be_bytes());
    out.extend_from_slice(&16384i16.to_be_bytes());

    let colr = Colr::parse(&out).unwrap();
    let paint = colr.paint(33).unwrap();
    match paint {
        ColrPaint::LinearGradient {
            color_line,
            x0,
            y0,
            x1,
            y1,
            x2,
            y2,
        } => {
            assert_eq!(x0, 10);
            assert_eq!(y0, 20);
            assert_eq!(x1, 30);
            assert_eq!(y1, 40);
            assert_eq!(x2, 50);
            assert_eq!(y2, 60);
            assert_eq!(color_line.extend, Extend::Pad);
            assert_eq!(color_line.len(), 2);
            let stops: Vec<_> = color_line.stops().collect();
            assert_eq!(stops.len(), 2);
            assert_eq!(stops[0].palette_index, 1);
            assert_eq!(stops[1].palette_index, 2);
        }
        _ => panic!("expected LinearGradient, got {paint:?}"),
    }
}

/// PaintComposite carries two child offsets; make sure both land
/// in `child_paint_offsets()`.
#[test]
fn v1_composite_exposes_both_children() {
    // Build a minimal header pointing at a composite at the end.
    let mut out = v1_header();

    out.extend_from_slice(&1u32.to_be_bytes());
    out.extend_from_slice(&7u16.to_be_bytes());
    out.extend_from_slice(&10u32.to_be_bytes());

    // PaintComposite:
    //   u8 format=32, Offset24 source, u8 mode, Offset24 backdrop.
    let composite_start = out.len();
    out.push(32);
    out.extend_from_slice(&[0, 0, 0]); // source
    out.push(3); // mode = SrcOver
    out.extend_from_slice(&[0, 0, 0]); // backdrop

    // Child source paint: PaintSolid.
    let src_start = out.len();
    let src_rel = (src_start - composite_start) as u32;
    out[composite_start + 1] = ((src_rel >> 16) & 0xff) as u8;
    out[composite_start + 2] = ((src_rel >> 8) & 0xff) as u8;
    out[composite_start + 3] = (src_rel & 0xff) as u8;
    out.push(2);
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&16384i16.to_be_bytes());

    // Child backdrop paint: PaintSolid.
    let bd_start = out.len();
    let bd_rel = (bd_start - composite_start) as u32;
    out[composite_start + 5] = ((bd_rel >> 16) & 0xff) as u8;
    out[composite_start + 6] = ((bd_rel >> 8) & 0xff) as u8;
    out[composite_start + 7] = (bd_rel & 0xff) as u8;
    out.push(2);
    out.extend_from_slice(&2u16.to_be_bytes());
    out.extend_from_slice(&16384i16.to_be_bytes());

    let colr = Colr::parse(&out).unwrap();
    let root = colr.paint(7).unwrap();
    let kids = root.child_paint_offsets();
    assert_eq!(kids.len(), 2);
    match root {
        ColrPaint::Composite { composite_mode, .. } => {
            assert_eq!(composite_mode, CompositeMode::SrcOver);
        }
        _ => panic!("expected Composite"),
    }
    // Both children resolve to Solid.
    for off in kids {
        let child = colr.paint_at(off).unwrap();
        assert!(matches!(child, ColrPaint::Solid { .. }));
    }
}

/// Spot-check translate, scale, rotate, skew. Each should
/// round-trip its single transform argument.
#[test]
fn v1_simple_transform_variants_round_trip() {
    fn build_single_transform(format: u8, payload: &[u8]) -> Vec<u8> {
        let mut out = v1_header();
        out.extend_from_slice(&1u32.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&10u32.to_be_bytes());
        let pstart = out.len();
        out.push(format);
        out.extend_from_slice(&[0, 0, 0]); // Offset24 child (patched below)
        out.extend_from_slice(payload);
        let cstart = out.len();
        let rel = (cstart - pstart) as u32;
        out[pstart + 1] = ((rel >> 16) & 0xff) as u8;
        out[pstart + 2] = ((rel >> 8) & 0xff) as u8;
        out[pstart + 3] = (rel & 0xff) as u8;
        // Child Solid so the tree is well-formed.
        out.push(2);
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&16384i16.to_be_bytes());
        out
    }

    // Format 14 (Translate): dx=5, dy=-7 (i16 each).
    let bytes = build_single_transform(14, &[0, 5, 0xff, 0xf9]);
    let colr = Colr::parse(&bytes).unwrap();
    match colr.paint(1).unwrap() {
        ColrPaint::Translate { dx, dy, .. } => {
            assert_eq!(dx, 5);
            assert_eq!(dy, -7);
        }
        p => panic!("expected Translate, got {p:?}"),
    }

    // Format 20 (ScaleUniform): scale=0.5 (F2Dot14 = 8192).
    let bytes = build_single_transform(20, &[0x20, 0x00]);
    let colr = Colr::parse(&bytes).unwrap();
    match colr.paint(1).unwrap() {
        ColrPaint::ScaleUniform { scale, .. } => {
            assert!((scale - 0.5).abs() < 1e-3);
        }
        p => panic!("expected ScaleUniform, got {p:?}"),
    }

    // Format 24 (Rotate): angle=0.25 (= F2Dot14 4096).
    let bytes = build_single_transform(24, &[0x10, 0x00]);
    let colr = Colr::parse(&bytes).unwrap();
    match colr.paint(1).unwrap() {
        ColrPaint::Rotate { angle, .. } => {
            assert!((angle - 0.25).abs() < 1e-3);
        }
        p => panic!("expected Rotate, got {p:?}"),
    }
}
