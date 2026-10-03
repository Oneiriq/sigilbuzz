//! EBDT mono and composite (format 8) bitmap tests on synthetic
//! EBLC / EBDT fixtures.

use sigilbuzz::Face;
use sigilbuzz_render::{Rasterizer, RenderError};

use crate::fixtures::{build_sfnt, maxp_05};

/// Builds an EBDT format-1 entry: 5-byte SmallGlyphMetrics followed
/// by `data` (byte-aligned 1bpp mask).
fn ebdt_format1_entry(width: u8, height: u8, data: &[u8]) -> Vec<u8> {
    let mut out = vec![
        height,
        width,
        0,                  // bearing_x
        height as i8 as u8, // bearing_y
        width,              // advance
    ];
    out.extend_from_slice(data);
    out
}

/// Builds an EBLC table with one strike at `ppem` covering glyph ids
/// 1..=1, using index format 1 (variable-metric u32 offsets), pointing
/// at an EBDT table where `image_data_offset` plus the offset locates
/// the entry.
fn build_eblc_one_glyph(ppem: u8, glyph_size: u32) -> Vec<u8> {
    let mut blob = Vec::new();
    // EBLC header
    blob.extend_from_slice(&2u16.to_be_bytes()); // major (v2)
    blob.extend_from_slice(&0u16.to_be_bytes()); // minor
    blob.extend_from_slice(&1u32.to_be_bytes()); // numSizes

    // BitmapSize record (48 B)
    let bitmap_size_off = blob.len() as u32; // 8
    let index_array_off = bitmap_size_off + 48; // 56
    blob.extend_from_slice(&index_array_off.to_be_bytes());
    // indexTablesSize: array entry (8) + sub header (8) + 2 u32 offsets (8) = 24
    blob.extend_from_slice(&24u32.to_be_bytes());
    blob.extend_from_slice(&1u32.to_be_bytes()); // numberOfIndexSubTables
    blob.extend_from_slice(&0u32.to_be_bytes()); // colorRef
    blob.extend_from_slice(&[0u8; 12]); // hori metrics
    blob.extend_from_slice(&[0u8; 12]); // vert metrics
    blob.extend_from_slice(&1u16.to_be_bytes()); // startGlyphIndex
    blob.extend_from_slice(&1u16.to_be_bytes()); // endGlyphIndex
    blob.push(ppem);
    blob.push(ppem);
    blob.push(1); // bitDepth = mono
    blob.push(0x01); // flags = horizontal

    // IndexSubTableArray entry: first/last/additionalOffset (=8, just past this entry)
    blob.extend_from_slice(&1u16.to_be_bytes());
    blob.extend_from_slice(&1u16.to_be_bytes());
    blob.extend_from_slice(&8u32.to_be_bytes()); // additional offset (relative to array base)

    // IndexSubTable header: format 1 (var-metric u32), image format 1, image data offset 0.
    blob.extend_from_slice(&1u16.to_be_bytes());
    blob.extend_from_slice(&1u16.to_be_bytes());
    blob.extend_from_slice(&0u32.to_be_bytes());
    // Two offsets: 0, glyph_size (sentinel)
    blob.extend_from_slice(&0u32.to_be_bytes());
    blob.extend_from_slice(&glyph_size.to_be_bytes());
    blob
}

fn build_ebdt(entry: &[u8]) -> Vec<u8> {
    let mut blob = Vec::new();
    blob.extend_from_slice(&2u16.to_be_bytes()); // major
    blob.extend_from_slice(&0u16.to_be_bytes()); // minor
    blob.extend_from_slice(entry);
    blob
}

#[test]
fn ebdt_synthetic_renders_mono_glyph_at_strike_size() {
    // 16x16 'A' silhouette: byte-aligned 1bpp, 32 bytes (2 per row).
    // Just need a recognizable shape; spot-check a known set bit.
    let mut mask = vec![0u8; 32];
    // Top: row 0 has crossbar bits in middle.
    mask[0] = 0b0011_1100;
    mask[1] = 0b0011_1100;
    // Bottom-left descender: row 15 has bit 0 set on left.
    mask[30] = 0b1100_0000;
    mask[31] = 0b0000_0011;

    let entry = ebdt_format1_entry(16, 16, &mask);
    // EBDT table = 4-byte header + entry; EBLC's image_data_offset is
    // 0, so glyph offset 0 lands at the *start* of EBDT (the header).
    // Shift the entry into a position the EBLC offset can address by
    // wrapping it, but our EBLC builder uses image_data_offset = 0,
    // so the glyph entry must start at byte 0 of EBDT (i.e. before the
    // header). EBDT::parse expects the 4-byte version header first;
    // adjust the EBLC builder to use image_data_offset = 4.
    let _ = entry;

    let entry = ebdt_format1_entry(16, 16, &mask);
    // Re-build the EBLC with image_data_offset = 4 (skipping the
    // EBDT version header).
    let mut blob = Vec::new();
    blob.extend_from_slice(&2u16.to_be_bytes());
    blob.extend_from_slice(&0u16.to_be_bytes());
    blob.extend_from_slice(&1u32.to_be_bytes());
    let bitmap_size_off = blob.len() as u32;
    let index_array_off = bitmap_size_off + 48;
    blob.extend_from_slice(&index_array_off.to_be_bytes());
    blob.extend_from_slice(&24u32.to_be_bytes());
    blob.extend_from_slice(&1u32.to_be_bytes());
    blob.extend_from_slice(&0u32.to_be_bytes());
    blob.extend_from_slice(&[0u8; 12]);
    blob.extend_from_slice(&[0u8; 12]);
    blob.extend_from_slice(&1u16.to_be_bytes());
    blob.extend_from_slice(&1u16.to_be_bytes());
    blob.push(16);
    blob.push(16);
    blob.push(1);
    blob.push(0x01);
    blob.extend_from_slice(&1u16.to_be_bytes());
    blob.extend_from_slice(&1u16.to_be_bytes());
    blob.extend_from_slice(&8u32.to_be_bytes());
    blob.extend_from_slice(&1u16.to_be_bytes()); // index format
    blob.extend_from_slice(&1u16.to_be_bytes()); // image format
    blob.extend_from_slice(&4u32.to_be_bytes()); // image_data_offset = 4 (past EBDT header)
    blob.extend_from_slice(&0u32.to_be_bytes()); // entry off 0
    blob.extend_from_slice(&(entry.len() as u32).to_be_bytes()); // sentinel
    let eblc = blob;

    let ebdt = build_ebdt(&entry);
    let font = build_sfnt(vec![
        (*b"maxp", maxp_05(2)),
        (*b"EBLC", eblc),
        (*b"EBDT", ebdt),
    ]);

    let face = Face::parse_bytes(&font, 0).unwrap();
    let rast = Rasterizer::new();
    let pix = rast
        .rasterize_bitmap_glyph(&face, 1, 16.0, &[])
        .expect("EBDT mono glyph renders");
    assert_eq!(pix.width, 16);
    assert_eq!(pix.height, 16);
    // Top-left corner is unset (bit 7 of 0b0011_1100).
    assert_eq!(pix.get(0, 0), [0, 0, 0, 0]);
    // x=2 of row 0: bit 5 of 0b0011_1100 is set.
    assert_eq!(pix.get(2, 0), [0, 0, 0, 255]);
    // x=14 of row 15: bit 1 of 0b0000_0011 is set.
    assert_eq!(pix.get(14, 15), [0, 0, 0, 255]);
}

#[test]
fn ebdt_synthetic_falls_back_when_only_ebdt_present() {
    // Confirm EBDT triggers when the font carries no CBDT/sbix.
    // (Mirror the previous test's plumbing but assert the variant
    // routing from the unified accessor.)
    let mask = [0xffu8; 2]; // 8x2 all-set
    let entry = ebdt_format1_entry(8, 2, &mask);
    let eblc = build_eblc_one_glyph(16, entry.len() as u32);
    // Patch the image_data_offset from 0 to 4 (past EBDT version header).
    // Locate the IndexSubTable header inside the eblc blob: header(8) +
    // bitmap_size(48) + array_entry(8) = 64; image_data_offset is at
    // bytes 68..72.
    let mut eblc = eblc;
    eblc[68..72].copy_from_slice(&4u32.to_be_bytes());
    let ebdt = build_ebdt(&entry);
    let font = build_sfnt(vec![
        (*b"maxp", maxp_05(2)),
        (*b"EBLC", eblc),
        (*b"EBDT", ebdt),
    ]);
    let face = Face::parse_bytes(&font, 0).unwrap();
    // glyph_bitmap should expose this as the EBDT variant.
    let entry = face.glyph_bitmap(1, 16).unwrap().expect("EBDT entry");
    match entry {
        sigilbuzz::GlyphBitmapEntry::Ebdt { ppem_y, bitmap, .. } => {
            assert_eq!(ppem_y, 16);
            assert_eq!(bitmap.image_format, 1);
            assert_eq!(bitmap.metrics.width(), 8);
        }
        _ => panic!("expected Ebdt variant"),
    }
}

// ---------------------------------------------------------------------------
// EBDT format 8 (composite mono bitmaps): synthetic 3-glyph fixtures.
//
// Layout common to every test below:
//   gid 0  .notdef (no entry)
//   gid 1  composite that references gids 2 and 3
//   gid 2  4x4 fully-set mono square at (0, 0)
//   gid 3  2x2 fully-set mono square at (8, 0)
// EBLC carries one strike at 16 ppem with index format 1 covering
// gids 1..=3. Glyph entries within EBDT live in directory order; the
// EBLC u32 offsets address them relative to image_data_offset.
// ---------------------------------------------------------------------------

/// Builds an EBDT format-1 entry: 5-byte SmallGlyphMetrics + raw mask.
fn ebdt_fmt1_entry_mask(width: u8, height: u8, mask: &[u8]) -> Vec<u8> {
    let mut out = vec![
        height,
        width,
        0,                  // bearing_x
        height as i8 as u8, // bearing_y
        width,              // advance
    ];
    out.extend_from_slice(mask);
    out
}

/// Builds an EBDT format-8 composite entry: 5-byte SmallGlyphMetrics +
/// 1-byte pad + u16 numComponents + 4 bytes per component.
fn ebdt_fmt8_composite_entry(width: u8, height: u8, components: &[(u16, i8, i8)]) -> Vec<u8> {
    let mut out = vec![height, width, 0, height as i8 as u8, width];
    out.push(0); // pad
    out.extend_from_slice(&(components.len() as u16).to_be_bytes());
    for (gid, dx, dy) in components {
        out.extend_from_slice(&gid.to_be_bytes());
        out.push(*dx as u8);
        out.push(*dy as u8);
    }
    out
}

fn build_ebdt_multi(entries: &[Vec<u8>]) -> Vec<u8> {
    let mut blob = Vec::new();
    blob.extend_from_slice(&2u16.to_be_bytes());
    blob.extend_from_slice(&0u16.to_be_bytes());
    for e in entries {
        blob.extend_from_slice(e);
    }
    blob
}

#[test]
fn ebdt_format8_composite_overlays_two_components() {
    // Parent (gid 1): 16-wide x 4-tall canvas, format 8.
    // gid 2: 4x4 fully-set mono square at offset (0, 0).
    // gid 3: 2x2 fully-set mono square at offset (8, 0).
    //
    // Each glyph carries its own image_format, so the EBLC strike has
    // three IndexSubTables (one per gid) sharing the same data
    // table.
    use sigilbuzz::{Blob, Face};
    use sigilbuzz_render::Rasterizer;

    let parent_entry = ebdt_fmt8_composite_entry(16, 4, &[(2, 0, 0), (3, 8, 0)]);
    let leaf2_entry = ebdt_fmt1_entry_mask(4, 4, &[0xF0, 0xF0, 0xF0, 0xF0]);
    let leaf3_entry = ebdt_fmt1_entry_mask(2, 2, &[0xC0, 0xC0]);
    let ebdt = build_ebdt_multi(&[
        parent_entry.clone(),
        leaf2_entry.clone(),
        leaf3_entry.clone(),
    ]);
    let eblc = build_eblc_three_subtables(
        16,
        // (start_gid, end_gid, image_format, entry_offsets relative
        // to image_data_offset, then sentinel)
        &[
            (1, 1, 8, &[0, parent_entry.len() as u32]),
            (
                2,
                2,
                1,
                &[
                    parent_entry.len() as u32,
                    (parent_entry.len() + leaf2_entry.len()) as u32,
                ],
            ),
            (
                3,
                3,
                1,
                &[
                    (parent_entry.len() + leaf2_entry.len()) as u32,
                    (parent_entry.len() + leaf2_entry.len() + leaf3_entry.len()) as u32,
                ],
            ),
        ],
    );
    let font = build_sfnt(vec![
        (*b"maxp", maxp_05(4)),
        (*b"EBLC", eblc),
        (*b"EBDT", ebdt),
    ]);

    let blob = Blob::new(&font);
    let face = Face::parse(&blob, 0).unwrap();
    let rast = Rasterizer::new();
    let pix = rast
        .rasterize_bitmap_glyph(&face, 1, 16.0, &[])
        .expect("composite renders");
    // Parent canvas should be 16x4. Component gid 2 covers x=0..=3
    // y=0..=3 (4x4). Component gid 3 covers x=8..=9 y=0..=1 (2x2).
    assert_eq!(pix.width, 16);
    assert_eq!(pix.height, 4);
    // Inside gid 2's footprint: opaque black.
    assert_eq!(pix.get(0, 0), [0, 0, 0, 255]);
    assert_eq!(pix.get(3, 3), [0, 0, 0, 255]);
    // Gap between components: transparent.
    assert_eq!(pix.get(5, 0), [0, 0, 0, 0]);
    // Inside gid 3's footprint at offset (8, 0): opaque black.
    assert_eq!(pix.get(8, 0), [0, 0, 0, 255]);
    assert_eq!(pix.get(9, 1), [0, 0, 0, 255]);
    // Outside gid 3's 2x2 footprint: transparent.
    assert_eq!(pix.get(10, 0), [0, 0, 0, 0]);
    assert_eq!(pix.get(8, 2), [0, 0, 0, 0]);
}

/// Builds an EBLC blob with one strike at `ppem` and N IndexSubTables,
/// each covering a single (start_gid..=end_gid) range with its own
/// image_format. `subtables` is one tuple per sub-table:
/// `(start_gid, end_gid, image_format, &[offsets...])`. The offsets
/// are u32 values written into a format-1 var-metric offset array
/// (count + 1 entries). All sub-tables share image_data_offset = 4 so
/// the offsets address bytes in EBDT past the version header.
fn build_eblc_three_subtables(ppem: u8, subtables: &[(u16, u16, u16, &[u32])]) -> Vec<u8> {
    let mut blob = Vec::new();
    blob.extend_from_slice(&2u16.to_be_bytes()); // major
    blob.extend_from_slice(&0u16.to_be_bytes()); // minor
    blob.extend_from_slice(&1u32.to_be_bytes()); // numSizes

    // BitmapSize record (48 B). It needs to know the global gid range
    // covered (min start, max end).
    let start_global = subtables.iter().map(|s| s.0).min().unwrap();
    let end_global = subtables.iter().map(|s| s.1).max().unwrap();

    let bitmap_size_off = blob.len() as u32;
    let index_array_off = bitmap_size_off + 48;
    let array_entry_bytes: u32 = 8 * subtables.len() as u32;
    // Each sub-table = 8 (header) + 4 * (count+1)
    let mut subtable_total: u32 = 0;
    for (s, e, _, _) in subtables {
        let count = u32::from(e - s + 1);
        subtable_total += 8 + 4 * (count + 1);
    }
    let index_tables_size = array_entry_bytes + subtable_total;
    blob.extend_from_slice(&index_array_off.to_be_bytes());
    blob.extend_from_slice(&index_tables_size.to_be_bytes());
    blob.extend_from_slice(&(subtables.len() as u32).to_be_bytes());
    blob.extend_from_slice(&0u32.to_be_bytes()); // colorRef
    blob.extend_from_slice(&[0u8; 12]); // hori metrics
    blob.extend_from_slice(&[0u8; 12]); // vert metrics
    blob.extend_from_slice(&start_global.to_be_bytes());
    blob.extend_from_slice(&end_global.to_be_bytes());
    blob.push(ppem);
    blob.push(ppem);
    blob.push(1);
    blob.push(0x01);

    // IndexSubTableArray: one entry per sub-table, sorted by first_gid.
    // additionalOffsetToIndexSubTable is relative to the array base.
    let mut additional: u32 = array_entry_bytes;
    for (s, e, _, _) in subtables {
        blob.extend_from_slice(&s.to_be_bytes());
        blob.extend_from_slice(&e.to_be_bytes());
        blob.extend_from_slice(&additional.to_be_bytes());
        let count = u32::from(e - s + 1);
        additional += 8 + 4 * (count + 1);
    }
    // IndexSubTable bodies.
    for (_, _, image_format, offsets) in subtables {
        blob.extend_from_slice(&1u16.to_be_bytes()); // index format
        blob.extend_from_slice(&image_format.to_be_bytes());
        blob.extend_from_slice(&4u32.to_be_bytes()); // image_data_offset
        for o in *offsets {
            blob.extend_from_slice(&o.to_be_bytes());
        }
    }
    blob
}

#[test]
fn ebdt_composite_self_reference_surfaces_decode_failed() {
    use sigilbuzz::{Blob, Face};
    use sigilbuzz_render::Rasterizer;

    // gid 1 composite references gid 1 itself.
    let parent = ebdt_fmt8_composite_entry(8, 4, &[(1, 0, 0)]);
    let ebdt = build_ebdt_multi(core::slice::from_ref(&parent));
    let eblc = build_eblc_three_subtables(16, &[(1, 1, 8, &[0, parent.len() as u32])]);
    let font = build_sfnt(vec![
        (*b"maxp", maxp_05(2)),
        (*b"EBLC", eblc),
        (*b"EBDT", ebdt),
    ]);
    let blob = Blob::new(&font);
    let face = Face::parse(&blob, 0).unwrap();
    let rast = Rasterizer::new();
    let err = rast
        .rasterize_bitmap_glyph(&face, 1, 16.0, &[])
        .expect_err("self-reference should fail");
    match err {
        RenderError::BitmapDecodeFailed(msg) => {
            assert!(
                msg.contains("self-reference") || msg.contains("cycle"),
                "unexpected message: {msg}"
            );
        }
        other => panic!("expected BitmapDecodeFailed, got {other:?}"),
    }
}

#[test]
fn ebdt_composite_two_glyph_cycle_surfaces_decode_failed() {
    use sigilbuzz::{Blob, Face};
    use sigilbuzz_render::Rasterizer;

    // gid 1 -> gid 2 -> gid 1. Both are composites.
    let parent1 = ebdt_fmt8_composite_entry(8, 4, &[(2, 0, 0)]);
    let parent2 = ebdt_fmt8_composite_entry(8, 4, &[(1, 0, 0)]);
    let ebdt = build_ebdt_multi(&[parent1.clone(), parent2.clone()]);
    let eblc = build_eblc_three_subtables(
        16,
        &[
            (1, 1, 8, &[0, parent1.len() as u32]),
            (
                2,
                2,
                8,
                &[parent1.len() as u32, (parent1.len() + parent2.len()) as u32],
            ),
        ],
    );
    let font = build_sfnt(vec![
        (*b"maxp", maxp_05(3)),
        (*b"EBLC", eblc),
        (*b"EBDT", ebdt),
    ]);
    let blob = Blob::new(&font);
    let face = Face::parse(&blob, 0).unwrap();
    let rast = Rasterizer::new();
    let err = rast
        .rasterize_bitmap_glyph(&face, 1, 16.0, &[])
        .expect_err("cycle should fail");
    assert!(matches!(err, RenderError::BitmapDecodeFailed(_)));
}

#[test]
fn ebdt_composite_oob_component_glyph_id_surfaces_decode_failed() {
    use sigilbuzz::{Blob, Face};
    use sigilbuzz_render::Rasterizer;

    // gid 1 references gid 99, which is well past maxp.numGlyphs.
    let parent = ebdt_fmt8_composite_entry(8, 4, &[(99, 0, 0)]);
    let ebdt = build_ebdt_multi(core::slice::from_ref(&parent));
    let eblc = build_eblc_three_subtables(16, &[(1, 1, 8, &[0, parent.len() as u32])]);
    let font = build_sfnt(vec![
        (*b"maxp", maxp_05(3)), // num_glyphs = 3, so 99 is OOB
        (*b"EBLC", eblc),
        (*b"EBDT", ebdt),
    ]);
    let blob = Blob::new(&font);
    let face = Face::parse(&blob, 0).unwrap();
    let rast = Rasterizer::new();
    let err = rast
        .rasterize_bitmap_glyph(&face, 1, 16.0, &[])
        .expect_err("OOB component should fail");
    match err {
        RenderError::BitmapDecodeFailed(msg) => {
            assert!(
                msg.contains("out of range") || msg.contains("missing"),
                "unexpected message: {msg}"
            );
        }
        other => panic!("expected BitmapDecodeFailed, got {other:?}"),
    }
}

#[test]
fn ebdt_placement_follows_the_strike_bearings() {
    // An 8x2 mask whose left edge is 2 pixels left of the origin and
    // whose top edge is 7 pixels above the baseline.
    let mut entry = vec![2, 8, (-2_i8) as u8, 7, 8];
    entry.extend_from_slice(&[0xFF, 0xFF]);
    let mut eblc = build_eblc_one_glyph(16, entry.len() as u32);
    // image_data_offset past the EBDT version header, as above.
    eblc[68..72].copy_from_slice(&4u32.to_be_bytes());
    let font = build_sfnt(vec![
        (*b"maxp", maxp_05(2)),
        (*b"EBLC", eblc),
        (*b"EBDT", build_ebdt(&entry)),
    ]);
    let face = Face::parse_bytes(&font, 0).unwrap();
    let rast = Rasterizer::new();
    let (pix, at) = rast
        .rasterize_bitmap_glyph_placed(&face, 1, 16.0, &[])
        .unwrap();
    assert_eq!((pix.width, pix.height), (8, 2));
    assert_eq!(at, sigilbuzz_render::Placement::new(-2, -7));
    // Resampled to twice the strike, the offset doubles with the image.
    let (pix, at) = rast
        .rasterize_bitmap_glyph_placed(&face, 1, 32.0, &[])
        .unwrap();
    assert_eq!((pix.width, pix.height), (16, 4));
    assert_eq!(at, sigilbuzz_render::Placement::new(-4, -14));
}
