//! Tests for the SVG parser and geometry primitives: colors,
//! transforms, path data, the document walk and shape primitives.
//! Masks, filters, dash arrays and the textPath helpers have their own
//! child modules.

use super::*;
use alloc::string::String;
use alloc::vec;

use sigilbuzz::tables::PathOp;

use super::dash::dash_polyline;
use super::document::{parse_document, parse_url_ref};
use super::filter::{apply_gaussian_blur, apply_offset, clamped_window_sum};
use super::model::{Fill, GradKind, Paint, SvgDoc};
use super::paint_server::parse_stop_offset;
use super::path::{parse_path_d, parse_points_list};
use super::render::{render_doc, render_fill, RenderBudget};
use super::stroke::flatten_to_polylines;
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
fn stop_style_sets_color_and_opacity() {
    // The style declarations win over the attributes, and the
    // trailing semicolon does not drop the stop.
    let xml = r##"<svg viewBox="0 0 10 10">
        <defs>
            <linearGradient id="g" x1="0" y1="0" x2="10" y2="0">
                <stop offset="0" stop-opacity="1" style="stop-color:#00FF00;stop-opacity:0.25;"/>
                <stop offset="1" stop-color="#0000FF" stop-opacity="0.5"/>
            </linearGradient>
        </defs>
        <rect x="0" y="0" width="10" height="10" fill="url(#g)"/>
    </svg>"##;
    let doc = parse_document(xml).unwrap();
    let Paint::Gradient(g) = &doc.fills[0].paint else {
        panic!("expected gradient fill");
    };
    assert_eq!(g.stops.len(), 2);
    let first = g.stops[0].color;
    assert!((first.g - 1.0).abs() < 1e-6 && first.r.abs() < 1e-6);
    assert!(
        (first.a - 0.25).abs() < 1e-6,
        "style opacity, got {}",
        first.a
    );
    assert!((g.stops[1].color.a - 0.5).abs() < 1e-6, "attribute opacity");
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

/// Strokes a right-angle corner at (50, 10) with the given join
/// and reports whether two pixels past the outer corner are
/// painted: (51, 8) lies inside the bevel, (53, 6) only inside the
/// miter.
fn outer_corner_pixels(join: &str) -> (bool, bool) {
    let xml = alloc::format!(
        r##"<svg viewBox="0 0 64 64"><path d="M 10 10 L 50 10 L 50 50" stroke="#000"
            stroke-width="8" stroke-linejoin="{join}" fill="none"/></svg>"##
    );
    let doc = parse_document(&xml).unwrap();
    let mut pix = ColorPixmap::new(64, 64);
    render_doc(&mut pix, &doc, &Affine::identity(), 0.25);
    (pix.get(51, 8)[3] > 0, pix.get(53, 6)[3] > 0)
}

#[test]
fn bevel_join_fills_the_outer_corner_without_a_spike() {
    assert_eq!(outer_corner_pixels("bevel"), (true, false));
    assert_eq!(outer_corner_pixels("miter"), (true, true));
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

#[test]
fn path_d_number_after_closepath_is_an_error() {
    // A number after `Z` used to repeat the closepath without
    // consuming input, looping forever while pushing Close ops.
    assert!(parse_path_d("M0 0 L1 0 Z 1 1").is_err());
    assert!(parse_path_d("m0 0 z5").is_err());
}

#[test]
fn parse_color_rejects_six_bytes_of_non_ascii_hex() {
    // Six bytes of text where byte 2 falls inside a character used
    // to panic on the slice.
    assert_eq!(parse_color("#a\u{20AC}bc"), None);
    assert_eq!(parse_color("#\u{e9}\u{e9}\u{e9}"), None);
}

#[test]
fn use_fan_out_is_bounded_by_the_work_budget() {
    // Ten levels of groups that each reference the next level ten
    // times expand to 10^10 element visits. This used to hang.
    use core::fmt::Write;
    let mut xml = String::from(r#"<svg viewBox="0 0 10 10"><defs>"#);
    for level in 0..10 {
        write!(xml, r#"<g id="l{level}">"#).unwrap();
        for _ in 0..10 {
            write!(xml, r##"<use href="#l{}"/>"##, level + 1).unwrap();
        }
        xml.push_str("</g>");
    }
    xml.push_str(r##"<g id="l10"/></defs><use href="#l0"/></svg>"##);
    assert_eq!(
        parse_document(&xml).unwrap_err(),
        RenderError::Parse("svg work cap")
    );
}

#[test]
fn repeated_large_clip_is_bounded_by_the_ops_budget() {
    // Every fill used to carry its own copy of the clip path, so
    // 4096 fills sharing a 20k-op clip stored 80M operations.
    use core::fmt::Write;
    let mut clip = String::from("M0 0");
    for i in 0..20_000 {
        write!(clip, " L{} {}", i % 97, i % 89).unwrap();
    }
    let mut xml = String::from(r#"<svg viewBox="0 0 10 10"><defs><clipPath id="c">"#);
    write!(xml, r#"<path d="{clip}"/></clipPath></defs>"#).unwrap();
    for _ in 0..4096 {
        xml.push_str(r#"<rect width="5" height="5" clip-path="url(#c)"/>"#);
    }
    xml.push_str("</svg>");
    let doc = parse_document(&xml).unwrap();
    let stored: usize = doc.fills.iter().map(Fill::weight).sum();
    assert!(stored <= MAX_DOC_OPS, "stored {stored} ops");
    assert!(doc.fills.len() < 4096);
}

#[test]
fn filter_keeps_at_most_max_primitives() {
    let mut xml = String::from(r#"<svg viewBox="0 0 10 10"><defs><filter id="f">"#);
    for _ in 0..500 {
        xml.push_str(r#"<feOffset dx="1"/>"#);
    }
    xml.push_str(r##"</filter></defs><rect width="5" height="5" filter="url(#f)"/></svg>"##);
    let doc = parse_document(&xml).unwrap();
    let f = doc.fills[0].filter.as_ref().expect("filter attached");
    assert_eq!(f.primitives.len(), MAX_FILTER_PRIMITIVES);
}

#[test]
fn shared_mask_rendering_is_bounded_by_the_pass_budget() {
    // Every masked fill renders all mask children again, so fills
    // times children grows without limit. Here 1000 fills with a
    // 100-child mask would need about 102k canvas passes.
    use core::fmt::Write;
    let mut xml = String::from(r#"<svg viewBox="0 0 16 16"><defs><mask id="m">"#);
    for i in 0..100 {
        write!(
            xml,
            r#"<rect x="{}" y="0" width="1" height="16" fill="white"/>"#,
            i % 16
        )
        .unwrap();
    }
    xml.push_str("</mask></defs>");
    for _ in 0..1000 {
        xml.push_str(r#"<rect width="8" height="8" fill="red" mask="url(#m)"/>"#);
    }
    xml.push_str("</svg>");
    let doc = parse_document(&xml).unwrap();
    assert_eq!(doc.fills.len(), 1000);
    let mut out = ColorPixmap::new(16, 16);
    let mut budget = RenderBudget::new();
    for fill in &doc.fills {
        render_fill(&mut out, fill, &Affine::identity(), 0.25, &mut budget);
    }
    assert!(budget.passes_left < doc.fills[0].render_passes() + 100);
    assert_eq!(out.get(4, 4)[3], 255, "early fills still render");
}

#[test]
fn repeated_mask_resolution_is_bounded_by_the_work_budget() {
    // Each masked element walks the whole mask body again. With a
    // 1000-child mask and 4000 elements that is four million
    // element visits, so the parse stops at the work budget.
    let mut xml = String::from(r#"<svg viewBox="0 0 16 16"><defs><mask id="m">"#);
    for _ in 0..1000 {
        xml.push_str(r#"<rect width="1" height="16" fill="white"/>"#);
    }
    xml.push_str("</mask></defs>");
    for _ in 0..4000 {
        xml.push_str(r#"<rect width="8" height="8" fill="red" mask="url(#m)"/>"#);
    }
    xml.push_str("</svg>");
    assert_eq!(
        parse_document(&xml).unwrap_err(),
        RenderError::Parse("svg work cap")
    );
}

#[test]
fn tiny_dash_on_a_long_line_stops_at_the_split_budget() {
    // Past ~16384 units a 0.001 dash no longer advances the walk
    // position, so this used to loop forever.
    let pts = [(0.0, 0.0), (100_000.0, 0.0)];
    let segs = dash_polyline(&pts, &[100_000.0], false, &[0.001, 0.001], 0.0);
    assert!(!segs.is_empty());
    assert!(segs.len() <= MAX_DASH_SPLITS);
}

#[test]
fn non_finite_curves_do_not_multiply_stroke_points() {
    // A NaN control point used to subdivide 16 levels deep and emit
    // 65536 points per curve.
    let ops = [
        PathOp::MoveTo { x: 0.0, y: 0.0 },
        PathOp::QuadTo {
            cx: f32::NAN,
            cy: 0.0,
            x: 10.0,
            y: 0.0,
        },
        PathOp::CubicTo {
            c1x: f32::INFINITY,
            c1y: 0.0,
            c2x: 0.0,
            c2y: 0.0,
            x: 20.0,
            y: 5.0,
        },
    ];
    let polys = flatten_to_polylines(&ops);
    let points: usize = polys.iter().map(|p| p.points.len()).sum();
    assert_eq!(points, 3);
}

#[test]
fn huge_blur_radius_is_clamped() {
    let mut src = ColorPixmap::new(7, 5);
    for (i, b) in src.data.iter_mut().enumerate() {
        *b = (i * 37 % 256) as u8;
    }
    // Used to overflow `r * 2 + 1` (a panic in debug builds) and
    // visit four billion window samples per row.
    let out = apply_gaussian_blur(&src, 1e30, f32::INFINITY);
    assert_eq!((out.width, out.height), (7, 5));
}

#[test]
fn counted_blur_window_matches_the_sample_by_sample_sum() {
    for r in 0..24 {
        for len in 1..12 {
            let sample = |k: i32| (k * 13 + 7) as u32 % 256;
            let naive: u32 = (-r..=r).map(|k| sample(k.clamp(0, len - 1))).sum();
            assert_eq!(clamped_window_sum(r, len, sample), naive, "r {r} len {len}");
        }
    }
}

#[test]
fn huge_filter_offset_does_not_overflow() {
    let mut src = ColorPixmap::new(4, 4);
    src.data.fill(200);
    // `y - dy` used to overflow `i32` once the offset saturated.
    for (dx, dy) in [
        (0.0, -1e30),
        (-1e30, 0.0),
        (f32::NEG_INFINITY, f32::INFINITY),
    ] {
        let out = apply_offset(&src, dx, dy);
        assert!(out.data.iter().all(|&b| b == 0));
    }
}
