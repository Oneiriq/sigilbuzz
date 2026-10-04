//! `Face::glyph_outlines` reads the outline tables once for a run of
//! glyphs. Every glyph must come out as `Face::glyph_outline_at_coords`
//! draws it, at the default instance and away from it, in any order.

use sigilbuzz::tables::{OutlineSink, PathOp};
use sigilbuzz::Face;

const FONTS: &[(&str, &[u8])] = &[
    (
        "noto sans kr cff2",
        include_bytes!("fixtures/noto_sans_kr_vf_cff2_subset.otf"),
    ),
    (
        "noto sans kr cff2 vertical",
        include_bytes!("fixtures/noto_sans_kr_vf_vertical_subset.otf"),
    ),
    (
        "source sans 3 cff2",
        include_bytes!("fonts/SourceSans3VF-Latin-Subset.otf"),
    ),
    (
        "source code pro cff",
        include_bytes!("fonts/SourceCodePro-Latin-Subset.otf"),
    ),
    (
        "hahmlet glyf gvar",
        include_bytes!("fixtures/hahmlet_gvar_subset.ttf"),
    ),
    ("rubik glyf gvar", include_bytes!("fixtures/rubik_vf.ttf")),
    (
        "open sans glyf",
        include_bytes!("fixtures/opensans_regular.ttf"),
    ),
];

const SETTINGS: &[&[f32]] = &[&[], &[0.0], &[0.37], &[-0.61], &[1.0], &[0.5, 0.25]];

#[test]
fn a_run_draws_every_glyph_as_the_face_does() {
    for &(name, data) in FONTS {
        let face = Face::parse_bytes(data, 0).unwrap();
        let n = face.maxp().unwrap().num_glyphs;
        for &coords in SETTINGS {
            let outlines = face.glyph_outlines(coords);
            // Backwards, then forwards, then past the last glyph: the
            // kept tables must not depend on the order.
            for gid in (0..n).rev().chain(0..n).chain([n, n + 7, u16::MAX]) {
                assert_eq!(
                    outlines.outline(gid),
                    face.glyph_outline_at_coords(gid, coords),
                    "{name} glyph {gid} at {coords:?}"
                );
            }
        }
    }
}

/// Collects what a sink receives.
#[derive(Default)]
struct Ops(Vec<PathOp>);

impl OutlineSink for Ops {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.push(PathOp::MoveTo { x, y });
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.0.push(PathOp::LineTo { x, y });
    }
    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        self.0.push(PathOp::QuadTo { cx, cy, x, y });
    }
    fn curve_to(&mut self, c1x: f32, c1y: f32, c2x: f32, c2y: f32, x: f32, y: f32) {
        self.0.push(PathOp::CubicTo {
            c1x,
            c1y,
            c2x,
            c2y,
            x,
            y,
        });
    }
    fn close(&mut self) {
        self.0.push(PathOp::Close);
    }
}

#[test]
fn drawing_into_a_sink_sends_the_outline_ops() {
    for &(name, data) in FONTS {
        let face = Face::parse_bytes(data, 0).unwrap();
        let outlines = face.glyph_outlines(&[0.37]);
        for gid in 0..face.maxp().unwrap().num_glyphs {
            let mut ops = Ops::default();
            let drew = outlines.draw(gid, &mut ops).unwrap();
            let outline = outlines.outline(gid).unwrap();
            assert_eq!(drew, outline.is_some(), "{name} glyph {gid}");
            let want = outline.map(|o| o.ops().to_vec()).unwrap_or_default();
            assert_eq!(ops.0, want, "{name} glyph {gid}");
        }
    }
}

#[test]
fn a_broken_table_fails_every_glyph_alike() {
    // A CFF2 table cut short: the face still parses, every glyph errors
    // the way the face's own method errors.
    let data = include_bytes!("fixtures/noto_sans_kr_vf_cff2_subset.otf");
    let face = Face::parse_bytes(data, 0).unwrap();
    let record = *face.record(*b"CFF2").unwrap();
    let mut broken = data.to_vec();
    let start = record.offset as usize;
    for b in &mut broken[start..start + 8] {
        *b = 0xFF;
    }
    let face = Face::parse_bytes(&broken, 0).unwrap();
    let outlines = face.glyph_outlines(&[0.5]);
    for gid in 0..3 {
        let want = face.glyph_outline_at_coords(gid, &[0.5]);
        assert!(want.is_err());
        assert_eq!(outlines.outline(gid), want);
    }
}
