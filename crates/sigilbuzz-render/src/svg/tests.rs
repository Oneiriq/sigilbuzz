//! Tests for the SVG parser and geometry primitives: colors,
//! transforms, path data, the document walk and shape primitives.
//! Masks, filters, dash arrays and the textPath helpers have their own
//! child modules.

use super::*;
use alloc::vec;

use sigilbuzz::tables::PathOp;

use super::document::{parse_document, parse_url_ref};
use super::model::{Fill, GradKind, Paint, SvgDoc};
use super::paint_server::parse_stop_offset;
use super::path::{parse_path_d, parse_points_list};
use super::style::{parse_color, parse_transform, parse_viewbox};

mod dash;
mod filter;
mod mask;
mod text_path;

fn first_fill(doc: &SvgDoc) -> &Fill {
    &doc.fills[0]
}

#[test]
fn parse_color_hex_long() {
    assert_eq!(parse_color("#FF8800"), Some([0xff, 0x88, 0x00, 255]));
    assert_eq!(parse_color("#000000"), Some([0, 0, 0, 255]));
}

#[test]
fn parse_color_hex_short_doubles_each_nybble() {
    assert_eq!(parse_color("#f80"), Some([0xff, 0x88, 0x00, 255]));
}

#[test]
fn parse_color_rgb_clamps_oversize() {
    assert_eq!(parse_color("rgb(300, -2, 128)"), Some([255, 0, 128, 255]));
}

#[test]
fn parse_color_named_and_none() {
    assert_eq!(parse_color("black"), Some([0, 0, 0, 255]));
    assert_eq!(parse_color("WHITE"), Some([255, 255, 255, 255]));
    assert_eq!(parse_color("none"), None);
}

#[test]
fn parse_viewbox_four_numbers() {
    assert_eq!(parse_viewbox("0 0 100 200"), Some((0.0, 0.0, 100.0, 200.0)));
    assert_eq!(
        parse_viewbox("-5, -5, 10, 10"),
        Some((-5.0, -5.0, 10.0, 10.0))
    );
}

#[test]
fn parse_transform_translate_scale_matrix() {
    let t = parse_transform("translate(10, 20)").unwrap();
    assert!((t.dx - 10.0).abs() < 1e-5 && (t.dy - 20.0).abs() < 1e-5);
    let s = parse_transform("scale(2)").unwrap();
    assert!((s.xx - 2.0).abs() < 1e-5 && (s.yy - 2.0).abs() < 1e-5);
    let m = parse_transform("matrix(1 0 0 -1 0 100)").unwrap();
    assert!((m.yy + 1.0).abs() < 1e-5);
    assert!((m.dy - 100.0).abs() < 1e-5);
}

#[test]
fn parse_transform_chains_left_to_right() {
    let xf = parse_transform("translate(10, 0) scale(2)").unwrap();
    let (x, y) = xf.apply(1.0, 0.0);
    assert!((x - 12.0).abs() < 1e-5);
    assert!(y.abs() < 1e-5);
}

#[test]
fn path_d_parses_absolute_mlz() {
    let ops = parse_path_d("M 0 0 L 10 0 L 10 10 L 0 10 Z").unwrap();
    assert!(matches!(ops[0], PathOp::MoveTo { x, y } if x == 0.0 && y == 0.0));
    assert!(matches!(ops[3], PathOp::LineTo { x, y } if x == 0.0 && y == 10.0));
    assert!(matches!(ops[4], PathOp::Close));
}

#[test]
fn path_d_handles_relative_commands() {
    let abs = parse_path_d("M 0 0 L 10 0 L 10 10 L 0 10 Z").unwrap();
    let rel = parse_path_d("m 0 0 l 10 0 l 0 10 l -10 0 z").unwrap();
    let to_xy = |op: &PathOp| match *op {
        PathOp::MoveTo { x, y } | PathOp::LineTo { x, y } => Some((x, y)),
        _ => None,
    };
    let abs_pts: Vec<_> = abs.iter().filter_map(to_xy).collect();
    let rel_pts: Vec<_> = rel.iter().filter_map(to_xy).collect();
    assert_eq!(abs_pts, rel_pts);
}

#[test]
fn path_d_handles_implicit_repetition() {
    let ops = parse_path_d("M 0 0 10 0 10 10 0 10 Z").unwrap();
    assert_eq!(ops.len(), 5);
    assert!(matches!(ops[1], PathOp::LineTo { x, y } if x == 10.0 && y == 0.0));
}

#[test]
fn path_d_handles_curves() {
    let ops = parse_path_d("M 0 0 C 0 10 10 10 10 0 Q 5 -5 0 0 Z").unwrap();
    assert!(matches!(ops[1], PathOp::CubicTo { .. }));
    assert!(matches!(ops[2], PathOp::QuadTo { .. }));
    assert!(matches!(ops[3], PathOp::Close));
}

#[test]
fn path_d_handles_h_and_v() {
    let ops = parse_path_d("M 1 2 H 5 V 7 Z").unwrap();
    assert!(matches!(ops[1], PathOp::LineTo { x, y } if x == 5.0 && y == 2.0));
    assert!(matches!(ops[2], PathOp::LineTo { x, y } if x == 5.0 && y == 7.0));
}

#[test]
fn path_d_handles_compact_negative_numbers() {
    let ops = parse_path_d("M0 0L10-5L-3 .5Z").unwrap();
    assert_eq!(ops.len(), 4);
    assert!(
        matches!(ops[2], PathOp::LineTo { x, y } if (x + 3.0).abs() < 1e-3 && (y - 0.5).abs() < 1e-3)
    );
}

#[test]
fn xml_parser_walks_attributes() {
    let xml = r##"<svg viewBox="0 0 100 100" xmlns="http://www.w3.org/2000/svg">
        <path d="M 0 0 L 100 0 L 100 100 L 0 100 Z" fill="#FF0000"/>
    </svg>"##;
    let doc = parse_document(xml).unwrap();
    assert_eq!(doc.view_w, 100.0);
    assert_eq!(doc.view_h, 100.0);
    assert_eq!(doc.fills.len(), 1);
    let Paint::Solid(c) = &first_fill(&doc).paint else {
        panic!("expected solid fill");
    };
    assert_eq!(*c, [0xff, 0, 0, 255]);
}

#[test]
fn group_transform_composes_onto_path() {
    let xml = r#"<svg viewBox="0 0 100 100">
        <g transform="scale(2)">
            <path d="M 0 0 L 10 0 L 10 10 Z" fill="black"/>
        </g>
    </svg>"#;
    let doc = parse_document(xml).unwrap();
    assert_eq!(doc.fills.len(), 1);
    let xf = doc.fills[0].xform;
    assert!((xf.xx - 2.0).abs() < 1e-5);
    assert!((xf.yy - 2.0).abs() < 1e-5);
}

#[test]
fn unknown_elements_are_skipped_not_failed() {
    let xml = r#"<svg viewBox="0 0 10 10">
        <metadata>hello</metadata>
        <path d="M 0 0 L 10 0 L 10 10 Z" fill="black"/>
    </svg>"#;
    let doc = parse_document(xml).unwrap();
    assert_eq!(doc.fills.len(), 1);
}

#[test]
fn fill_none_suppresses_the_path() {
    let xml = r#"<svg viewBox="0 0 10 10">
        <path d="M 0 0 L 10 0 L 10 10 Z" fill="none"/>
    </svg>"#;
    let doc = parse_document(xml).unwrap();
    assert!(doc.fills.is_empty());
}

#[test]
fn missing_root_svg_is_an_error() {
    assert!(parse_document("<not-svg/>").is_err());
}

#[test]
fn rect_with_no_radii_is_a_quad() {
    let xml = r#"<svg viewBox="0 0 10 10">
        <rect x="1" y="2" width="4" height="6" fill="black"/>
    </svg>"#;
    let doc = parse_document(xml).unwrap();
    assert_eq!(doc.fills.len(), 1);
    // 4 line segments + close.
    let ops = &doc.fills[0].ops;
    assert!(ops.iter().any(|o| matches!(o, PathOp::Close)));
    assert!(matches!(ops[0], PathOp::MoveTo { x, y } if x == 1.0 && y == 2.0));
}

#[test]
fn rect_with_rx_ry_emits_cubics() {
    let xml = r#"<svg viewBox="0 0 10 10">
        <rect x="0" y="0" width="10" height="10" rx="2" ry="2" fill="black"/>
    </svg>"#;
    let doc = parse_document(xml).unwrap();
    let ops = &doc.fills[0].ops;
    assert!(ops.iter().any(|o| matches!(o, PathOp::CubicTo { .. })));
}

#[test]
fn circle_emits_four_cubics() {
    let xml = r#"<svg viewBox="0 0 10 10">
        <circle cx="5" cy="5" r="3" fill="red"/>
    </svg>"#;
    let doc = parse_document(xml).unwrap();
    assert_eq!(doc.fills.len(), 1);
    let cubics = doc.fills[0]
        .ops
        .iter()
        .filter(|o| matches!(o, PathOp::CubicTo { .. }))
        .count();
    assert_eq!(cubics, 4);
}

#[test]
fn ellipse_emits_four_cubics() {
    let xml = r#"<svg viewBox="0 0 10 10">
        <ellipse cx="5" cy="5" rx="4" ry="2" fill="red"/>
    </svg>"#;
    let doc = parse_document(xml).unwrap();
    let cubics = doc.fills[0]
        .ops
        .iter()
        .filter(|o| matches!(o, PathOp::CubicTo { .. }))
        .count();
    assert_eq!(cubics, 4);
}

#[test]
fn use_resolves_in_document_reference() {
    let xml = r##"<svg viewBox="0 0 100 100">
        <defs><circle id="dot" cx="0" cy="0" r="3" fill="black"/></defs>
        <use xlink:href="#dot" x="10" y="10"/>
        <use xlink:href="#dot" x="50" y="50"/>
    </svg>"##;
    let doc = parse_document(xml).unwrap();
    assert_eq!(doc.fills.len(), 2);
    // First use translated to (10, 10).
    let (x, y) = doc.fills[0].xform.apply(0.0, 0.0);
    assert!((x - 10.0).abs() < 1e-5 && (y - 10.0).abs() < 1e-5);
}

#[test]
fn use_recursion_guard_caps_at_depth() {
    // <use> pointing at a <g> that itself contains a <use> back at
    // the parent should bottom out at MAX_USE_DEPTH instead of
    // recursing forever.
    let xml = r##"<svg viewBox="0 0 100 100">
        <defs>
            <g id="a"><use xlink:href="#a"/><circle cx="0" cy="0" r="1" fill="black"/></g>
        </defs>
        <use xlink:href="#a"/>
    </svg>"##;
    let doc = parse_document(xml).unwrap();
    // The non-cycling circle inside <g id="a"> renders at every
    // expansion level. The expansion bottoms out at MAX_USE_DEPTH;
    // the test just asserts we stayed under MAX_FILLS and didn't
    // panic.
    assert!(doc.fills.len() <= MAX_FILLS);
}

#[test]
fn linear_gradient_collected_with_stops() {
    let xml = r##"<svg viewBox="0 0 10 10">
        <defs>
            <linearGradient id="g" x1="0" y1="0" x2="10" y2="0">
                <stop offset="0" stop-color="#FF0000"/>
                <stop offset="1" stop-color="#0000FF"/>
            </linearGradient>
        </defs>
        <rect x="0" y="0" width="10" height="10" fill="url(#g)"/>
    </svg>"##;
    let doc = parse_document(xml).unwrap();
    assert_eq!(doc.fills.len(), 1);
    let Paint::Gradient(g) = &doc.fills[0].paint else {
        panic!("expected gradient fill");
    };
    assert_eq!(g.stops.len(), 2);
    assert!(matches!(g.kind, GradKind::Linear { .. }));
}

#[test]
fn radial_gradient_parsed() {
    let xml = r##"<svg viewBox="0 0 10 10">
        <defs>
            <radialGradient id="g" cx="5" cy="5" r="5">
                <stop offset="0" stop-color="#FF0000"/>
                <stop offset="1" stop-color="#0000FF"/>
            </radialGradient>
        </defs>
        <rect x="0" y="0" width="10" height="10" fill="url(#g)"/>
    </svg>"##;
    let doc = parse_document(xml).unwrap();
    let Paint::Gradient(g) = &doc.fills[0].paint else {
        panic!("expected gradient fill");
    };
    assert!(matches!(g.kind, GradKind::Radial { .. }));
}

#[test]
fn stroke_emits_outline_fill() {
    let xml = r##"<svg viewBox="0 0 100 100">
        <path d="M 10 10 L 90 10" stroke="#000" stroke-width="4" fill="none"/>
    </svg>"##;
    let doc = parse_document(xml).unwrap();
    // No fill (fill="none"), but one stroke fill.
    assert_eq!(doc.fills.len(), 1);
    assert!(doc.fills[0].is_stroke);
}

#[test]
fn stroke_zero_width_ignored() {
    let xml = r##"<svg viewBox="0 0 10 10">
        <path d="M 0 0 L 10 0" stroke="#000" stroke-width="0" fill="none"/>
    </svg>"##;
    let doc = parse_document(xml).unwrap();
    assert!(doc.fills.is_empty());
}

#[test]
fn clip_path_attaches_to_fill() {
    let xml = r##"<svg viewBox="0 0 100 100">
        <defs>
            <clipPath id="c"><circle cx="50" cy="50" r="20"/></clipPath>
        </defs>
        <rect x="0" y="0" width="100" height="100" fill="#000" clip-path="url(#c)"/>
    </svg>"##;
    let doc = parse_document(xml).unwrap();
    assert_eq!(doc.fills.len(), 1);
    assert!(doc.fills[0].clip.is_some());
}

#[test]
fn parse_url_ref_extracts_id() {
    assert_eq!(parse_url_ref("url(#abc)"), Some("abc".into()));
    assert_eq!(parse_url_ref(" url(#xyz) "), Some("xyz".into()));
    assert_eq!(parse_url_ref("url('#q')"), Some("q".into()));
    assert_eq!(parse_url_ref("none"), None);
}

#[test]
fn stop_offset_handles_percent_and_decimal() {
    assert!((parse_stop_offset("50%") - 0.5).abs() < 1e-5);
    assert!((parse_stop_offset("0.25") - 0.25).abs() < 1e-5);
}

#[test]
fn points_list_accepts_space_and_comma_separators() {
    let a = parse_points_list("0,0 10,0 10,10 0,10");
    let b = parse_points_list("0 0 10 0 10 10 0 10");
    let c = parse_points_list("0,0,10,0,10,10,0,10");
    assert_eq!(a, vec![(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)]);
    assert_eq!(a, b);
    assert_eq!(a, c);
}

#[test]
fn points_list_handles_decimals_and_signs() {
    let pts = parse_points_list("-1.5,2 3.25e1,-0.5");
    assert_eq!(pts.len(), 2);
    assert!((pts[0].0 + 1.5).abs() < 1e-5);
    assert!((pts[1].0 - 32.5).abs() < 1e-5);
    assert!((pts[1].1 + 0.5).abs() < 1e-5);
}

#[test]
fn points_list_drops_trailing_odd_coordinate() {
    let pts = parse_points_list("0 0 10 0 5");
    assert_eq!(pts, vec![(0.0, 0.0), (10.0, 0.0)]);
}

#[test]
fn polygon_lowers_to_closed_path() {
    let xml = r#"<svg viewBox="0 0 100 100">
        <polygon points="10,10 90,10 50,90" fill="black"/>
    </svg>"#;
    let doc = parse_document(xml).unwrap();
    assert_eq!(doc.fills.len(), 1);
    let ops = &doc.fills[0].ops;
    assert!(matches!(ops[0], PathOp::MoveTo { x, y } if x == 10.0 && y == 10.0));
    assert!(matches!(ops[1], PathOp::LineTo { x, y } if x == 90.0 && y == 10.0));
    assert!(matches!(ops[2], PathOp::LineTo { x, y } if x == 50.0 && y == 90.0));
    assert!(matches!(ops[3], PathOp::Close));
}

#[test]
fn polyline_lowers_to_open_path() {
    let xml = r#"<svg viewBox="0 0 100 100">
        <polyline points="10,10 90,10 50,90" fill="none" stroke="black" stroke-width="2"/>
    </svg>"#;
    let doc = parse_document(xml).unwrap();
    // No fill (fill="none"); stroke produces one fill ribbon.
    assert_eq!(doc.fills.len(), 1);
    assert!(doc.fills[0].is_stroke);
}

#[test]
fn line_lowers_to_two_op_path() {
    // fill="none" suppresses the (degenerate) fill so we can see
    // the stroke alone.
    let xml = r#"<svg viewBox="0 0 100 100">
        <line x1="10" y1="10" x2="90" y2="90" stroke="black" stroke-width="2" fill="none"/>
    </svg>"#;
    let doc = parse_document(xml).unwrap();
    assert_eq!(doc.fills.len(), 1);
    assert!(doc.fills[0].is_stroke);
    // The stroke source ops were MoveTo + LineTo before being
    // expanded into a ribbon: the Fill we collected is the ribbon,
    // so just ensure it's non-empty.
    assert!(!doc.fills[0].ops.is_empty());
}

#[test]
fn polygon_with_too_few_points_drops() {
    let xml = r#"<svg viewBox="0 0 100 100">
        <polygon points="10,10" fill="black"/>
    </svg>"#;
    let doc = parse_document(xml).unwrap();
    assert!(doc.fills.is_empty());
}
