//! `<textPath>` placement of consumer-shaped glyph runs along a
//! referenced path.

use sigilbuzz::{Blob, Face};
use sigilbuzz_render::Rasterizer;

use crate::fonts::{align4, build_svg_table, record};

// =========================================================================
// textPath: consumer-pre-shaped runs along a referenced <path>
// =========================================================================
//
// PR #236 deferred SVG `<textPath>` because the renderer doesn't shape
// text. The new entry, `rasterize_svg_glyph_with_text_paths`, takes
// a pre-shaped run from the consumer (one record per visual glyph,
// carrying gid + x_advance) and walks the referenced path's
// arc-length, translating each glyph's outline onto its
// cumulative-advance position. Axis-aligned only; tangent rotation is
// 0.22.0 follow-up work.
//
// The font built below has gid 0 = .notdef (empty), gid 1 = SVG-bearing,
// gid 2 = a 100x100 square outlined glyph. The SVG document on gid 1
// references gid 2 via the textPath API.

/// Builds a tiny TrueType simple-glyph: an axis-aligned 100x100 square
/// at design-unit origin (0,0). Same encoding used by
/// `tests/varc_synthetic.rs`.
fn build_square_simple_glyph() -> Vec<u8> {
    let mut g = Vec::new();
    g.extend_from_slice(&1i16.to_be_bytes()); // numberOfContours
    g.extend_from_slice(&0i16.to_be_bytes()); // xMin
    g.extend_from_slice(&0i16.to_be_bytes()); // yMin
    g.extend_from_slice(&100i16.to_be_bytes()); // xMax
    g.extend_from_slice(&100i16.to_be_bytes()); // yMax
    g.extend_from_slice(&3u16.to_be_bytes()); // endPts[0] = 3 (4 points)
    g.extend_from_slice(&0u16.to_be_bytes()); // instructionLength
    g.extend_from_slice(&[0x01u8; 4]); // ON_CURVE flags, long-form coords
    for d in [0i16, 100, 0, -100] {
        g.extend_from_slice(&d.to_be_bytes());
    }
    for d in [0i16, 0, 100, 0] {
        g.extend_from_slice(&d.to_be_bytes());
    }
    g
}

/// Mirrors `build_svg_font` but installs a 100x100 square glyph at gid 2
/// so the textPath consumer can reference it.
fn build_svg_font_with_square_at_gid2(svg_payload: &[u8]) -> Vec<u8> {
    let head = {
        let mut h = Vec::new();
        h.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        h.extend_from_slice(&0u32.to_be_bytes());
        h.extend_from_slice(&0u32.to_be_bytes());
        h.extend_from_slice(&0x5F0F_3CF5u32.to_be_bytes());
        h.extend_from_slice(&0u16.to_be_bytes());
        h.extend_from_slice(&1024u16.to_be_bytes()); // upem
        h.extend_from_slice(&0u64.to_be_bytes());
        h.extend_from_slice(&0u64.to_be_bytes());
        h.extend_from_slice(&0i16.to_be_bytes());
        h.extend_from_slice(&0i16.to_be_bytes());
        h.extend_from_slice(&500i16.to_be_bytes());
        h.extend_from_slice(&500i16.to_be_bytes());
        h.extend_from_slice(&0u16.to_be_bytes());
        h.extend_from_slice(&8u16.to_be_bytes());
        h.extend_from_slice(&2i16.to_be_bytes());
        h.extend_from_slice(&0i16.to_be_bytes()); // indexToLocFormat = short
        h.extend_from_slice(&0i16.to_be_bytes());
        align4(&mut h);
        h
    };
    let maxp = {
        let mut m = Vec::new();
        m.extend_from_slice(&0x0000_5000u32.to_be_bytes());
        m.extend_from_slice(&3u16.to_be_bytes()); // 3 glyphs
        align4(&mut m);
        m
    };
    let hhea = {
        let mut h = Vec::new();
        h.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        h.extend_from_slice(&800i16.to_be_bytes());
        h.extend_from_slice(&(-200i16).to_be_bytes());
        h.extend_from_slice(&0i16.to_be_bytes());
        h.extend_from_slice(&500u16.to_be_bytes());
        h.extend_from_slice(&0i16.to_be_bytes());
        h.extend_from_slice(&0i16.to_be_bytes());
        h.extend_from_slice(&500i16.to_be_bytes());
        h.extend_from_slice(&1i16.to_be_bytes());
        h.extend_from_slice(&0i16.to_be_bytes());
        h.extend_from_slice(&0i16.to_be_bytes());
        for _ in 0..4 {
            h.extend_from_slice(&0i16.to_be_bytes());
        }
        h.extend_from_slice(&0i16.to_be_bytes());
        h.extend_from_slice(&3u16.to_be_bytes()); // numberOfHMetrics
        align4(&mut h);
        h
    };
    let hmtx = {
        let mut m = Vec::new();
        for _ in 0..3 {
            m.extend_from_slice(&500u16.to_be_bytes());
            m.extend_from_slice(&0i16.to_be_bytes());
        }
        align4(&mut m);
        m
    };
    let square = build_square_simple_glyph();
    let mut glyf = Vec::new();
    let off0 = glyf.len();
    let off1 = glyf.len();
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
        let mut l = Vec::new();
        for off in [off0, off1, off2, off3] {
            l.extend_from_slice(&((off / 2) as u16).to_be_bytes());
        }
        align4(&mut l);
        l
    };

    let svg = build_svg_table(&[(1, 1, svg_payload)]);
    let payloads: Vec<([u8; 4], &[u8])> = vec![
        (*b"SVG ", svg.as_slice()),
        (*b"glyf", glyf.as_slice()),
        (*b"head", head.as_slice()),
        (*b"hhea", hhea.as_slice()),
        (*b"hmtx", hmtx.as_slice()),
        (*b"loca", loca.as_slice()),
        (*b"maxp", maxp.as_slice()),
    ];
    let num_tables = payloads.len() as u16;
    let header_len = 12 + num_tables as usize * 16;
    let mut out = Vec::new();
    out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    out.extend_from_slice(&num_tables.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
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
fn textpath_axis_aligned_horizontal_line_places_glyphs_along_x() {
    // <defs><path id="line" d="M 10 50 L 200 50"/></defs>
    // <textPath href="#line">consumer-pre-shape</textPath>
    //
    // The line spans 190 user-space units along x at y=50. Three
    // pre-shaped glyphs (gid 2 = 100x100 square in design units) at
    // font_size=20 (scale 20/1024 ~0.0195) and x_advance=40 each:
    //   glyph 0 -> x=10  (cum 0)
    //   glyph 1 -> x=50  (cum 40)
    //   glyph 2 -> x=90  (cum 80)
    // Each glyph paints a tiny ~2x2 px square in user space. We
    // assert the rasterized canvas has opaque pixels around each
    // expected glyph origin and is empty above/below the line.
    let payload = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 220 100">
        <defs><path id="line" d="M 10 50 L 200 50"/></defs>
        <textPath xlink:href="#line" fill="#FF0000"/>
    </svg>"##;
    let bytes = build_svg_font_with_square_at_gid2(payload);
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();

    let rast = Rasterizer::new();
    let runs = vec![
        sigilbuzz_render::TextPathGlyph {
            gid: 2,
            x_advance: 40.0,
        },
        sigilbuzz_render::TextPathGlyph {
            gid: 2,
            x_advance: 40.0,
        },
        sigilbuzz_render::TextPathGlyph {
            gid: 2,
            x_advance: 40.0,
        },
    ];
    let inputs = vec![sigilbuzz_render::TextPathInput {
        text_path_id: "line",
        font_size: 20.0,
        glyph_runs: runs,
    }];
    let pix = rast
        .rasterize_svg_glyph_with_text_paths(&face, 1, 220.0, &[], &inputs)
        .unwrap();

    // Canvas is at least the viewBox width; rendered with size_pt = 220
    // and viewBox 220x100, scale factor = 1.0. Each glyph is a 100-unit
    // square in design units -> ~2 user-space units after font_size/upem
    // (20/1024) -> ~2 pixels (because user-space -> pixel scale = 1).
    assert!(pix.width >= 220);
    assert_eq!(pix.height, 100);

    // Each placed glyph origin is at a known x; the square's
    // top-right corner extends to (origin_x+~2, origin_y-~2). Sample
    // a generous window around each origin and confirm at least one
    // red-ish pixel landed there.
    let centers: [(u32, u32); 3] = [(10, 50), (50, 50), (90, 50)];
    for (cx, cy) in centers {
        let mut found = false;
        for dy in 0..6_u32 {
            for dx in 0..6_u32 {
                let x = cx + dx;
                let y = cy.saturating_sub(dy);
                if x < pix.width && y < pix.height {
                    let p = pix.get(x, y);
                    if p[3] > 0 && p[0] > 100 {
                        found = true;
                    }
                }
            }
        }
        assert!(
            found,
            "expected red pixels near glyph origin at ({cx},{cy})"
        );
    }

    // Off-line region (y far above the line, x clearly past the path
    // end) must remain transparent.
    assert_eq!(pix.get(150, 5)[3], 0);
}

#[test]
fn textpath_glyphs_past_path_end_silently_drop() {
    // Same setup, but supply 10 glyphs each with x_advance=40 against
    // a 190-unit path. After cum > 190 the renderer should silently
    // drop the rest (path-cycling deferred). The render must not
    // panic and must place at least the first few glyphs.
    let payload = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 220 100">
        <defs><path id="short" d="M 10 50 L 200 50"/></defs>
        <textPath xlink:href="#short" fill="#FF0000"/>
    </svg>"##;
    let bytes = build_svg_font_with_square_at_gid2(payload);
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();

    let rast = Rasterizer::new();
    let runs: Vec<_> = (0..10)
        .map(|_| sigilbuzz_render::TextPathGlyph {
            gid: 2,
            x_advance: 40.0,
        })
        .collect();
    let inputs = vec![sigilbuzz_render::TextPathInput {
        text_path_id: "short",
        font_size: 20.0,
        glyph_runs: runs,
    }];
    let pix = rast
        .rasterize_svg_glyph_with_text_paths(&face, 1, 220.0, &[], &inputs)
        .unwrap();
    // At least one red pixel exists somewhere on the line.
    let mut any_red = false;
    for y in 40..60 {
        for x in 0..pix.width {
            let p = pix.get(x, y);
            if p[3] > 0 && p[0] > 100 {
                any_red = true;
                break;
            }
        }
        if any_red {
            break;
        }
    }
    assert!(any_red, "at least the first glyph should have rendered");
}

#[test]
fn textpath_curved_path_translates_glyphs_along_curve_axis_aligned() {
    // Single cubic Bézier: M 10 80 C 10 0, 210 0, 210 80. A
    // gentle-arch curve from (10,80) to (210,80). Three pre-shaped
    // glyphs walk the cumulative arc length and land along the
    // curve's path-position. Axis-aligned: glyph outlines are *not*
    // rotated to follow the tangent (deferred work).
    let payload = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 220 100">
        <defs><path id="curve" d="M 10 80 C 10 0, 210 0, 210 80"/></defs>
        <textPath xlink:href="#curve" fill="#0000FF"/>
    </svg>"##;
    let bytes = build_svg_font_with_square_at_gid2(payload);
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();

    let rast = Rasterizer::new();
    let runs = vec![
        sigilbuzz_render::TextPathGlyph {
            gid: 2,
            x_advance: 60.0,
        },
        sigilbuzz_render::TextPathGlyph {
            gid: 2,
            x_advance: 60.0,
        },
        sigilbuzz_render::TextPathGlyph {
            gid: 2,
            x_advance: 60.0,
        },
    ];
    let inputs = vec![sigilbuzz_render::TextPathInput {
        text_path_id: "curve",
        font_size: 20.0,
        glyph_runs: runs,
    }];
    let pix = rast
        .rasterize_svg_glyph_with_text_paths(&face, 1, 220.0, &[], &inputs)
        .unwrap();

    // First glyph is at the curve start (10, 80). Confirm a blue
    // pixel near there.
    let mut start_blue = false;
    for dy in 0..6 {
        for dx in 0..6 {
            let p = pix.get(10 + dx, 80u32.saturating_sub(dy));
            if p[3] > 0 && p[2] > 100 {
                start_blue = true;
            }
        }
    }
    assert!(start_blue, "start of curve should have a blue glyph");

    // Mid-arc: at cumulative advance ~60, the arc-length walk on a
    // gentle arch lands near x ~30..70 (the curve climbs slowly at
    // first because of the y=0 controls bowing it up). Confirm
    // *some* blue pixel exists in the mid-canvas region above y=80.
    let mut mid_blue = false;
    for y in 0..80 {
        for x in 30..120 {
            let p = pix.get(x, y);
            if p[3] > 0 && p[2] > 100 {
                mid_blue = true;
                break;
            }
        }
    }
    assert!(mid_blue, "mid-arc glyph should land above y=80 on the arch");
}

#[test]
fn textpath_unmatched_id_silently_skips() {
    // <textPath href="#missing"> with no matching def: the run is
    // silently dropped. The base SVG document still renders.
    let payload = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100">
        <path d="M 0 0 L 100 0 L 100 100 L 0 100 Z" fill="#00FF00"/>
        <textPath xlink:href="#missing" fill="#FF0000"/>
    </svg>"##;
    let bytes = build_svg_font_with_square_at_gid2(payload);
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();

    let rast = Rasterizer::new();
    let inputs = vec![sigilbuzz_render::TextPathInput {
        text_path_id: "absent_id",
        font_size: 20.0,
        glyph_runs: vec![sigilbuzz_render::TextPathGlyph {
            gid: 2,
            x_advance: 30.0,
        }],
    }];
    let pix = rast
        .rasterize_svg_glyph_with_text_paths(&face, 1, 100.0, &[], &inputs)
        .unwrap();
    // Green square still renders.
    let p = pix.get(50, 50);
    assert!(
        p[1] > 200 && p[3] == 255,
        "background square should be solid green, got {p:?}"
    );
}
