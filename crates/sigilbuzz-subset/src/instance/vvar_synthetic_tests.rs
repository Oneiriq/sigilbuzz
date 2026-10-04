//! Synthetic-VF tests that exercise the VVAR-aware vmtx bake.
//!
//! No real fixture in sigilbuzz's test corpus carries `vmtx` +
//! `VVAR` together. Most variable fonts in the wild are
//! horizontal-only. We unit-test the helpers directly with
//! hand-built records rather than spinning up a synthetic SFNT
//! around the bake. The integration shape (vmtx delta application
//! and VVAR drop in the directory) is exercised by the
//! [`super::tests::rubik_vmtx_passthrough_when_source_has_none`]
//! test on the no-VVAR side.

use super::metrics::{apply_mvar_records, patch_i16, patch_u16};
use super::*;
use crate::hmtx::emit_long_metrics;

#[test]
fn patch_i16_clamps_at_overflow() {
    let mut buf = Some(alloc::vec![0x7Fu8, 0xFEu8]); // 32766
    patch_i16(&mut buf, 0, 5);
    let b = buf.unwrap();
    assert_eq!(i16::from_be_bytes([b[0], b[1]]), i16::MAX);
}

#[test]
fn patch_u16_floors_at_zero() {
    let mut buf = Some(alloc::vec![0x00u8, 0x05u8]);
    patch_u16(&mut buf, 0, -50);
    let b = buf.unwrap();
    assert_eq!(u16::from_be_bytes([b[0], b[1]]), 0);
}

#[test]
fn patch_i16_handles_short_buffer_gracefully() {
    let mut buf = Some(alloc::vec![0u8]);
    // Out-of-range offset must not panic: short bufs survive.
    patch_i16(&mut buf, 10, 5);
    assert_eq!(buf.unwrap().len(), 1);
}

#[test]
fn vmtx_emission_compresses_trailing_run() {
    // 5 glyphs, every glyph shares advance 1000. The compression
    // matches `bake_hmtx`: trailing identical advances collapse
    // into the tsb-only tail. The shared-advance run leaves 2
    // long entries (the loop bottoms at 1 then adds back 1 to
    // anchor the shared advance, same as hmtx).
    let advances = alloc::vec![1000u16; 5];
    let tsbs = alloc::vec![10i16, 20, 30, 40, 50];
    let (bytes, n_long) = emit_long_metrics(&advances, &tsbs);
    assert_eq!(n_long, 2);
    // 2 long entries (4 B each) + 3 trailing tsbs (2 B each) = 14.
    assert_eq!(bytes.len(), 4 * 2 + 3 * 2);
}

#[test]
fn vmtx_emission_extends_long_range_when_trailing_advances_diverge() {
    // 5 glyphs. Source vmtx had long_count=1 (every glyph shared
    // advance 1000), but a hypothetical VVAR delta at gid 3 shifted
    // its advance to 1100. The emission must promote gid 3 into
    // the long range so its distinct advance survives the byte
    // emission. Without the long-count recompute fix this trailing
    // delta is silently dropped.
    let advances = alloc::vec![1000u16, 1000, 1000, 1100, 1000];
    let tsbs = alloc::vec![10i16, 20, 30, 40, 50];
    let (bytes, n_long) = emit_long_metrics(&advances, &tsbs);
    // Same compression rule as hmtx: scan trailing equal-to-last
    // run, plus one anchor entry. Last advance is 1000; gid 3 is
    // 1100 (different) so the run is just gid 4. long_count = 5
    // - 1 + 1 = 5 (every glyph in the long range).
    assert_eq!(n_long, 5);
    assert_eq!(bytes.len(), 4 * 5);
    // Gid 3's advance survives at the rebuilt long-entry slot.
    let g3_adv = u16::from_be_bytes([bytes[3 * 4], bytes[3 * 4 + 1]]);
    assert_eq!(g3_adv, 1100);
}

#[test]
fn write_vhea_metrics_count_patches_tail() {
    let mut vhea = alloc::vec![0u8; 36];
    crate::util::write_vhea_metrics_count(&mut vhea, 7).unwrap();
    assert_eq!(&vhea[34..36], &7u16.to_be_bytes());
}

/// Builds a minimal MVAR table carrying `records` (each pointing
/// at IVS item (outer=0, inner=0)) and an embedded variation store
/// that resolves to `delta` at coord 1.0.
fn build_synthetic_mvar(records: &[[u8; 4]], delta: i16) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // major
    out.extend_from_slice(&0u16.to_be_bytes()); // minor
    out.extend_from_slice(&0u16.to_be_bytes()); // reserved
    out.extend_from_slice(&8u16.to_be_bytes()); // valueRecordSize
    out.extend_from_slice(&(records.len() as u16).to_be_bytes());
    let store_off_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes()); // store offset placeholder
    for tag in records {
        out.extend_from_slice(tag);
        out.extend_from_slice(&0u16.to_be_bytes()); // outer
        out.extend_from_slice(&0u16.to_be_bytes()); // inner
    }
    let store_off = out.len() as u16;
    out[store_off_slot..store_off_slot + 2].copy_from_slice(&store_off.to_be_bytes());

    // ItemVariationStore with one region (full peak at axis 0,
    // coord 1.0) and one subtable carrying a single i16 delta.
    // Layout: format(=1) + regionListOff + subtableCount +
    // subtableOff[1] + RegionList + Subtable.
    let mut ivs: Vec<u8> = Vec::new();
    ivs.extend_from_slice(&1u16.to_be_bytes()); // format
    let region_off_slot = ivs.len();
    ivs.extend_from_slice(&0u32.to_be_bytes()); // regionListOff placeholder
    ivs.extend_from_slice(&1u16.to_be_bytes()); // subtableCount
    let sub_off_slot = ivs.len();
    ivs.extend_from_slice(&0u32.to_be_bytes()); // subtableOffsets[0] placeholder

    let region_off = ivs.len() as u32;
    ivs.extend_from_slice(&1u16.to_be_bytes()); // axisCount
    ivs.extend_from_slice(&1u16.to_be_bytes()); // regionCount
                                                // Region 0 axis 0: start=0, peak=1.0, end=1.0 in F2DOT14.
    ivs.extend_from_slice(&0i16.to_be_bytes());
    ivs.extend_from_slice(&0x4000i16.to_be_bytes());
    ivs.extend_from_slice(&0x4000i16.to_be_bytes());

    let sub_off = ivs.len() as u32;
    ivs.extend_from_slice(&1u16.to_be_bytes()); // itemCount
    ivs.extend_from_slice(&1u16.to_be_bytes()); // wordDeltaCount = 1 (i16 wide)
    ivs.extend_from_slice(&1u16.to_be_bytes()); // regionIndexCount
    ivs.extend_from_slice(&0u16.to_be_bytes()); // regionIndexes[0]
                                                // Single delta row, one region: i16 word.
    ivs.extend_from_slice(&delta.to_be_bytes());

    ivs[region_off_slot..region_off_slot + 4].copy_from_slice(&region_off.to_be_bytes());
    ivs[sub_off_slot..sub_off_slot + 4].copy_from_slice(&sub_off.to_be_bytes());

    out.extend_from_slice(&ivs);
    out
}

#[test]
fn apply_mvar_records_skips_duplicate_tag() {
    // MVAR with two `hasc` records pointing at the same item.
    // Without the dedup the OS/2 sTypoAscender would be patched
    // twice: this test pins `apply_mvar_records` to first-wins.
    let blob = build_synthetic_mvar(&[*b"hasc", *b"hasc"], 100);
    let mvar = sigilbuzz::tables::Mvar::parse(&blob).unwrap();
    // OS/2 v2 (96 bytes) with sTypoAscender = 800 at offset 68.
    let mut os2 = alloc::vec![0u8; 96];
    os2[68..70].copy_from_slice(&800i16.to_be_bytes());
    let baked = apply_mvar_records(&mvar, &[1.0], Some(os2), None, None, None).unwrap();
    let out = baked.os2.unwrap();
    let val = i16::from_be_bytes([out[68], out[69]]);
    // First-wins: 800 + 100 == 900. (Without dedup: 800 + 200 = 1000.)
    assert_eq!(val, 900, "duplicate hasc must apply delta exactly once");
}

#[test]
fn apply_mvar_records_moves_the_caret_fields() {
    // hcrs, hcrn and hcof vary hhea's caretSlopeRise, caretSlopeRun and
    // caretOffset (offsets 18, 20, 22); vcrs, vcrn and vcof the same
    // fields of vhea. Each record adds 99 at the peak, so 49.5 at 0.5,
    // which rounds half up as HarfBuzz rounds field plus delta: 50.
    // gsp0 (a gasp range) changes nothing, as in HarfBuzz.
    let tags = [
        *b"gsp0", *b"hcof", *b"hcrn", *b"hcrs", *b"vcof", *b"vcrn", *b"vcrs",
    ];
    let blob = build_synthetic_mvar(&tags, 99);
    let mvar = sigilbuzz::tables::Mvar::parse(&blob).unwrap();
    let mut header = alloc::vec![0u8; 36];
    header[18..20].copy_from_slice(&1i16.to_be_bytes());
    header[22..24].copy_from_slice(&(-114i16).to_be_bytes());
    let os2 = alloc::vec![0u8; 96];
    let post = alloc::vec![0u8; 32];
    let baked = apply_mvar_records(
        &mvar,
        &[0.5],
        Some(os2.clone()),
        Some(header.clone()),
        Some(header.clone()),
        Some(post.clone()),
    )
    .unwrap();
    let field = |t: &[u8], off: usize| i16::from_be_bytes([t[off], t[off + 1]]);
    for table in [baked.hhea.unwrap(), baked.vhea.unwrap()] {
        assert_eq!(field(&table, 18), 51, "caretSlopeRise 1 + 50");
        assert_eq!(field(&table, 20), 50, "caretSlopeRun 0 + 50");
        assert_eq!(field(&table, 22), -64, "caretOffset -114 + 50");
        let mut rest = table.clone();
        rest[18..24].copy_from_slice(&header[18..24]);
        assert_eq!(rest, header, "no other field moves");
    }
    assert_eq!(baked.os2.unwrap(), os2);
    assert_eq!(baked.post.unwrap(), post);

    // A tie below zero: -114 - 0.5 is -114.5, which HarfBuzz rounds up
    // to -114, as the delta -0.5 rounds to 0.
    let blob = build_synthetic_mvar(&[*b"hcof"], -1);
    let mvar = sigilbuzz::tables::Mvar::parse(&blob).unwrap();
    let baked = apply_mvar_records(&mvar, &[0.5], None, Some(header.clone()), None, None).unwrap();
    assert_eq!(field(&baked.hhea.unwrap(), 22), -114);
}
