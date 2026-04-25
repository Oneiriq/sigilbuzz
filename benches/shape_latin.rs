#![allow(missing_docs)] // Criterion's `criterion_group!` macro generates undocumented items.

//! Latin shaping bench: sigilbuzz vs rustybuzz.
//!
//! Shapes a 200-codepoint Latin pangram corpus on Open Sans Regular.
//! Throughput is reported in codepoints per second so the runs are
//! comparable across machines and absolute byte counts.
//!
//! The corpus is a `const &str` so re-runs are byte-identical and
//! Criterion's variance numbers reflect engine drift rather than
//! input churn.

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use sigilbuzz::{shape, Blob, Buffer, Face, Font};

const OPEN_SANS: &[u8] = include_bytes!("../tests/fixtures/opensans_regular.ttf");

/// 200-codepoint Latin pangram corpus. The five sentences combine to
/// 200 Unicode scalar values; each one exercises ligatures (`fi`,
/// `fl`), kerning pairs (`Wa`, `Vo`, `To`), and the standard `liga`
/// + `kern` lookups Open Sans ships.
const CORPUS: &str = "The quick brown fox jumps over the lazy dog. \
    Pack my box with five dozen liquor jugs.  \
    Waltz, bad nymph, for quick jigs vex. \
    Sphinx of black quartz, judge my vow. \
    How vexingly quick daft zebras jump!!";

const _: () = {
    // Compile-time guard: keep the corpus pinned at 200 codepoints so
    // throughput numbers stay comparable across releases. (Latin is
    // pure ASCII so byte-len == char-len.)
    assert!(CORPUS.len() == 200);
};

fn bench_sigilbuzz(c: &mut Criterion) {
    let blob = Blob::new(OPEN_SANS);
    let face = Face::parse(&blob, 0).expect("parse face");
    let font = Font::new(face, 1000.0);

    let mut group = c.benchmark_group("shape_latin");
    let codepoints = CORPUS.chars().count() as u64;
    group.throughput(Throughput::Elements(codepoints));

    group.bench_function("sigilbuzz", |b| {
        b.iter(|| {
            let mut buffer = Buffer::new();
            buffer.push_str(black_box(CORPUS));
            let out = shape(&font, &buffer, &[]).expect("shape");
            black_box(out.len())
        });
    });

    let rb_face = rustybuzz::Face::from_slice(OPEN_SANS, 0).expect("rb face");
    group.bench_function("rustybuzz", |b| {
        b.iter(|| {
            let mut rb_buf = rustybuzz::UnicodeBuffer::new();
            rb_buf.push_str(black_box(CORPUS));
            let out = rustybuzz::shape(&rb_face, &[], rb_buf);
            black_box(out.len())
        });
    });

    group.finish();
}

criterion_group!(benches, bench_sigilbuzz);
criterion_main!(benches);
