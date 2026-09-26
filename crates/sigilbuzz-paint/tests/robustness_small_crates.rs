//! Malformed COLRv1 paint graphs that used to hang the evaluator or
//! grow its output without bound.
//!
//! Each fixture is a hand-built COLR table inside a minimal SFNT. The
//! paint graphs are acyclic by glyph id but share nodes, so a naive
//! depth-first walk expands them exponentially.

use sigilbuzz::Face;
use sigilbuzz_paint::{evaluate, DrawCmd, PaintSource};

/// Glyph id that owns the root paint in every fixture.
const ROOT_GID: u16 = 7;

/// Work budget of the evaluator, counted as paint nodes plus color
/// stops. Mirrors the private `MAX_WORK` constant in `eval.rs`.
const MAX_WORK: usize = 1 << 18;

/// Builds a minimal SFNT holding the COLR table and a one-entry CPAL.
fn build_face_bytes(colr: &[u8]) -> Vec<u8> {
    let cpal = build_cpal();
    let dir_len = 12 + 2 * 16;
    let colr_off = dir_len;
    let cpal_off = colr_off + colr.len();

    let mut out = Vec::new();
    out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    out.extend_from_slice(&2u16.to_be_bytes()); // numTables
    out.extend_from_slice(&[0; 6]); // searchRange, entrySelector, rangeShift
    for (tag, off, len) in [
        (b"COLR", colr_off, colr.len()),
        (b"CPAL", cpal_off, cpal.len()),
    ] {
        out.extend_from_slice(tag);
        out.extend_from_slice(&0u32.to_be_bytes()); // checksum
        out.extend_from_slice(&(off as u32).to_be_bytes());
        out.extend_from_slice(&(len as u32).to_be_bytes());
    }
    out.extend_from_slice(colr);
    out.extend_from_slice(&cpal);
    out
}

/// CPAL v0 with one palette holding one opaque red entry.
fn build_cpal() -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_be_bytes()); // version
    out.extend_from_slice(&1u16.to_be_bytes()); // numPaletteEntries
    out.extend_from_slice(&1u16.to_be_bytes()); // numPalettes
    out.extend_from_slice(&1u16.to_be_bytes()); // numColorRecords
    out.extend_from_slice(&14u32.to_be_bytes()); // colorRecordsArrayOffset
    out.extend_from_slice(&0u16.to_be_bytes()); // colorRecordIndices[0]
    out.extend_from_slice(&[0, 0, 255, 255]); // BGRA
    out
}

/// COLR v1 header plus a BaseGlyphList with one record for
/// `ROOT_GID`. Returns the bytes and the offset of the BaseGlyphList.
/// `layer_list_offset` is written into the header as given.
/// The root paint offset is patched in by the caller.
fn build_v1_header(layer_list_offset: u32) -> (Vec<u8>, usize) {
    let header_len: u32 = 34;
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // version
    out.extend_from_slice(&0u16.to_be_bytes()); // numBaseGlyphRecords
    out.extend_from_slice(&header_len.to_be_bytes()); // baseGlyphRecordsOffset
    out.extend_from_slice(&header_len.to_be_bytes()); // layerRecordsOffset
    out.extend_from_slice(&0u16.to_be_bytes()); // numLayerRecords
    out.extend_from_slice(&header_len.to_be_bytes()); // baseGlyphListOffset
    out.extend_from_slice(&layer_list_offset.to_be_bytes()); // layerListOffset
    out.extend_from_slice(&0u32.to_be_bytes()); // clipListOffset
    out.extend_from_slice(&0u32.to_be_bytes()); // varIndexMapOffset
    out.extend_from_slice(&0u32.to_be_bytes()); // itemVariationStoreOffset
    let bgl = out.len();
    out.extend_from_slice(&1u32.to_be_bytes()); // numBaseGlyphPaintRecords
    out.extend_from_slice(&ROOT_GID.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes()); // paintOffset, patched later
    (out, bgl)
}

/// Writes the root paint offset of the single BaseGlyphPaintRecord.
fn set_root_paint(colr: &mut [u8], bgl: usize, paint_at: usize) {
    let rel = (paint_at - bgl) as u32;
    colr[bgl + 6..bgl + 10].copy_from_slice(&rel.to_be_bytes());
}

/// Appends a PaintColrLayers record.
fn push_colr_layers(colr: &mut Vec<u8>, num_layers: u8, first_layer: u32) {
    colr.push(1);
    colr.push(num_layers);
    colr.extend_from_slice(&first_layer.to_be_bytes());
}

/// Appends a LayerList whose entries point at the given absolute paint
/// offsets. Returns the offset of the list.
fn push_layer_list(colr: &mut Vec<u8>, targets: &[usize]) -> usize {
    let start = colr.len();
    colr.extend_from_slice(&(targets.len() as u32).to_be_bytes());
    for &target in targets {
        colr.extend_from_slice(&((target - start) as u32).to_be_bytes());
    }
    start
}

fn is_balanced(cmds: &[DrawCmd]) -> bool {
    let mut open = 0usize;
    for cmd in cmds {
        match cmd {
            DrawCmd::PushLayer { .. } => open += 1,
            DrawCmd::PopLayer => {
                let Some(next) = open.checked_sub(1) else {
                    return false;
                };
                open = next;
            }
            DrawCmd::FillGlyph { .. } => {}
        }
    }
    open == 0
}

#[test]
fn shared_colr_layers_fan_out_terminates() {
    // A LayerList of 255 entries that all point at one
    // PaintColrLayers(255 layers, first 0). Every layer is the same
    // node again, so a plain walk visits 255^64 nodes before the depth
    // cap stops it. It emits nothing, it just never returns.
    let layer_list = 44usize;
    let paint = layer_list + 4 + 255 * 4;
    let (mut colr, bgl) = build_v1_header(layer_list as u32);
    assert_eq!(colr.len(), layer_list);
    push_layer_list(&mut colr, &[paint; 255]);
    assert_eq!(colr.len(), paint);
    push_colr_layers(&mut colr, 255, 0);
    set_root_paint(&mut colr, bgl, paint);

    let bytes = build_face_bytes(&colr);
    let face = Face::parse_bytes(&bytes, 0).unwrap();
    let cmds = evaluate(&face, ROOT_GID);
    assert!(
        cmds.is_empty(),
        "no leaf paints, got {} commands",
        cmds.len()
    );
}

#[test]
fn self_referencing_composite_output_is_bounded() {
    // PaintComposite whose source and backdrop offsets are both 0, so
    // both children are the composite itself. Each visit emits a
    // PushLayer and a PopLayer, and the node count doubles per level.
    let (mut colr, bgl) = build_v1_header(0);
    let paint = colr.len();
    colr.push(32); // PaintComposite
    colr.extend_from_slice(&[0, 0, 0]); // sourcePaintOffset
    colr.push(3); // SrcOver
    colr.extend_from_slice(&[0, 0, 0]); // backdropPaintOffset
    set_root_paint(&mut colr, bgl, paint);

    let bytes = build_face_bytes(&colr);
    let face = Face::parse_bytes(&bytes, 0).unwrap();
    let cmds = evaluate(&face, ROOT_GID);
    assert!(!cmds.is_empty());
    assert!(
        cmds.len() <= 2 * MAX_WORK,
        "composite expansion not bounded: {} commands",
        cmds.len()
    );
    assert!(is_balanced(&cmds), "PushLayer and PopLayer must pair up");
}

#[test]
fn shared_gradient_stops_are_bounded() {
    // Two levels of PaintColrLayers fan out to 255 * 255 references of
    // one PaintLinearGradient whose ColorLine holds 65535 stops. The
    // node count stays small, but resolving every stop would produce
    // over four billion ColorStop values.
    let layer_list = 44usize;
    let entries = 510usize;
    let root = layer_list + 4 + entries * 4;
    let inner = root + 6;
    let gradient = inner + 6;
    let color_line = gradient + 16;

    let (mut colr, bgl) = build_v1_header(layer_list as u32);
    assert_eq!(colr.len(), layer_list);
    let mut targets = vec![inner; 255];
    targets.resize(entries, gradient);
    push_layer_list(&mut colr, &targets);
    assert_eq!(colr.len(), root);
    push_colr_layers(&mut colr, 255, 0);
    push_colr_layers(&mut colr, 255, 255);
    assert_eq!(colr.len(), gradient);
    colr.push(4); // PaintLinearGradient
    colr.extend_from_slice(&[0, 0, 16]); // colorLineOffset
    for v in [0i16, 0, 100, 0, 0, 100] {
        colr.extend_from_slice(&v.to_be_bytes());
    }
    assert_eq!(colr.len(), color_line);
    colr.push(0); // extend: pad
    colr.extend_from_slice(&u16::MAX.to_be_bytes()); // numStops
    for _ in 0..u16::MAX {
        colr.extend_from_slice(&0i16.to_be_bytes()); // stopOffset
        colr.extend_from_slice(&0u16.to_be_bytes()); // paletteIndex
        colr.extend_from_slice(&0x4000i16.to_be_bytes()); // alpha 1.0
    }
    set_root_paint(&mut colr, bgl, root);

    let bytes = build_face_bytes(&colr);
    let face = Face::parse_bytes(&bytes, 0).unwrap();
    let cmds = evaluate(&face, ROOT_GID);
    let mut gradients = 0usize;
    let mut stops = 0usize;
    for cmd in &cmds {
        if let DrawCmd::FillGlyph {
            paint: PaintSource::Gradient(g),
            ..
        } = cmd
        {
            gradients += 1;
            assert_eq!(g.stops.len(), usize::from(u16::MAX));
            stops += g.stops.len();
        }
    }
    assert!(gradients > 0, "the first gradients fit in the budget");
    assert!(stops <= MAX_WORK, "stop output not bounded: {stops} stops");
}
