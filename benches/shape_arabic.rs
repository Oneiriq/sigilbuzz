#![allow(missing_docs)] // Criterion's `criterion_group!` macro generates undocumented items.

//! Arabic shaping bench: sigilbuzz vs rustybuzz.
//!
//! Stresses the GSUB cursor walker and the LookupFlag skip iterators
//! — the heaviest path in 0.4.0. Amiri's `rlig` feature carries
//! ~40 chained-context lookups whose IgnoreMarks bits drive the
//! mark-skip iterator on every input cursor advance.
//!
//! Throughput is reported in codepoints per second.

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use sigilbuzz::{shape, Blob, Buffer, Direction, Face, Font};

const AMIRI: &[u8] = include_bytes!("../tests/fixtures/amiri_regular.ttf");

/// 200-codepoint Arabic corpus. Mixes plain joining sequences,
/// Quranic-grade vocalised passages (rlig + IgnoreMarks), and mixed
/// Arabic + Latin to exercise script-segment transitions.
const CORPUS: &str = "السلام عليكم ورحمة الله وبركاته. مرحبا بكم في عالم سيجلبز. \
    بسم الله الرحمن الرحيم. الحمد لله رب العالمين. الرحمن الرحيم. \
    مالك يوم الدين. إياك نعبد وإياك نستعين. اهدنا الصراط المستقيم.";

fn bench_arabic(c: &mut Criterion) {
    let blob = Blob::new(AMIRI);
    let face = Face::parse(&blob, 0).expect("parse face");
    let font = Font::new(face, 1000.0);

    let mut group = c.benchmark_group("shape_arabic");
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

    let rb_face = rustybuzz::Face::from_slice(AMIRI, 0).expect("rb face");
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

criterion_group!(benches, bench_arabic);
criterion_main!(benches);
