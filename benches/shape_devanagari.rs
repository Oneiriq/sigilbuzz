#![allow(missing_docs)] // Criterion's `criterion_group!` macro generates undocumented items.

//! Devanagari shaping bench: sigilbuzz vs rustybuzz.
//!
//! Stresses the Indic syllable classifier, reph reorder, and the
//! Indic feature dispatch chain (`nukt` / `akhn` / `rphf` / `blwf` /
//! `half` / `pstf` / `cjct` plus the GSUB pres/blws/psts post-pass).
//!
//! Throughput is reported in codepoints per second.

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use sigilbuzz::{shape, Blob, Buffer, Face, Font};

const NOTO_DEVA: &[u8] = include_bytes!("../tests/fonts/NotoSansDevanagari-Regular.ttf");

/// Devanagari corpus exercising:
///   * plain consonant + matra
///   * reph reorder (ra + virama at start of syllable)
///   * conjunct stacks (virama-joined consonant pairs)
///   * pre-base matra reorder (i-matra → before base)
///   * Devanagari digits (Symbol pass-through)
const CORPUS: &str = "नमस्ते दुनिया। हम सिजिलबज़ का परीक्षण कर रहे हैं। \
    राष्ट्रीय भाषा हिंदी है। संसद में चर्चा चल रही है। \
    विद्यालय खुल गया है। पुस्तकालय बंद है। शिक्षक पढ़ा रहे हैं। \
    छात्र ध्यान से सुन रहे हैं।";

fn bench_devanagari(c: &mut Criterion) {
    let blob = Blob::new(NOTO_DEVA);
    let face = Face::parse(&blob, 0).expect("parse face");
    let font = Font::new(face, 1000.0);

    let mut group = c.benchmark_group("shape_devanagari");
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

    let rb_face = rustybuzz::Face::from_slice(NOTO_DEVA, 0).expect("rb face");
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

criterion_group!(benches, bench_devanagari);
criterion_main!(benches);
