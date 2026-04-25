#![allow(missing_docs)] // Criterion's `criterion_group!` macro generates undocumented items.

//! Slug-encoder bench: outline extraction + flattening + band
//! decomposition for representative glyphs across two fonts.
//!
//! No rustybuzz comparison — rustybuzz does not ship a Slug-style
//! GPU outline encoder, so this bench tracks sigilbuzz-gpu against
//! its own historical numbers in `docs/PERFORMANCE.md`.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use sigilbuzz::{Blob, Face};
use sigilbuzz_gpu::{encode_glyph, SlugOptions};

const OPEN_SANS: &[u8] = include_bytes!("../../../tests/fixtures/opensans_regular.ttf");
const AMIRI: &[u8] = include_bytes!("../../../tests/fixtures/amiri_regular.ttf");

fn glyph_for(face: &Face<'_>, ch: char) -> u16 {
    face.cmap()
        .expect("cmap")
        .glyph_id(ch)
        .unwrap_or_else(|| panic!("missing glyph for {ch:?}"))
}

fn bench_encode(c: &mut Criterion) {
    let opens_blob = Blob::new(OPEN_SANS);
    let opens_face = Face::parse(&opens_blob, 0).expect("parse opens");
    let amiri_blob = Blob::new(AMIRI);
    let amiri_face = Face::parse(&amiri_blob, 0).expect("parse amiri");

    // Representative shapes:
    //   'A'  — simple, mostly-straight base glyph
    //   'g'  — descender + closed curve, oblique two-storey
    //   'O'  — pure oval, all four cubic quadrants
    //   'ا'  — Arabic alef, long vertical with tiny tail
    let cases: &[(&str, u16)] = &[
        ("opensans_A", glyph_for(&opens_face, 'A')),
        ("opensans_g", glyph_for(&opens_face, 'g')),
        ("opensans_O", glyph_for(&opens_face, 'O')),
        ("amiri_alef", glyph_for(&amiri_face, '\u{0627}')),
    ];
    let opts = SlugOptions::default();

    let mut group = c.benchmark_group("encode");
    for (name, gid) in cases {
        let face = if name.starts_with("amiri") {
            &amiri_face
        } else {
            &opens_face
        };
        group.bench_function(*name, |b| {
            b.iter(|| {
                let glyph = encode_glyph(face, black_box(*gid), &opts).expect("encode");
                black_box(glyph.segments.len() + glyph.bands.len())
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_encode);
criterion_main!(benches);
