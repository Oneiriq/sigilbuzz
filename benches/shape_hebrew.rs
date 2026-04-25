#![allow(missing_docs)] // Criterion's `criterion_group!` macro generates undocumented items.

//! Hebrew shaping bench: sigilbuzz vs rustybuzz.
//!
//! Stresses the GPOS mark-to-base / mark-to-mark anchor pipeline.
//! Hebrew is non-cursive, so no joining state machine fires; the
//! interesting cost is anchor lookup, niqqud / cantillation
//! stacking, and RTL iteration.
//!
//! Throughput is reported in codepoints per second.

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use sigilbuzz::{shape, Blob, Buffer, Direction, Face, Font};

const NOTO_HEBREW: &[u8] = include_bytes!("../tests/fonts/NotoSansHebrew-Regular.ttf");

/// Hebrew corpus exercising plain consonants, niqqud-stacked words,
/// cantillation marks, and final-form letters. Includes the opening
/// of Genesis 1:1 — the canonical mkmk stress test.
const CORPUS: &str = "שלום עולם. אנחנו בודקים את מנוע סיגלבז. \
    בְּרֵאשִׁית בָּרָא אֱלֹהִים אֵת הַשָּׁמַיִם וְאֵת הָאָרֶץ. \
    הללויה. בוקר טוב. תודה רבה. ירושלים בירת ישראל. \
    הספר על השולחן. הילד קורא בכיתה.";

fn bench_hebrew(c: &mut Criterion) {
    let blob = Blob::new(NOTO_HEBREW);
    let face = Face::parse(&blob, 0).expect("parse face");
    let font = Font::new(face, 1000.0);

    let mut group = c.benchmark_group("shape_hebrew");
    let codepoints = CORPUS.chars().count() as u64;
    group.throughput(Throughput::Elements(codepoints));

    group.bench_function("sigilbuzz", |b| {
        b.iter(|| {
            let mut buffer = Buffer::new();
            buffer.set_direction(Direction::Rtl);
            buffer.push_str(black_box(CORPUS));
            let out = shape(&font, &buffer, &[]).expect("shape");
            black_box(out.len())
        });
    });

    let rb_face = rustybuzz::Face::from_slice(NOTO_HEBREW, 0).expect("rb face");
    group.bench_function("rustybuzz", |b| {
        b.iter(|| {
            let mut rb_buf = rustybuzz::UnicodeBuffer::new();
            rb_buf.push_str(black_box(CORPUS));
            rb_buf.set_direction(rustybuzz::Direction::RightToLeft);
            let out = rustybuzz::shape(&rb_face, &[], rb_buf);
            black_box(out.len())
        });
    });

    group.finish();
}

criterion_group!(benches, bench_hebrew);
criterion_main!(benches);
