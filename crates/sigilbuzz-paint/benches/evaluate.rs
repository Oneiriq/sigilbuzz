#![allow(missing_docs)] // Criterion's `criterion_group!` macro generates undocumented items.

//! COLRv1 evaluator bench. Drives `sigilbuzz_paint::evaluate` over a
//! suite of hand-authored COLR fixtures that mirror the parser's
//! integration tests in `tests/evaluator.rs`. Each fixture exercises
//! a different paint shape so regressions show up per-feature
//! instead of being averaged into one number.

use criterion::{criterion_group, criterion_main, Criterion};
use sigilbuzz::Face;
use sigilbuzz_paint::{evaluate, evaluate_at_coords};
use std::hint::black_box;

// =========================================================================
// Fixture builders: copies of the helpers in tests/evaluator.rs. The
// duplication is intentional: benches cannot reach into a sibling
// crate's test sources, and pulling in a third file just for the
// helpers would mean wiring up a `[[bin]]` or a path-included
// `mod` outside the `src/` tree. Keep these in sync with the test
// helpers if you add a new fixture in either place.
// =========================================================================

fn build_face_bytes(colr: &[u8], cpal: &[u8]) -> Vec<u8> {
    let dir_len = 12 + 2 * 16;
    let cpal_off = dir_len;
    let colr_off = cpal_off + cpal.len();

    let mut out = Vec::new();
    out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    out.extend_from_slice(&2u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());

    out.extend_from_slice(b"COLR");
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&(colr_off as u32).to_be_bytes());
    out.extend_from_slice(&(colr.len() as u32).to_be_bytes());

    out.extend_from_slice(b"CPAL");
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&(cpal_off as u32).to_be_bytes());
    out.extend_from_slice(&(cpal.len() as u32).to_be_bytes());

    out.extend_from_slice(cpal);
    out.extend_from_slice(colr);
    out
}

fn build_cpal_v0(colors: &[(u8, u8, u8, u8)]) -> Vec<u8> {
    let num_palettes: u16 = 1;
    let entries = colors.len() as u16;
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&entries.to_be_bytes());
    out.extend_from_slice(&num_palettes.to_be_bytes());
    out.extend_from_slice(&entries.to_be_bytes());
    let header_plus_indices = 12 + num_palettes as usize * 2;
    out.extend_from_slice(&(header_plus_indices as u32).to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    for (r, g, b, a) in colors {
        out.push(*b);
        out.push(*g);
        out.push(*r);
        out.push(*a);
    }
    out
}

fn build_v1_header(glyph_id: u16) -> Vec<u8> {
    let header_len = 30u32;
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&header_len.to_be_bytes());
    out.extend_from_slice(&header_len.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&header_len.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&1u32.to_be_bytes());
    out.extend_from_slice(&glyph_id.to_be_bytes());
    out.extend_from_slice(&10u32.to_be_bytes());
    out
}

fn f2dot14(v: f32) -> [u8; 2] {
    let raw = (v * 16384.0).round() as i16;
    raw.to_be_bytes()
}

// =========================================================================
// Fixture suites mirroring tests/evaluator.rs.
// =========================================================================

fn solid_fixture() -> Vec<u8> {
    let mut colr = build_v1_header(7);
    colr.push(2); // PaintSolid
    colr.extend_from_slice(&1u16.to_be_bytes()); // paletteIndex
    colr.extend_from_slice(&f2dot14(1.0)); // alpha
    let cpal = build_cpal_v0(&[(255, 0, 0, 255), (0, 255, 0, 255)]);
    build_face_bytes(&colr, &cpal)
}

fn linear_gradient_fixture() -> Vec<u8> {
    let mut colr = build_v1_header(33);
    let paint_start = colr.len();
    colr.push(4); // PaintLinearGradient
    colr.extend_from_slice(&[0, 0, 0]);
    colr.extend_from_slice(&10i16.to_be_bytes());
    colr.extend_from_slice(&20i16.to_be_bytes());
    colr.extend_from_slice(&30i16.to_be_bytes());
    colr.extend_from_slice(&40i16.to_be_bytes());
    colr.extend_from_slice(&50i16.to_be_bytes());
    colr.extend_from_slice(&60i16.to_be_bytes());

    let cl_start = colr.len();
    let cl_rel = (cl_start - paint_start) as u32;
    colr[paint_start + 1] = ((cl_rel >> 16) & 0xff) as u8;
    colr[paint_start + 2] = ((cl_rel >> 8) & 0xff) as u8;
    colr[paint_start + 3] = (cl_rel & 0xff) as u8;
    colr.push(0); // extend = Pad
    colr.extend_from_slice(&2u16.to_be_bytes()); // numStops
    colr.extend_from_slice(&f2dot14(0.0));
    colr.extend_from_slice(&0u16.to_be_bytes());
    colr.extend_from_slice(&f2dot14(1.0));
    colr.extend_from_slice(&f2dot14(1.0));
    colr.extend_from_slice(&1u16.to_be_bytes());
    colr.extend_from_slice(&f2dot14(1.0));

    let cpal = build_cpal_v0(&[(255, 0, 0, 255), (0, 0, 255, 255)]);
    build_face_bytes(&colr, &cpal)
}

fn translate_scale_fixture() -> Vec<u8> {
    // Tree: Translate(10, 0) -> Scale(2, 2) -> Solid.
    let mut colr = build_v1_header(7);
    let translate_start = colr.len();
    colr.push(14);
    colr.extend_from_slice(&[0, 0, 0]);
    colr.extend_from_slice(&10i16.to_be_bytes());
    colr.extend_from_slice(&0i16.to_be_bytes());

    let scale_start = colr.len();
    let rel = (scale_start - translate_start) as u32;
    colr[translate_start + 1] = ((rel >> 16) & 0xff) as u8;
    colr[translate_start + 2] = ((rel >> 8) & 0xff) as u8;
    colr[translate_start + 3] = (rel & 0xff) as u8;
    colr.push(16);
    colr.extend_from_slice(&[0, 0, 0]);
    colr.extend_from_slice(&f2dot14(2.0));
    colr.extend_from_slice(&f2dot14(2.0));

    let solid_start = colr.len();
    let rel2 = (solid_start - scale_start) as u32;
    colr[scale_start + 1] = ((rel2 >> 16) & 0xff) as u8;
    colr[scale_start + 2] = ((rel2 >> 8) & 0xff) as u8;
    colr[scale_start + 3] = (rel2 & 0xff) as u8;
    colr.push(2);
    colr.extend_from_slice(&0u16.to_be_bytes());
    colr.extend_from_slice(&f2dot14(1.0));

    let cpal = build_cpal_v0(&[(255, 255, 255, 255)]);
    build_face_bytes(&colr, &cpal)
}

fn composite_fixture() -> Vec<u8> {
    let mut colr = build_v1_header(7);
    let composite_start = colr.len();
    colr.push(32); // PaintComposite
    colr.extend_from_slice(&[0, 0, 0]);
    colr.push(13); // mode = Screen
    colr.extend_from_slice(&[0, 0, 0]);

    let src_start = colr.len();
    let src_rel = (src_start - composite_start) as u32;
    colr[composite_start + 1] = ((src_rel >> 16) & 0xff) as u8;
    colr[composite_start + 2] = ((src_rel >> 8) & 0xff) as u8;
    colr[composite_start + 3] = (src_rel & 0xff) as u8;
    colr.push(2);
    colr.extend_from_slice(&0u16.to_be_bytes());
    colr.extend_from_slice(&f2dot14(1.0));

    let bd_start = colr.len();
    let bd_rel = (bd_start - composite_start) as u32;
    colr[composite_start + 5] = ((bd_rel >> 16) & 0xff) as u8;
    colr[composite_start + 6] = ((bd_rel >> 8) & 0xff) as u8;
    colr[composite_start + 7] = (bd_rel & 0xff) as u8;
    colr.push(2);
    colr.extend_from_slice(&1u16.to_be_bytes());
    colr.extend_from_slice(&f2dot14(1.0));

    let cpal = build_cpal_v0(&[(255, 0, 0, 255), (0, 255, 0, 255)]);
    build_face_bytes(&colr, &cpal)
}

fn radial_gradient_fixture() -> Vec<u8> {
    let mut colr = build_v1_header(50);
    let paint_start = colr.len();
    colr.push(6); // PaintRadialGradient
    colr.extend_from_slice(&[0, 0, 0]);
    colr.extend_from_slice(&0i16.to_be_bytes());
    colr.extend_from_slice(&0i16.to_be_bytes());
    colr.extend_from_slice(&0u16.to_be_bytes());
    colr.extend_from_slice(&100i16.to_be_bytes());
    colr.extend_from_slice(&100i16.to_be_bytes());
    colr.extend_from_slice(&200u16.to_be_bytes());

    let cl_start = colr.len();
    let cl_rel = (cl_start - paint_start) as u32;
    colr[paint_start + 1] = ((cl_rel >> 16) & 0xff) as u8;
    colr[paint_start + 2] = ((cl_rel >> 8) & 0xff) as u8;
    colr[paint_start + 3] = (cl_rel & 0xff) as u8;
    colr.push(0);
    colr.extend_from_slice(&2u16.to_be_bytes());
    colr.extend_from_slice(&f2dot14(0.0));
    colr.extend_from_slice(&0u16.to_be_bytes());
    colr.extend_from_slice(&f2dot14(1.0));
    colr.extend_from_slice(&f2dot14(1.0));
    colr.extend_from_slice(&1u16.to_be_bytes());
    colr.extend_from_slice(&f2dot14(1.0));

    let cpal = build_cpal_v0(&[(255, 0, 0, 255), (0, 0, 255, 255)]);
    build_face_bytes(&colr, &cpal)
}

fn sweep_gradient_fixture() -> Vec<u8> {
    let mut colr = build_v1_header(60);
    let paint_start = colr.len();
    colr.push(8);
    colr.extend_from_slice(&[0, 0, 0]);
    colr.extend_from_slice(&50i16.to_be_bytes());
    colr.extend_from_slice(&50i16.to_be_bytes());
    colr.extend_from_slice(&f2dot14(0.0));
    colr.extend_from_slice(&f2dot14(1.0));

    let cl_start = colr.len();
    let cl_rel = (cl_start - paint_start) as u32;
    colr[paint_start + 1] = ((cl_rel >> 16) & 0xff) as u8;
    colr[paint_start + 2] = ((cl_rel >> 8) & 0xff) as u8;
    colr[paint_start + 3] = (cl_rel & 0xff) as u8;
    colr.push(2);
    colr.extend_from_slice(&3u16.to_be_bytes());
    colr.extend_from_slice(&f2dot14(0.0));
    colr.extend_from_slice(&0u16.to_be_bytes());
    colr.extend_from_slice(&f2dot14(1.0));
    colr.extend_from_slice(&f2dot14(0.5));
    colr.extend_from_slice(&1u16.to_be_bytes());
    colr.extend_from_slice(&f2dot14(1.0));
    colr.extend_from_slice(&f2dot14(1.0));
    colr.extend_from_slice(&2u16.to_be_bytes());
    colr.extend_from_slice(&f2dot14(1.0));

    let cpal = build_cpal_v0(&[(255, 0, 0, 255), (0, 255, 0, 255), (0, 0, 255, 255)]);
    build_face_bytes(&colr, &cpal)
}

fn var_solid_fixture() -> Vec<u8> {
    let mut colr = build_v1_header(7);
    colr.push(3);
    colr.extend_from_slice(&0u16.to_be_bytes());
    colr.extend_from_slice(&f2dot14(1.0));
    colr.extend_from_slice(&0xFFFF_FFFFu32.to_be_bytes());
    let cpal = build_cpal_v0(&[(64, 128, 192, 255)]);
    build_face_bytes(&colr, &cpal)
}

fn bench_evaluate(c: &mut Criterion) {
    let cases: &[(&str, Vec<u8>, u16)] = &[
        ("solid", solid_fixture(), 7),
        ("linear_gradient", linear_gradient_fixture(), 33),
        ("translate_scale", translate_scale_fixture(), 7),
        ("composite", composite_fixture(), 7),
        ("radial_gradient", radial_gradient_fixture(), 50),
        ("sweep_gradient", sweep_gradient_fixture(), 60),
    ];

    let mut group = c.benchmark_group("evaluate");
    for (name, bytes, gid) in cases {
        let face = Face::parse_bytes(bytes, 0).expect("face");
        group.bench_function(*name, |b| {
            b.iter(|| {
                let cmds = evaluate(&face, black_box(*gid));
                black_box(cmds.len())
            });
        });
    }

    // PaintVar* with a coord slice: separate benchmark because it
    // uses `evaluate_at_coords` rather than `evaluate`.
    let var_bytes = var_solid_fixture();
    let var_face = Face::parse_bytes(&var_bytes, 0).expect("face");
    group.bench_function("var_solid_at_coords", |b| {
        b.iter(|| {
            let cmds = evaluate_at_coords(&var_face, black_box(7), &[]);
            black_box(cmds.len())
        });
    });

    group.finish();
}

criterion_group!(benches, bench_evaluate);
criterion_main!(benches);
