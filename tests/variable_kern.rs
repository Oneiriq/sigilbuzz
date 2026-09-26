//! Variable-font kerning: GPOS pair adjustments whose ValueRecords
//! carry VariationIndex device offsets.
//!
//! The (A, V) pair in `var_kern.ttf` pulls a -100 advance delta off
//! GDEF's ItemVariationStore at wght=900 and zero at the default
//! wght=400. This is the integration cover for issue #13: before the
//! GPOS feature-variations wiring, sigilbuzz parsed the VariationIndex
//! offsets but threw them away, so kerning was frozen at the default
//! instance.
//!
//! The fixture is built by [`fixture::build`] below and regenerated
//! with
//!
//! ```text
//! cargo test --test variable_kern -- --ignored regenerate_var_kern_fixture
//! ```
//!
//! `committed_fixture_matches_the_builder` fails when the two drift.
//!
//! # Device offset base
//!
//! A PairPos format 1 PairValueRecord measures its Device /
//! VariationIndex offsets from the start of its PairSet table, as the
//! OpenType spec and HarfBuzz (`PairSet::apply` passes `this`) do.
//! The fixture follows that rule. Its earlier Python-built version
//! measured from the PairPos subtable, which only worked because the
//! shaper resolved from the subtable too.
//!
//! # Why no byte-for-byte rustybuzz parity here
//!
//! rustybuzz 0.20 (through ttf-parser 0.25) measures those offsets
//! from two bytes into the PairSet, just past its record count, so it
//! misses the VariationIndex in this fixture and in Rubik alike and
//! keeps the default-instance kerning at every weight. The Rubik test
//! below checks against deltas computed with ttf-parser's own
//! ItemVariationStore reader instead.

use sigilbuzz::{shape, Blob, Buffer, Face, Feature, Font};

/// Built by [`fixture::build`]: two glyphs ("A" and "V"), one `wght`
/// axis (400 to 900), and a GPOS kern pair whose x_advance delta is
/// -100 at wght=900 and 0 at wght=400 via a VariationIndex into
/// GDEF's ItemVariationStore.
const VAR_KERN: &[u8] = include_bytes!("fixtures/var_kern.ttf");
const RUBIK: &[u8] = include_bytes!("fixtures/rubik_vf.ttf");

fn normalize_wght(face: &Face<'_>, user_value: f32) -> Vec<f32> {
    let fvar = face.fvar().unwrap().expect("synthetic font has fvar");
    let avar = face.avar().unwrap();
    let normalized = fvar.normalize_coords(&[user_value]);
    match avar {
        Some(a) => a.remap_all(&normalized),
        None => normalized,
    }
}

fn advances(bytes: &[u8], coords: &[f32], text: &str, features: &[Feature]) -> Vec<i32> {
    let blob = Blob::new(bytes);
    let face = Face::parse(&blob, 0).unwrap();
    let font = Font::new(face, 1000.0).with_coords(coords);
    let mut buf = Buffer::new();
    buf.push_str(text);
    let shaped = shape(&font, &buf, features).unwrap();
    shaped.glyphs.iter().map(|g| g.x_advance).collect()
}

fn sigilbuzz_advances(coords: &[f32], text: &str) -> Vec<i32> {
    advances(VAR_KERN, coords, text, &[])
}

#[test]
fn committed_fixture_matches_the_builder() {
    assert!(
        fixture::build() == VAR_KERN,
        "tests/fixtures/var_kern.ttf is stale; regenerate it with \
         `cargo test --test variable_kern -- --ignored regenerate_var_kern_fixture`"
    );
}

#[test]
#[ignore = "writes tests/fixtures/var_kern.ttf; run explicitly to regenerate"]
fn regenerate_var_kern_fixture() {
    std::fs::write("tests/fixtures/var_kern.ttf", fixture::build()).unwrap();
}

#[test]
fn default_instance_kern_delta_is_zero() {
    // At the default wght (400) the VariationIndex resolves to a
    // zero delta, so "AV" emits the same advances as the bare
    // hmtx: 500 for A, 500 for V.
    let advances = sigilbuzz_advances(&[], "AV");
    assert_eq!(advances.len(), 2);
    assert_eq!(advances[0], 500, "A at default wght");
    assert_eq!(advances[1], 500, "V at default wght");
}

#[test]
fn heavy_weight_tightens_av_pair_by_a_hundred_units() {
    // At wght = 900 the variation region peaks -> delta = -100
    // lands on the first glyph's x_advance.
    let blob = Blob::new(VAR_KERN);
    let face = Face::parse(&blob, 0).unwrap();
    let coords = normalize_wght(&face, 900.0);
    let advances = sigilbuzz_advances(&coords, "AV");
    assert_eq!(advances.len(), 2);
    assert_eq!(advances[0], 400, "A at wght=900 gets the -100 kern delta");
    assert_eq!(advances[1], 500, "V unchanged (valueFormat2 = empty)");
}

#[test]
fn halfway_axis_coord_gives_halfway_delta() {
    // The variation region is (start=0, peak=1, end=1) so at a
    // normalized coord of 0.5 the scalar is 0.5 and the delta is
    // -50. wght 400 .. 900 user-space -> halfway is 650.
    let blob = Blob::new(VAR_KERN);
    let face = Face::parse(&blob, 0).unwrap();
    let coords = normalize_wght(&face, 650.0);
    let advances = sigilbuzz_advances(&coords, "AV");
    assert_eq!(advances[0], 450, "halfway on wght pulls half the delta");
}

#[test]
fn advance_delta_scales_monotonically_across_axis() {
    // Sanity: as the wght coordinate grows, the kern tightens. Any
    // regression to "kern frozen at default" would make every
    // sample return the same advance.
    let blob = Blob::new(VAR_KERN);
    let face = Face::parse(&blob, 0).unwrap();
    let sample = |w: f32| {
        let coords = normalize_wght(&face, w);
        sigilbuzz_advances(&coords, "AV")[0]
    };
    let a0 = sample(400.0);
    let a1 = sample(550.0);
    let a2 = sample(700.0);
    let a3 = sample(900.0);
    assert_eq!(a0, 500);
    assert!(a1 < a0, "wght 550 should tighten vs default");
    assert!(a2 < a1, "wght 700 should tighten further");
    assert!(a3 < a2, "wght 900 should tighten the most");
    assert_eq!(a3, 400);
}

/// Rubik VF kerns Cyrillic И before ")" through a PairPos format 1
/// record: x_advance -11 plus an x-advance VariationIndex (outer 0,
/// inner 79) measured from its PairSet. The expected delta comes from
/// ttf-parser's ItemVariationStore reader, independent of sigilbuzz's;
/// the kern contribution is isolated by shaping with `kern` on and
/// off at the same weight.
#[test]
fn rubik_pair_set_device_offsets_follow_the_weight_axis() {
    let blob = Blob::new(RUBIK);
    let face = Face::parse(&blob, 0).unwrap();
    let ttf = ttf_parser::Face::parse(RUBIK, 0).unwrap();
    let gdef = ttf.tables().gdef.unwrap();
    let no_kern = [Feature {
        tag: *b"kern",
        value: 0,
    }];
    for wght in [300.0f32, 600.0, 750.0, 900.0] {
        let coords = normalize_wght(&face, wght);
        let normalized: Vec<ttf_parser::NormalizedCoordinate> = coords
            .iter()
            .map(|&c| ttf_parser::NormalizedCoordinate::from(c))
            .collect();
        let delta = gdef
            .glyph_variation_delta(0, 79, &normalized)
            .expect("VariationIndex 0/79 resolves");
        let expected = -11 + delta.round() as i32;
        let kerned = advances(RUBIK, &coords, "\u{0418})", &[]);
        let plain = advances(RUBIK, &coords, "\u{0418})", &no_kern);
        assert_eq!(
            kerned[0] - plain[0],
            expected,
            "wght {wght}: kern must be -11 plus the VariationIndex delta {delta}"
        );
    }
}

/// Builder for `tests/fixtures/var_kern.ttf`.
mod fixture {
    fn be16(out: &mut Vec<u8>, v: u16) {
        out.extend_from_slice(&v.to_be_bytes());
    }

    fn be32(out: &mut Vec<u8>, v: u32) {
        out.extend_from_slice(&v.to_be_bytes());
    }

    const UPEM: u16 = 1000;
    const GID_A: u16 = 1;
    const GID_V: u16 = 2;

    fn head() -> Vec<u8> {
        let mut t = Vec::new();
        be32(&mut t, 0x0001_0000); // version
        be32(&mut t, 0x0001_0000); // fontRevision
        be32(&mut t, 0); // checkSumAdjustment, patched by `build`
        be32(&mut t, 0x5F0F_3CF5); // magicNumber
        be16(&mut t, 0x0003); // flags: baseline and lsb at 0
        be16(&mut t, UPEM);
        t.extend_from_slice(&[0; 16]); // created, modified
        for v in [0u16, 0, 500, 1000] {
            be16(&mut t, v); // xMin, yMin, xMax, yMax
        }
        be16(&mut t, 0); // macStyle
        be16(&mut t, 8); // lowestRecPPEM
        be16(&mut t, 2); // fontDirectionHint
        be16(&mut t, 0); // indexToLocFormat: short
        be16(&mut t, 0); // glyphDataFormat
        t
    }

    fn hhea() -> Vec<u8> {
        let mut t = Vec::new();
        be32(&mut t, 0x0001_0000);
        be16(&mut t, 800); // ascender
        be16(&mut t, (-200i16) as u16); // descender
        be16(&mut t, 0); // lineGap
        be16(&mut t, 500); // advanceWidthMax
        be16(&mut t, 0); // minLeftSideBearing
        be16(&mut t, 0); // minRightSideBearing
        be16(&mut t, 500); // xMaxExtent
        be16(&mut t, 1); // caretSlopeRise
        be16(&mut t, 0); // caretSlopeRun
        t.extend_from_slice(&[0; 10]); // caretOffset + 4 reserved
        be16(&mut t, 0); // metricDataFormat
        be16(&mut t, 3); // numberOfHMetrics
        t
    }

    fn maxp() -> Vec<u8> {
        let mut t = Vec::new();
        be32(&mut t, 0x0001_0000);
        be16(&mut t, 3); // numGlyphs
        be16(&mut t, 4); // maxPoints
        be16(&mut t, 1); // maxContours
        be16(&mut t, 0); // maxCompositePoints
        be16(&mut t, 0); // maxCompositeContours
        be16(&mut t, 2); // maxZones
        t.extend_from_slice(&[0; 16]); // remaining limits
        t
    }

    fn hmtx() -> Vec<u8> {
        let mut t = Vec::new();
        for _ in 0..3 {
            be16(&mut t, 500);
            be16(&mut t, 0);
        }
        t
    }

    /// cmap with one format 4 subtable: A -> 1, V -> 2.
    fn cmap() -> Vec<u8> {
        let segs: [(u16, u16); 3] = [
            (u16::from(b'A'), GID_A),
            (u16::from(b'V'), GID_V),
            (0xFFFF, 0),
        ];
        let mut sub = Vec::new();
        be16(&mut sub, 4); // format
        be16(&mut sub, 16 + 8 * segs.len() as u16); // length
        be16(&mut sub, 0); // language
        be16(&mut sub, 2 * segs.len() as u16); // segCountX2
        be16(&mut sub, 4); // searchRange
        be16(&mut sub, 1); // entrySelector
        be16(&mut sub, 2); // rangeShift
        for (code, _) in segs {
            be16(&mut sub, code); // endCode
        }
        be16(&mut sub, 0); // reservedPad
        for (code, _) in segs {
            be16(&mut sub, code); // startCode
        }
        for (code, gid) in segs {
            // idDelta; the final segment maps 0xFFFF to glyph 0.
            let delta = if code == 0xFFFF {
                1
            } else {
                gid.wrapping_sub(code)
            };
            be16(&mut sub, delta);
        }
        for _ in segs {
            be16(&mut sub, 0); // idRangeOffset
        }
        let mut t = Vec::new();
        be16(&mut t, 0); // version
        be16(&mut t, 1); // numTables
        be16(&mut t, 3); // platform: Windows
        be16(&mut t, 1); // encoding: Unicode BMP
        be32(&mut t, 12);
        t.extend_from_slice(&sub);
        t
    }

    /// One rectangle contour per glyph: `.notdef` 400x700, A and V
    /// 500x1000.
    fn glyf_loca() -> (Vec<u8>, Vec<u8>) {
        let mut glyf = Vec::new();
        let mut loca = Vec::new();
        for (w, h) in [(400i16, 700i16), (500, 1000), (500, 1000)] {
            be16(&mut loca, (glyf.len() / 2) as u16);
            be16(&mut glyf, 1); // numberOfContours
            for v in [0, 0, w, h] {
                be16(&mut glyf, v as u16); // bbox
            }
            be16(&mut glyf, 3); // endPtsOfContours[0]
            be16(&mut glyf, 0); // instructionLength
            glyf.extend_from_slice(&[0x01; 4]); // on-curve, i16 deltas
            for dx in [0, w, 0, -w] {
                be16(&mut glyf, dx as u16);
            }
            for dy in [0, 0, h, 0] {
                be16(&mut glyf, dy as u16);
            }
        }
        be16(&mut loca, (glyf.len() / 2) as u16);
        (glyf, loca)
    }

    /// fvar with one `wght` axis, 400 to 900, default 400.
    fn fvar() -> Vec<u8> {
        let mut t = Vec::new();
        be16(&mut t, 1); // majorVersion
        be16(&mut t, 0); // minorVersion
        be16(&mut t, 16); // axesArrayOffset
        be16(&mut t, 2); // reserved
        be16(&mut t, 1); // axisCount
        be16(&mut t, 20); // axisSize
        be16(&mut t, 0); // instanceCount
        be16(&mut t, 8); // instanceSize
        t.extend_from_slice(b"wght");
        for v in [400u32, 400, 900] {
            be32(&mut t, v << 16); // min, default, max (Fixed)
        }
        be16(&mut t, 0); // flags
        be16(&mut t, 256); // axisNameID
        t
    }

    /// GDEF 1.3 with no class tables and an ItemVariationStore of one
    /// region (peak at wght +1.0) and one delta row: -100.
    fn gdef() -> Vec<u8> {
        let mut ivs = Vec::new();
        be16(&mut ivs, 1); // format
        be32(&mut ivs, 12); // variationRegionListOffset
        be16(&mut ivs, 1); // itemVariationDataCount
        be32(&mut ivs, 22); // itemVariationDataOffsets[0]
        be16(&mut ivs, 1); // axisCount
        be16(&mut ivs, 1); // regionCount
        for v in [0u16, 0x4000, 0x4000] {
            be16(&mut ivs, v); // start, peak, end (F2DOT14)
        }
        be16(&mut ivs, 1); // itemCount
        be16(&mut ivs, 1); // wordDeltaCount
        be16(&mut ivs, 1); // regionIndexCount
        be16(&mut ivs, 0); // regionIndexes[0]
        be16(&mut ivs, (-100i16) as u16); // delta
        let mut t = Vec::new();
        be16(&mut t, 1); // majorVersion
        be16(&mut t, 3); // minorVersion
        t.extend_from_slice(&[0; 10]); // five null subtable offsets
        be32(&mut t, 18); // itemVarStoreOffset
        t.extend_from_slice(&ivs);
        t
    }

    /// PairPos format 1: first glyph A, one PairSet holding (A, V) with
    /// x_advance 0 and an x-advance VariationIndex (0, 0). The device
    /// offset is measured from the PairSet, as the spec requires.
    fn pair_pos() -> Vec<u8> {
        const PAIR_SET: u16 = 12;
        // PairSet: count, secondGlyph, xAdvance, xAdvDevice.
        const PAIR_SET_LEN: u16 = 8;
        const COVERAGE: u16 = PAIR_SET + PAIR_SET_LEN;
        const COVERAGE_LEN: u16 = 6;
        const VARIATION_INDEX: u16 = COVERAGE + COVERAGE_LEN;
        let mut t = Vec::new();
        be16(&mut t, 1); // posFormat
        be16(&mut t, COVERAGE);
        be16(&mut t, 0x0044); // valueFormat1: X_ADVANCE | X_ADVANCE_DEVICE
        be16(&mut t, 0); // valueFormat2
        be16(&mut t, 1); // pairSetCount
        be16(&mut t, PAIR_SET);
        be16(&mut t, 1); // pairValueCount
        be16(&mut t, GID_V); // secondGlyph
        be16(&mut t, 0); // xAdvance
        be16(&mut t, VARIATION_INDEX - PAIR_SET); // xAdvDevice
        be16(&mut t, 1); // Coverage format 1
        be16(&mut t, 1);
        be16(&mut t, GID_A);
        be16(&mut t, 0); // deltaSetOuterIndex
        be16(&mut t, 0); // deltaSetInnerIndex
        be16(&mut t, 0x8000); // deltaFormat: VariationIndex
        t
    }

    /// GPOS 1.0: DFLT script, one `kern` feature, one lookup.
    fn gpos() -> Vec<u8> {
        let mut script_list = Vec::new();
        be16(&mut script_list, 1);
        script_list.extend_from_slice(b"DFLT");
        be16(&mut script_list, 8); // Script
        be16(&mut script_list, 4); // defaultLangSys
        be16(&mut script_list, 0); // langSysCount
        be16(&mut script_list, 0); // lookupOrderOffset
        be16(&mut script_list, 0xFFFF); // requiredFeatureIndex
        be16(&mut script_list, 1); // featureIndexCount
        be16(&mut script_list, 0);

        let mut feature_list = Vec::new();
        be16(&mut feature_list, 1);
        feature_list.extend_from_slice(b"kern");
        be16(&mut feature_list, 8); // Feature
        be16(&mut feature_list, 0); // featureParams
        be16(&mut feature_list, 1); // lookupIndexCount
        be16(&mut feature_list, 0);

        let mut lookup_list = Vec::new();
        be16(&mut lookup_list, 1);
        be16(&mut lookup_list, 4); // Lookup
        be16(&mut lookup_list, 2); // lookupType: pair adjustment
        be16(&mut lookup_list, 0); // lookupFlag
        be16(&mut lookup_list, 1); // subTableCount
        be16(&mut lookup_list, 8); // subtableOffsets[0]
        lookup_list.extend_from_slice(&pair_pos());

        let mut t = Vec::new();
        be16(&mut t, 1);
        be16(&mut t, 0);
        let script_off = 10;
        let feature_off = script_off + script_list.len();
        let lookup_off = feature_off + feature_list.len();
        be16(&mut t, script_off as u16);
        be16(&mut t, feature_off as u16);
        be16(&mut t, lookup_off as u16);
        t.extend_from_slice(&script_list);
        t.extend_from_slice(&feature_list);
        t.extend_from_slice(&lookup_list);
        t
    }

    fn checksum(bytes: &[u8]) -> u32 {
        bytes.chunks(4).fold(0u32, |sum, chunk| {
            let mut word = [0u8; 4];
            word[..chunk.len()].copy_from_slice(chunk);
            sum.wrapping_add(u32::from_be_bytes(word))
        })
    }

    /// The whole font: tables sorted by tag, each 4-byte aligned, with
    /// table checksums and `head.checkSumAdjustment` filled in.
    pub fn build() -> Vec<u8> {
        let (glyf, loca) = glyf_loca();
        let tables: [([u8; 4], Vec<u8>); 10] = [
            (*b"GDEF", gdef()),
            (*b"GPOS", gpos()),
            (*b"cmap", cmap()),
            (*b"fvar", fvar()),
            (*b"glyf", glyf),
            (*b"head", head()),
            (*b"hhea", hhea()),
            (*b"hmtx", hmtx()),
            (*b"loca", loca),
            (*b"maxp", maxp()),
        ];
        let mut out = Vec::new();
        be32(&mut out, 0x0001_0000); // sfntVersion
        be16(&mut out, tables.len() as u16);
        be16(&mut out, 128); // searchRange: 8 tables * 16
        be16(&mut out, 3); // entrySelector
        be16(&mut out, (tables.len() as u16) * 16 - 128); // rangeShift
        let mut offset = 12 + 16 * tables.len();
        let mut head_at = 0;
        for (tag, body) in &tables {
            if tag == b"head" {
                head_at = offset;
            }
            out.extend_from_slice(tag);
            be32(&mut out, checksum(body));
            be32(&mut out, offset as u32);
            be32(&mut out, body.len() as u32);
            offset += body.len().next_multiple_of(4);
        }
        for (_, body) in &tables {
            out.extend_from_slice(body);
            out.resize(out.len().next_multiple_of(4), 0);
        }
        let adjustment = 0xB1B0_AFBAu32.wrapping_sub(checksum(&out));
        out[head_at + 8..head_at + 12].copy_from_slice(&adjustment.to_be_bytes());
        out
    }
}
