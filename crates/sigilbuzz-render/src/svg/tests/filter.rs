//! Tests for `<filter>` parsing and the color matrix helpers.

use super::*;

use crate::svg::filter::{hue_rotate_matrix, parse_std_deviation, saturate_matrix};
use crate::svg::model::FilterOp;

#[test]
fn filter_attaches_to_fill_when_referenced() {
    let xml = r##"<svg viewBox="0 0 100 100">
        <defs>
            <filter id="b"><feGaussianBlur stdDeviation="2"/></filter>
        </defs>
        <rect x="0" y="0" width="100" height="100" fill="#000" filter="url(#b)"/>
    </svg>"##;
    let doc = parse_document(xml).unwrap();
    assert_eq!(doc.fills.len(), 1);
    let f = doc.fills[0].filter.as_ref().expect("filter expected");
    assert_eq!(f.primitives.len(), 1);
    assert!(matches!(f.primitives[0].op, FilterOp::GaussianBlur { .. }));
}

#[test]
fn filter_unknown_id_silently_drops() {
    let xml = r##"<svg viewBox="0 0 10 10">
        <rect x="0" y="0" width="10" height="10" fill="#000" filter="url(#missing)"/>
    </svg>"##;
    let doc = parse_document(xml).unwrap();
    assert_eq!(doc.fills.len(), 1);
    assert!(doc.fills[0].filter.is_none());
}

#[test]
fn filter_parses_full_drop_shadow_chain() {
    let xml = r##"<svg viewBox="0 0 100 100">
        <defs>
            <filter id="ds">
                <feGaussianBlur in="SourceAlpha" stdDeviation="2" result="b"/>
                <feOffset in="b" dx="4" dy="4" result="o"/>
                <feMerge>
                    <feMergeNode in="o"/>
                    <feMergeNode in="SourceGraphic"/>
                </feMerge>
            </filter>
        </defs>
        <rect x="10" y="10" width="40" height="40" fill="#000" filter="url(#ds)"/>
    </svg>"##;
    let doc = parse_document(xml).unwrap();
    let f = doc.fills[0].filter.as_ref().unwrap();
    assert_eq!(f.primitives.len(), 3);
    assert!(matches!(f.primitives[0].op, FilterOp::GaussianBlur { .. }));
    assert!(matches!(f.primitives[1].op, FilterOp::Offset { .. }));
    assert!(matches!(f.primitives[2].op, FilterOp::Merge { .. }));
}

#[test]
fn color_matrix_saturate_zero_collapses_red_channels() {
    let m = saturate_matrix(0.0);
    // Pure red (1,0,0,1) -> gray: each output channel ~0.213.
    let r = m[0] * 1.0 + m[1] * 0.0 + m[2] * 0.0 + m[3] * 1.0 + m[4];
    let g = m[5] * 1.0 + m[6] * 0.0 + m[7] * 0.0 + m[8] * 1.0 + m[9];
    let b = m[10] * 1.0 + m[11] * 0.0 + m[12] * 0.0 + m[13] * 1.0 + m[14];
    assert!((r - g).abs() < 1e-3);
    assert!((g - b).abs() < 1e-3);
}

#[test]
fn color_matrix_hue_rotate_zero_is_identity() {
    let m = hue_rotate_matrix(0.0);
    // (1,0,0) stays roughly (1,0,0).
    let r = m[0] * 1.0 + m[1] * 0.0 + m[2] * 0.0;
    assert!((r - 1.0).abs() < 1e-2);
}

#[test]
fn parse_std_deviation_handles_one_or_two_values() {
    assert_eq!(parse_std_deviation("3"), Some((3.0, 3.0)));
    assert_eq!(parse_std_deviation("3 5"), Some((3.0, 5.0)));
    assert_eq!(parse_std_deviation("3,5"), Some((3.0, 5.0)));
    // Negative collapses to zero.
    assert_eq!(parse_std_deviation("-2"), Some((0.0, 0.0)));
    assert_eq!(parse_std_deviation(""), None);
}
