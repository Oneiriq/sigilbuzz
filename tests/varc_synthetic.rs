//! Integration test for the VARC parser and Face::glyph_outline_at_coords
//! routing.
//!
//! Builds a synthetic SFNT in memory carrying:
//! - `head`, `maxp`, `hhea`, `hmtx`, `loca`, `glyf` (the minimum the
//!   TrueType outline path needs), and
//! - a `VARC` table that covers gid 1, with one component referencing
//!   gid 2 (a real glyph in `glyf`) and a translation transform.
//!
//! Then asserts that `Face::glyph_outline_at_coords(1)` returns the
//! gid-2 outline shifted by the VARC component's translation.
//!
//! `fontTools` 4.50+ has VARC support; for now we hand-pack the bytes
//! to keep CI free of Python dependencies — same posture as other
//! synthetic-font fixtures in `tests/`.

use sigilbuzz::tables::PathOp;
use sigilbuzz::{Blob, Face};

/// Pack one TableRecord as 16 BE bytes (tag, checksum=0, offset, length).
fn record(tag: [u8; 4], offset: u32, length: u32) -> [u8; 16] {
    let mut buf = [0u8; 16];
    buf[0..4].copy_from_slice(&tag);
    // checksum 0
    buf[8..12].copy_from_slice(&offset.to_be_bytes());
    buf[12..16].copy_from_slice(&length.to_be_bytes());
    buf
}

/// Builds a single simple glyph: a square with one contour. Returns
/// the raw glyf bytes and the point count for the loca entry.
fn build_square_glyph() -> Vec<u8> {
    // numberOfContours = 1, bbox = (0, 0, 100, 100)
    // endPtsOfContours = [3]
    // instructionLength = 0
    // 4 points, all on-curve, with absolute coordinates (0,0), (100,0),
    // (100,100), (0,100).
    let mut g = Vec::new();
    g.extend_from_slice(&1i16.to_be_bytes()); // numberOfContours
    g.extend_from_slice(&0i16.to_be_bytes()); // xMin
    g.extend_from_slice(&0i16.to_be_bytes()); // yMin
    g.extend_from_slice(&100i16.to_be_bytes()); // xMax
    g.extend_from_slice(&100i16.to_be_bytes()); // yMax
    g.extend_from_slice(&3u16.to_be_bytes()); // endPts[0] = 3
    g.extend_from_slice(&0u16.to_be_bytes()); // instructionLength
                                              // Per-point flags: ON_CURVE (0x01), X_SHORT (0x02), Y_SHORT (0x04).
                                              // Coords as signed bytes then.
                                              // Use repeat-flag for compactness? Keep it simple: 4 separate
                                              // flag bytes with X_SHORT|Y_SHORT|ON_CURVE = 0x07 — coords as
                                              // unsigned i8 abs values, X_SAME / Y_SAME bits decide sign.
                                              // For (0,0),(100,0),(100,100),(0,100), x deltas = 0,100,0,-100
                                              // and y deltas = 0,0,100,0.
                                              // Flag byte: bit0=ON_CURVE, bit1=X_SHORT, bit2=Y_SHORT,
                                              // bit4=X_SAME(if X_SHORT then sign), bit5=Y_SAME(same).
                                              // We'll use long-form coords (no shortcuts) by setting flags to
                                              // ON_CURVE only (0x01): then x and y read as i16 deltas after.
    g.extend_from_slice(&[0x01u8; 4]); // ON_CURVE, no short, no same — once per point
                                       // X deltas (i16): 0, 100, 0, -100
    for d in [0i16, 100, 0, -100] {
        g.extend_from_slice(&d.to_be_bytes());
    }
    // Y deltas (i16): 0, 0, 100, 0
    for d in [0i16, 0, 100, 0] {
        g.extend_from_slice(&d.to_be_bytes());
    }
    g
}

/// Builds a minimal VARC table that covers gid 1 with one component
/// referencing gid 2, translated by (50, 25), no axis variation.
fn build_minimal_varc() -> Vec<u8> {
    // Header: major=1, minor=0, offsets to coverage / varStore /
    // conditionList / axisIndicesList / glyphRecords.
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // major
    out.extend_from_slice(&0u16.to_be_bytes()); // minor
    let cov_slot = out.len();
    out.extend_from_slice(&0u32.to_be_bytes()); // coverage
    out.extend_from_slice(&0u32.to_be_bytes()); // varStore = none
    out.extend_from_slice(&0u32.to_be_bytes()); // conditionList = none
    out.extend_from_slice(&0u32.to_be_bytes()); // axisIndicesList = none
    let gr_slot = out.len();
    out.extend_from_slice(&0u32.to_be_bytes()); // glyphRecords

    // Coverage format 1: gid 1.
    let cov_off = out.len() as u32;
    out[cov_slot..cov_slot + 4].copy_from_slice(&cov_off.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes()); // format
    out.extend_from_slice(&1u16.to_be_bytes()); // glyphCount
    out.extend_from_slice(&1u16.to_be_bytes()); // gid 1

    // Build the one component record: HAVE_TRANSLATE_X | HAVE_TRANSLATE_Y
    // = 0x10 | 0x20 = 0x30. Single-byte uint32var.
    let mut rec = Vec::new();
    rec.push(0x30);
    rec.extend_from_slice(&2u16.to_be_bytes()); // gid 2
    rec.extend_from_slice(&50i16.to_be_bytes()); // tx
    rec.extend_from_slice(&25i16.to_be_bytes()); // ty

    // glyphRecords: CFF2 INDEX with 1 entry.
    let gr_off = out.len() as u32;
    out[gr_slot..gr_slot + 4].copy_from_slice(&gr_off.to_be_bytes());
    out.extend_from_slice(&1u32.to_be_bytes()); // count
    out.push(1); // off_size
    out.push(1); // offsets[0] = 1
    out.push(1u8 + rec.len() as u8); // offsets[1]
    out.extend_from_slice(&rec);
    out
}

/// Builds a minimal SFNT carrying head/maxp/hhea/hmtx/loca/glyf/VARC.
/// gid 0 is .notdef (empty), gid 1 is empty (VARC composite), gid 2
/// is a 100×100 square.
#[allow(clippy::too_many_lines)]
fn build_synthetic_font() -> Vec<u8> {
    // Build the substantive table payloads first.
    let head = {
        // 54 bytes of head, padded to a multiple of 4.
        let mut h = Vec::new();
        h.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // version 1.0
        h.extend_from_slice(&0u32.to_be_bytes()); // fontRevision
        h.extend_from_slice(&0u32.to_be_bytes()); // checkSumAdjustment
        h.extend_from_slice(&0x5F0F_3CF5u32.to_be_bytes()); // magicNumber
        h.extend_from_slice(&0u16.to_be_bytes()); // flags
        h.extend_from_slice(&1024u16.to_be_bytes()); // unitsPerEm
        h.extend_from_slice(&0u64.to_be_bytes()); // created
        h.extend_from_slice(&0u64.to_be_bytes()); // modified
        h.extend_from_slice(&0i16.to_be_bytes()); // xMin
        h.extend_from_slice(&0i16.to_be_bytes()); // yMin
        h.extend_from_slice(&200i16.to_be_bytes()); // xMax
        h.extend_from_slice(&200i16.to_be_bytes()); // yMax
        h.extend_from_slice(&0u16.to_be_bytes()); // macStyle
        h.extend_from_slice(&8u16.to_be_bytes()); // lowestRecPPEM
        h.extend_from_slice(&2i16.to_be_bytes()); // fontDirectionHint
        h.extend_from_slice(&0i16.to_be_bytes()); // indexToLocFormat = short
        h.extend_from_slice(&0i16.to_be_bytes()); // glyphDataFormat
        while h.len() % 4 != 0 {
            h.push(0);
        }
        h
    };

    let maxp = {
        // 0.5 (TrueType) just gives version + numGlyphs.
        let mut m = Vec::new();
        m.extend_from_slice(&0x0000_5000u32.to_be_bytes()); // version 0.5
        m.extend_from_slice(&3u16.to_be_bytes()); // numGlyphs
        while m.len() % 4 != 0 {
            m.push(0);
        }
        m
    };

    let hhea = {
        let mut h = Vec::new();
        h.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // version
        h.extend_from_slice(&800i16.to_be_bytes()); // ascent
        h.extend_from_slice(&(-200i16).to_be_bytes()); // descent
        h.extend_from_slice(&0i16.to_be_bytes()); // lineGap
        h.extend_from_slice(&500u16.to_be_bytes()); // advanceWidthMax
        h.extend_from_slice(&0i16.to_be_bytes()); // minLeftSideBearing
        h.extend_from_slice(&0i16.to_be_bytes()); // minRightSideBearing
        h.extend_from_slice(&500i16.to_be_bytes()); // xMaxExtent
        h.extend_from_slice(&1i16.to_be_bytes()); // caretSlopeRise
        h.extend_from_slice(&0i16.to_be_bytes()); // caretSlopeRun
        h.extend_from_slice(&0i16.to_be_bytes()); // caretOffset
        for _ in 0..4 {
            h.extend_from_slice(&0i16.to_be_bytes()); // reserved
        }
        h.extend_from_slice(&0i16.to_be_bytes()); // metricDataFormat
        h.extend_from_slice(&3u16.to_be_bytes()); // numberOfHMetrics
        while h.len() % 4 != 0 {
            h.push(0);
        }
        h
    };

    let hmtx = {
        let mut m = Vec::new();
        for _ in 0..3 {
            m.extend_from_slice(&500u16.to_be_bytes()); // advance
            m.extend_from_slice(&0i16.to_be_bytes()); // lsb
        }
        while m.len() % 4 != 0 {
            m.push(0);
        }
        m
    };

    // glyf: gid 0 empty, gid 1 empty (composite via VARC, no glyf
    // body), gid 2 the square. loca short format means offsets are
    // halves of the byte offset.
    let square = build_square_glyph();
    // Pad each glyph to a 2-byte boundary (loca short).
    let mut glyf = Vec::new();
    let off0 = glyf.len();
    // gid 0 empty — no bytes, loca[0] = loca[1] indicates empty.
    let off1 = glyf.len();
    // gid 1 empty
    let off2 = glyf.len();
    glyf.extend_from_slice(&square);
    while glyf.len() % 2 != 0 {
        glyf.push(0);
    }
    let off3 = glyf.len();
    while glyf.len() % 4 != 0 {
        glyf.push(0);
    }

    let loca = {
        // short format: 4 u16 offsets (numGlyphs+1).
        let mut l = Vec::new();
        for off in [off0, off1, off2, off3] {
            l.extend_from_slice(&((off / 2) as u16).to_be_bytes());
        }
        while l.len() % 4 != 0 {
            l.push(0);
        }
        l
    };

    let varc = {
        let mut v = build_minimal_varc();
        while v.len() % 4 != 0 {
            v.push(0);
        }
        v
    };

    // Assemble in alphabetical-tag order with a 12-byte SFNT header
    // and 7 × 16-byte table records.
    let num_tables = 7u16;
    let header_len = 12 + num_tables as usize * 16;
    let payloads: Vec<([u8; 4], &[u8])> = vec![
        (*b"VARC", varc.as_slice()),
        (*b"glyf", glyf.as_slice()),
        (*b"head", head.as_slice()),
        (*b"hhea", hhea.as_slice()),
        (*b"hmtx", hmtx.as_slice()),
        (*b"loca", loca.as_slice()),
        (*b"maxp", maxp.as_slice()),
    ];

    let mut out = Vec::with_capacity(header_len);
    // SFNT header: TrueType version 1.0
    out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    out.extend_from_slice(&num_tables.to_be_bytes());
    // searchRange / entrySelector / rangeShift — uninspected by Face.
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());

    // Compute payload offsets.
    let mut cursor = header_len as u32;
    let mut directory: Vec<[u8; 16]> = Vec::with_capacity(payloads.len());
    for (tag, body) in &payloads {
        directory.push(record(*tag, cursor, body.len() as u32));
        cursor += body.len() as u32;
    }
    for d in &directory {
        out.extend_from_slice(d);
    }
    for (_, body) in &payloads {
        out.extend_from_slice(body);
    }
    out
}

#[test]
fn varc_routes_outline_through_composite() {
    let bytes = build_synthetic_font();
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();

    // gid 2 standalone: returns the square at its native location.
    let gid2_outline = face.glyph_outline(2).unwrap().expect("gid 2 outline");
    let gid2_pts: Vec<(f32, f32)> = gid2_outline
        .ops()
        .iter()
        .filter_map(|op| match op {
            PathOp::MoveTo { x, y } | PathOp::LineTo { x, y } => Some((*x, *y)),
            _ => None,
        })
        .collect();
    assert_eq!(
        gid2_pts.first().copied(),
        Some((0.0, 0.0)),
        "expected square to start at origin"
    );

    // gid 1 is VARC-covered with one component (gid 2) translated by
    // (50, 25). Outline should be the same square, shifted.
    let gid1_outline = face.glyph_outline(1).unwrap().expect("gid 1 outline");
    let gid1_pts: Vec<(f32, f32)> = gid1_outline
        .ops()
        .iter()
        .filter_map(|op| match op {
            PathOp::MoveTo { x, y } | PathOp::LineTo { x, y } => Some((*x, *y)),
            _ => None,
        })
        .collect();

    assert_eq!(gid2_pts.len(), gid1_pts.len(), "same point count");
    for ((sx, sy), (cx, cy)) in gid2_pts.iter().zip(gid1_pts.iter()) {
        assert!((cx - (sx + 50.0)).abs() < 1e-3, "x mismatch: {sx} -> {cx}");
        assert!((cy - (sy + 25.0)).abs() < 1e-3, "y mismatch: {sy} -> {cy}");
    }
}

#[test]
fn varc_accessor_returns_table_when_present() {
    let bytes = build_synthetic_font();
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    let varc = face.varc().unwrap().expect("VARC table");
    assert!(varc.covers(1));
    assert!(!varc.covers(2));
    assert_eq!(varc.glyph_record_count(), 1);
}

#[test]
fn varc_uncovered_gid_falls_through_to_glyf() {
    let bytes = build_synthetic_font();
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    // gid 2 is not VARC-covered → comes straight from glyf.
    let outline = face.glyph_outline(2).unwrap().expect("outline");
    assert!(!outline.is_empty(), "square should produce ops");
}
