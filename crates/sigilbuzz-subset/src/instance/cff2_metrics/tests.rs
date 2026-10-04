//! Tests for the CFF2 instance metrics.

use super::*;

/// The extents of an outline drawn into a fresh box.
fn extents_of(draw: impl FnOnce(&mut ControlBox)) -> Extents {
    let mut sink = ControlBox::new();
    draw(&mut sink);
    sink.extents()
}

#[test]
fn the_box_takes_control_points_and_rounds_its_ends() {
    let e = extents_of(|s| {
        s.move_to(10.4, 0.0);
        s.curve_to(-2.5, 50.0, 30.0, 120.5, 40.5, 20.0);
        s.close();
    });
    // x from -2.5 (a control point) to 40.5, y from 0 to 120.5: each
    // end rounds halves up, so -2.5 goes to -2.
    assert_eq!(
        e,
        Extents {
            x_bearing: -2,
            width: 41 + 2,
            y_bearing: 121,
            height: -121,
        }
    );
}

#[test]
fn a_move_without_a_segment_adds_nothing() {
    let e = extents_of(|s| {
        s.move_to(500.0, 500.0);
        s.move_to(0.0, 0.0);
        s.line_to(10.0, 20.0);
        s.move_to(-900.0, -900.0);
        s.close();
    });
    assert_eq!(
        e,
        Extents {
            x_bearing: 0,
            width: 10,
            y_bearing: 20,
            height: -20,
        }
    );
    assert!(!extents_of(|s| s.move_to(5.0, 5.0)).has_bounds());
}

#[test]
fn a_flat_side_has_no_extent_on_that_axis() {
    // A vertical line: no width, so HarfBuzz gives x a zero bearing
    // and width, while y keeps its extent.
    let e = extents_of(|s| {
        s.move_to(30.0, 0.0);
        s.line_to(30.0, 700.0);
    });
    assert_eq!(
        e,
        Extents {
            x_bearing: 0,
            width: 0,
            y_bearing: 700,
            height: -700,
        }
    );
    assert!(e.has_bounds());
}

fn hmtx(advances: &[u16], lsbs: &[i16]) -> HmtxBake {
    let (bytes, number_of_h_metrics) = emit_long_metrics(advances, lsbs);
    HmtxBake {
        bytes,
        number_of_h_metrics,
        advances: advances.to_vec(),
        lsbs: lsbs.to_vec(),
    }
}

#[test]
fn boxes_set_bearings_head_and_extremes() {
    // Glyph 0 has no outline and keeps its bearing of 7; glyphs 1 and
    // 2 take theirs from their boxes.
    let source = hmtx(&[500, 600, 700], &[7, 50, 60]);
    let extents = [
        Some(Extents::default()),
        Some(Extents {
            x_bearing: 33,
            width: 500,
            y_bearing: 700,
            height: -710,
        }),
        Some(Extents {
            x_bearing: -20,
            width: 760,
            y_bearing: 650,
            height: -650,
        }),
    ];
    let m = apply_extents(&source, &extents);
    assert_eq!(m.hmtx.lsbs, [7, 33, -20]);
    assert_eq!(m.hmtx.advances, [500, 600, 700]);
    assert_eq!(m.head_box, Some([-20, -10, 740, 700]));
    assert_eq!(m.max_advance, 700);
    // The empty glyph counts as zero wide: bearings 7, 33, -20;
    // trailing bearings 493, 67, -40; extents 7, 533, 740.
    assert_eq!(m.extremes, Some([-20, -40, 740]));
    let (bytes, _) = emit_long_metrics(&[500, 600, 700], &[7, 33, -20]);
    assert_eq!(m.hmtx.bytes, bytes);
}

#[test]
fn without_boxes_the_bearings_and_head_stay() {
    let source = hmtx(&[500, 600], &[7, 8]);
    let m = apply_extents(&source, &[Some(Extents::default()); 2]);
    assert_eq!(m.hmtx.lsbs, [7, 8]);
    assert_eq!(m.head_box, None);
    assert_eq!(m.extremes, Some([7, 493, 8]));
    let none = apply_extents(&hmtx(&[], &[]), &[]);
    assert_eq!(none.extremes, None);
    assert_eq!(none.max_advance, 0);
}

#[test]
fn out_of_range_values_clamp() {
    let source = hmtx(&[100], &[0]);
    let extents = [Some(Extents {
        x_bearing: -40_000,
        width: 80_000,
        y_bearing: 40_000,
        height: -80_000,
    })];
    let m = apply_extents(&source, &extents);
    assert_eq!(m.hmtx.lsbs, [i16::MIN]);
    assert_eq!(m.head_box, Some([i16::MIN, i16::MIN, i16::MAX, i16::MAX]));
}

#[test]
fn a_glyph_that_cannot_be_drawn_keeps_its_bearing_and_counts_nowhere() {
    let source = hmtx(&[500, 900], &[7, 8]);
    let extents = [
        Some(Extents {
            x_bearing: 10,
            width: 400,
            y_bearing: 700,
            height: -700,
        }),
        None,
    ];
    let m = apply_extents(&source, &extents);
    assert_eq!(m.hmtx.lsbs, [10, 8]);
    assert_eq!(m.head_box, Some([10, 0, 410, 700]));
    // The largest advance counts every glyph; the bearings only the
    // first.
    assert_eq!(m.max_advance, 900);
    assert_eq!(m.extremes, Some([10, 90, 410]));
}
