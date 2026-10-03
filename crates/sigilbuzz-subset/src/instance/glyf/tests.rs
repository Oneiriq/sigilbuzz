//! Unit tests for the glyf bake: composite offsets, phantom metrics,
//! and the simple glyph codec.

use super::*;
use alloc::vec;

/// A composite glyph body: a zero header, then one record per
/// `(flags, glyph, arg1, arg2)`, the arguments as words when the
/// flags say so and as bytes otherwise, with `MORE_COMPONENTS` set on
/// every record but the last.
fn composite(records: &[(u16, u16, i16, i16)]) -> Vec<u8> {
    let mut body = vec![0xFF, 0xFF, 0, 0, 0, 0, 0, 0, 0, 0];
    for (i, &(flags, glyph, a, b)) in records.iter().enumerate() {
        let more = if i + 1 < records.len() {
            COMP_MORE_COMPONENTS
        } else {
            0
        };
        body.extend_from_slice(&(flags | more).to_be_bytes());
        body.extend_from_slice(&glyph.to_be_bytes());
        if flags & COMP_ARG_1_AND_2_ARE_WORDS != 0 {
            body.extend_from_slice(&a.to_be_bytes());
            body.extend_from_slice(&b.to_be_bytes());
        } else {
            body.push(a as u8);
            body.push(b as u8);
        }
    }
    body
}

const XY: u16 = COMP_ARGS_ARE_XY_VALUES;

#[test]
fn component_offsets_move_by_their_deltas_and_round_halves_up() {
    let body = composite(&[(XY, 1, 10, -20), (XY, 2, -5, 7)]);
    let records = read_component_records(&body).unwrap();
    assert_eq!(
        records
            .iter()
            .map(CompRecord::gvar_point)
            .collect::<Vec<_>>(),
        vec![(10, -20), (-5, 7)]
    );
    let out = rewrite_components(&body, &records, &[(2.5, -0.5), (-2.5, 0.4)]);
    let moved = read_component_records(&out).unwrap();
    // 12.5 rounds to 13, -20.5 to -20, -7.5 to -7, 7.4 to 7.
    assert_eq!(
        moved.iter().map(CompRecord::gvar_point).collect::<Vec<_>>(),
        vec![(13, -20), (-7, 7)]
    );
    assert_eq!(out.len(), body.len(), "byte arguments still fit");
}

#[test]
fn byte_arguments_widen_to_words_when_the_offset_outgrows_them() {
    let body = composite(&[(XY, 1, 120, 0), (XY, 2, 3, 4)]);
    let records = read_component_records(&body).unwrap();
    let out = rewrite_components(&body, &records, &[(20.0, -200.0), (0.0, 0.0)]);
    let moved = read_component_records(&out).unwrap();
    assert_eq!(moved[0].gvar_point(), (140, -200));
    assert_ne!(moved[0].flags & COMP_ARG_1_AND_2_ARE_WORDS, 0);
    assert_eq!(moved[0].flags & COMP_MORE_COMPONENTS, COMP_MORE_COMPONENTS);
    assert_eq!(moved[1].gvar_point(), (3, 4));
    assert_eq!(out.len(), body.len() + 2);
}

#[test]
fn anchored_components_and_trailing_instructions_are_copied() {
    // An anchored component (points 3 and 4), then one with a scale
    // and words, then the instructions of the last record.
    let mut body = composite(&[(0, 1, 3, 4)]);
    body[10..12].copy_from_slice(&COMP_MORE_COMPONENTS.to_be_bytes());
    let flags = XY | COMP_ARG_1_AND_2_ARE_WORDS | COMP_WE_HAVE_A_SCALE | 0x0100;
    body.extend_from_slice(&flags.to_be_bytes());
    body.extend_from_slice(&2u16.to_be_bytes());
    body.extend_from_slice(&300i16.to_be_bytes());
    body.extend_from_slice(&(-300i16).to_be_bytes());
    body.extend_from_slice(&0x2000u16.to_be_bytes()); // scale 0.5
    body.extend_from_slice(&[0, 2, 0xB0, 0x01]); // two instruction bytes
    let records = read_component_records(&body).unwrap();
    assert_eq!(records[0].gvar_point(), (0, 0), "anchored: no offset");
    let out = rewrite_components(&body, &records, &[(50.0, 50.0), (1.0, -1.0)]);
    let moved = read_component_records(&out).unwrap();
    assert_eq!(&out[10..16], &body[10..16], "anchored record unchanged");
    assert_eq!(moved[1].gvar_point(), (301, -301));
    assert_eq!(&out[out.len() - 6..], &[0x20, 0x00, 0, 2, 0xB0, 0x01]);
}

#[test]
fn metrics_come_from_the_phantom_points_and_the_new_bounds() {
    // Phantom points: left origin -12.4, advance origin 600.6, top 880,
    // bottom -120.5.
    let pp = [(-12.4, 0.0), (600.6, 0.0), (0.0, 880.0), (0.0, -120.5)];
    let m = metrics_from(&pp, Some([30, -10, 500, 700]));
    assert_eq!(m.advance, 613); // 613.0
    assert_eq!(m.lsb, 42); // 30 - (-12.4) = 42.4
    assert_eq!(m.v_advance, 1001); // 1000.5 rounds up
    assert_eq!(m.tsb, 180);
    // An empty glyph measures its bearings from zero.
    let m = metrics_from(&pp, None);
    assert_eq!((m.lsb, m.tsb, m.bounds), (12, 880, None));
    // A negative advance clamps to zero.
    let crossed = [(10.0, 0.0), (4.0, 0.0), (0.0, 0.0), (0.0, 5.0)];
    let m = metrics_from(&crossed, None);
    assert_eq!((m.advance, m.v_advance), (0, 0));
}

#[test]
fn simple_glyphs_keep_on_curve_and_overlap_bits_and_drop_hints() {
    // Two points, the first flagged on curve and OVERLAP_SIMPLE, with a
    // one-byte instruction stream.
    let mut body = Vec::new();
    body.extend_from_slice(&1i16.to_be_bytes());
    body.extend_from_slice(&[0; 8]);
    body.extend_from_slice(&1u16.to_be_bytes()); // endPtsOfContours
    body.extend_from_slice(&1u16.to_be_bytes()); // instructionLength
    body.push(0xB0);
    body.push(0x41 | 0x02 | 0x04 | 0x10 | 0x20); // on curve, overlap, +x +y bytes
    body.push(0x00); // off curve, x and y words
                     // The x stream (+10, then +300), then the y stream (+20, then -40).
    body.push(10);
    body.extend_from_slice(&300i16.to_be_bytes());
    body.push(20);
    body.extend_from_slice(&(-40i16).to_be_bytes());
    let glyph = SimpleGlyph::decode(&body).unwrap();
    assert_eq!(glyph.points(), vec![(10, 20), (310, -20)]);
    let (baked, bounds) = encode_baked_simple(&glyph, &[(0.5, -0.5), (-0.5, 0.25)]);
    assert_eq!(bounds, Some([11, -20, 310, 20]));
    let again = SimpleGlyph::decode(&baked).unwrap();
    // 10.5 and 309.5 round up, 19.5 too, and -19.75 to -20.
    assert_eq!(again.points(), vec![(11, 20), (310, -20)]);
    assert_eq!(again.flags[0] & 0xC1, 0x41);
    assert_eq!(again.flags[1] & 0xC1, 0x00);
    // No instructions survive.
    assert_eq!(&baked[12..14], &[0, 0]);
}

#[test]
fn round_half_up_rounds_toward_positive_infinity_on_ties() {
    use crate::util::round_half_up;
    assert_eq!(round_half_up(2.5), 3);
    assert_eq!(round_half_up(-2.5), -2);
    assert_eq!(round_half_up(-2.6), -3);
    assert_eq!(round_half_up(-0.4), 0);
    assert_eq!(round_half_up(f32::NAN), 0);
    assert_eq!(round_half_up(1.0e12), i32::MAX);
    assert_eq!(round_half_up(-1.0e12), i32::MIN);
}
