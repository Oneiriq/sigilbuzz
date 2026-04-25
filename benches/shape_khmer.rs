#![allow(missing_docs)] // Criterion's `criterion_group!` macro generates undocumented items.

//! Khmer shaping bench: sigilbuzz vs rustybuzz.
//!
//! Stresses the Universal Shaping Engine (USE) state machine —
//! category classifier + syllable matcher + USE-specific reorder
//! pass. Khmer is the cleanest pure-USE workload because it has no
//! AAT fallback and no Indic-2 reorder shortcut.
//!
//! Throughput is reported in codepoints per second.

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use sigilbuzz::{shape, Blob, Buffer, Face, Font};

const NOTO_KHMER: &[u8] = include_bytes!("../tests/fonts/NotoSansKhmer-Regular.ttf");

/// Khmer corpus exercising:
///   * plain base + post-base vowel (kaa)
///   * pre-base vowel sign (sign-e, requires reorder)
///   * coeng (subscript) stacks
///   * register shifters (muusikatoan / triisap)
///   * final marks (nikahit, reahmuk)
// Note: Khmer text uses U+200B (ZWSP) at word boundaries in the wild;
// we use ASCII spaces here to keep the source byte-stable and clippy
// happy ("invisible character detected" fires on ZWSP in literals).
const CORPUS: &str = "សួស្ដី ពិភពលោក។ យើងកំពុងសាកល្បងម៉ាស៊ីនបង្កើតរូបអក្សរស៊ីហ្ស៊ីលប័ហ្ស។ \
    ភាសាខ្មែរជាភាសាជាតិ។ សៀវភៅនៅក្នុងបណ្ណាល័យ។ \
    សាលារៀនបើកម៉ោងប្រាំបី។ កុមារៀនអក្សរ។";

fn bench_khmer(c: &mut Criterion) {
    let blob = Blob::new(NOTO_KHMER);
    let face = Face::parse(&blob, 0).expect("parse face");
    let font = Font::new(face, 1000.0);

    let mut group = c.benchmark_group("shape_khmer");
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

    let rb_face = rustybuzz::Face::from_slice(NOTO_KHMER, 0).expect("rb face");
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

criterion_group!(benches, bench_khmer);
criterion_main!(benches);
