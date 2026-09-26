//! `glyph_to_svg_color` writes the paint tree in HarfBuzz's model:
//! a transform below a `PaintGlyph` stays on the paint, ClipList boxes
//! become clip paths, composites are isolated groups, and an unbounded
//! glyph produces no SVG.
//!
//! The font has one outline, gid 1, a 200-unit square.

#![cfg(feature = "color")]

use sigilbuzz::Face;
use sigilbuzz_svg::glyph_to_svg_color;

fn words(values: &[i16]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_be_bytes()).collect()
}

fn f2dot14(v: f32) -> [u8; 2] {
    ((v * 16384.0).round() as i16).to_be_bytes()
}

fn set_offset24(p: &mut [u8], at: usize, target: usize) {
    let v = target as u32;
    p[at..at + 3].copy_from_slice(&[(v >> 16) as u8, (v >> 8) as u8, v as u8]);
}

fn parent(mut head: Vec<u8>, child: &[u8]) -> Vec<u8> {
    let at = head.len();
    set_offset24(&mut head, 1, at);
    head.extend_from_slice(child);
    head
}

fn solid(entry: u16) -> Vec<u8> {
    let mut p = vec![2u8];
    p.extend_from_slice(&entry.to_be_bytes());
    p.extend_from_slice(&f2dot14(1.0));
    p
}

fn glyph(gid: u16, child: &[u8]) -> Vec<u8> {
    let mut head = vec![10u8, 0, 0, 0];
    head.extend_from_slice(&gid.to_be_bytes());
    parent(head, child)
}

fn scale(s: f32, child: &[u8]) -> Vec<u8> {
    let mut head = vec![20u8, 0, 0, 0];
    head.extend_from_slice(&f2dot14(s));
    parent(head, child)
}

fn linear() -> Vec<u8> {
    let mut p = vec![4u8, 0, 0, 0];
    p.extend(words(&[0, 0, 200, 0, 0, 200]));
    let mut line = vec![0u8];
    line.extend_from_slice(&2u16.to_be_bytes());
    for (offset, entry) in [(0.0, 0u16), (1.0, 1)] {
        line.extend_from_slice(&f2dot14(offset));
        line.extend_from_slice(&entry.to_be_bytes());
        line.extend_from_slice(&f2dot14(1.0));
    }
    parent(p, &line)
}

fn composite(source: &[u8], mode: u8, backdrop: &[u8]) -> Vec<u8> {
    let mut p = vec![32u8, 0, 0, 0, mode, 0, 0, 0];
    let at = p.len();
    set_offset24(&mut p, 1, at);
    p.extend_from_slice(source);
    let at = p.len();
    set_offset24(&mut p, 5, at);
    p.extend_from_slice(backdrop);
    p
}

/// COLR v1 with `paints` and one ClipList record per `clips` entry.
fn colr(paints: &[(u16, Vec<u8>)], clips: &[(u16, [i16; 4])]) -> Vec<u8> {
    let mut out = words(&[1, 0]);
    out.extend_from_slice(&34u32.to_be_bytes());
    out.extend_from_slice(&34u32.to_be_bytes());
    out.extend(words(&[0]));
    out.extend_from_slice(&34u32.to_be_bytes());
    out.extend_from_slice(&[0; 16]);
    out.extend_from_slice(&(paints.len() as u32).to_be_bytes());
    let records = out.len();
    for (gid, _) in paints {
        out.extend(words(&[*gid as i16]));
        out.extend_from_slice(&0u32.to_be_bytes());
    }
    for (i, (_, bytes)) in paints.iter().enumerate() {
        let rel = (out.len() - 34) as u32;
        out[records + i * 6 + 2..records + i * 6 + 6].copy_from_slice(&rel.to_be_bytes());
        out.extend_from_slice(bytes);
    }
    let at = out.len() as u32;
    out[22..26].copy_from_slice(&at.to_be_bytes());
    out.push(1);
    out.extend_from_slice(&(clips.len() as u32).to_be_bytes());
    for (i, (gid, _)) in clips.iter().enumerate() {
        out.extend(words(&[*gid as i16, *gid as i16]));
        out.extend_from_slice(&((5 + 7 * clips.len() + 9 * i) as u32).to_be_bytes()[1..]);
    }
    for (_, coords) in clips {
        out.push(1);
        out.extend(words(coords));
    }
    out
}

fn font_bytes() -> Vec<u8> {
    let mut square = words(&[1, 0, 0, 200, 200, 3, 0]);
    square.extend_from_slice(&[0x01; 4]);
    square.extend(words(&[0, 200, 0, -200, 0, 0, 200, 0]));
    let mut loca = Vec::new();
    for off in [0usize, 0, square.len()] {
        loca.extend_from_slice(&((off / 2) as u16).to_be_bytes());
    }
    let mut head = 0x0001_0000u32.to_be_bytes().to_vec();
    head.extend_from_slice(&[0; 8]);
    head.extend_from_slice(&0x5F0F_3CF5u32.to_be_bytes());
    head.extend(words(&[0, 1000]));
    head.extend_from_slice(&[0; 16]);
    head.extend(words(&[0, 0, 200, 200, 0, 8, 2, 0, 0]));
    let mut hhea = 0x0001_0000u32.to_be_bytes().to_vec();
    hhea.extend(words(&[
        800, -200, 0, 500, 0, 0, 200, 1, 0, 0, 0, 0, 0, 0, 0, 2,
    ]));
    let mut maxp = 0x0000_5000u32.to_be_bytes().to_vec();
    maxp.extend(words(&[2]));
    let mut cpal = words(&[0, 2, 1, 2]);
    cpal.extend_from_slice(&14u32.to_be_bytes());
    cpal.extend(words(&[0]));
    cpal.extend_from_slice(&[0, 0, 255, 255, 255, 0, 0, 255]); // red, blue
    let paints = [
        (5, glyph(1, &scale(2.0, &linear()))),
        (6, glyph(1, &solid(0))),
        (7, composite(&glyph(1, &solid(1)), 23, &glyph(1, &solid(0)))),
        (8, solid(0)),
    ];
    let tables: [(&[u8; 4], Vec<u8>); 8] = [
        (b"COLR", colr(&paints, &[(6, [0, 0, 100, 200])])),
        (b"CPAL", cpal),
        (b"glyf", square),
        (b"head", head),
        (b"hhea", hhea),
        (b"hmtx", words(&[500, 0, 500, 0])),
        (b"loca", loca),
        (b"maxp", maxp),
    ];
    let mut offset = 12 + 16 * tables.len();
    let mut out = 0x0001_0000u32.to_be_bytes().to_vec();
    out.extend(words(&[tables.len() as i16, 0, 0, 0]));
    for (tag, body) in &tables {
        out.extend_from_slice(*tag);
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&(offset as u32).to_be_bytes());
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        offset += body.len().div_ceil(4) * 4;
    }
    for (_, body) in &tables {
        out.extend_from_slice(body);
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }
    out
}

fn svg(gid: u16) -> Option<String> {
    let bytes = font_bytes();
    let face = Face::parse_bytes(&bytes, 0).expect("face parses");
    glyph_to_svg_color(&face, gid)
}

fn balanced(out: &str) {
    assert_eq!(
        out.matches("<g").count(),
        out.matches("</g>").count(),
        "{out}"
    );
}

#[test]
fn transform_below_paint_glyph_stays_on_the_gradient() {
    let out = svg(5).expect("renders");
    balanced(&out);
    // The square is drawn untransformed; the scale moves the gradient.
    assert!(out.contains(r#"<path d="M 0 0"#), "{out}");
    assert!(
        out.contains(r#"gradientTransform="matrix(2 0 0 2 0 0)""#),
        "{out}"
    );
    // The viewBox is the square plus the margin, not the doubled one.
    assert!(out.contains(r#"viewBox="-32 -32 264 264""#), "{out}");
}

#[test]
fn clip_boxes_clip_the_drawing_and_size_the_view() {
    let out = svg(6).expect("renders");
    balanced(&out);
    assert!(
        out.contains(
            r#"<clipPath id="clip-0"><path d="M 0 0 L 100 0 L 100 200 L 0 200 Z"/></clipPath>"#
        ),
        "{out}"
    );
    assert!(
        out.contains(r#"<g clip-path="url(#clip-0)"><path d="M 0 0"#),
        "{out}"
    );
    assert!(out.contains(r#"viewBox="-32 -32 164 264""#), "{out}");
}

#[test]
fn composites_are_isolated_groups() {
    let out = svg(7).expect("renders");
    balanced(&out);
    let isolated = out
        .find(r#"<g style="isolation:isolate">"#)
        .expect("isolation");
    let blended = out
        .find(r#"<g style="mix-blend-mode:multiply">"#)
        .expect("blend");
    let red = out.find(r#"fill="rgb(255,0,0)""#).expect("backdrop");
    let blue = out.find(r#"fill="rgb(0,0,255)""#).expect("source");
    // The backdrop sits inside the isolated group, before the blended
    // source group.
    assert!(isolated < red && red < blended && blended < blue, "{out}");
}

#[test]
fn unbounded_glyphs_produce_no_svg() {
    assert!(svg(8).is_none());
}
