//! `sigilbuzz slug`: encode a glyph for GPU rendering.
//!
//! Wraps [`sigilbuzz_gpu::encode_glyph`] and emits the resulting
//! [`SlugGlyph`](sigilbuzz_gpu::SlugGlyph) as hand-rolled JSON. The
//! binary does not depend on `serde` (see `docs/deps.md`).

use std::path::PathBuf;

use clap::Args as ClapArgs;

use sigilbuzz::{Blob, Face};
use sigilbuzz_gpu::{encode_glyph, SlugOptions};

use super::util::{read_font, with_stdout, CliResult};

/// Arguments for `sigilbuzz slug`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Path to the font file.
    pub font: PathBuf,
    /// Glyph id to encode.
    pub gid: u16,
    /// Override the encoder's auto-picked band count.
    #[arg(long)]
    pub bands: Option<u32>,
    /// Cubic-flattening tolerance in design units.
    #[arg(long)]
    pub cubic_tolerance: Option<f32>,
}

/// Runs `sigilbuzz slug`.
pub fn run(args: Args) -> CliResult {
    let bytes = read_font(&args.font)?;
    let blob = Blob::from_vec(bytes);
    let face = Face::parse(&blob, 0).map_err(|e| format!("parse face: {e:?}"))?;

    let opts = SlugOptions {
        band_count: args.bands,
        cubic_tolerance: args
            .cubic_tolerance
            .unwrap_or(SlugOptions::DEFAULT_CUBIC_TOLERANCE),
    };

    let glyph = match encode_glyph(&face, args.gid, &opts) {
        Some(g) => g,
        None => {
            return Err(format!(
                "gid {} has no rasterisable outline (whitespace, missing, or out of range)",
                args.gid
            ));
        }
    };

    // Hand-rolled JSON. SlugGlyph is small and the field layout is
    // stable, so no serde dependency is needed.
    with_stdout(|out| {
        write!(out, "{{\"bbox\":")?;
        write!(
            out,
            "{{\"xmin\":{},\"ymin\":{},\"xmax\":{},\"ymax\":{}}}",
            f(glyph.bbox.xmin),
            f(glyph.bbox.ymin),
            f(glyph.bbox.xmax),
            f(glyph.bbox.ymax),
        )?;
        write!(out, ",\"bands\":[")?;
        for (i, b) in glyph.bands.iter().enumerate() {
            if i > 0 {
                write!(out, ",")?;
            }
            write!(
                out,
                "{{\"segment_offset\":{},\"segment_count\":{}}}",
                b.segment_offset, b.segment_count
            )?;
        }
        write!(out, "],\"segments\":[")?;
        for (i, s) in glyph.segments.iter().enumerate() {
            if i > 0 {
                write!(out, ",")?;
            }
            write!(
                out,
                "{{\"p0\":[{},{}],\"p1\":[{},{}],\"p2\":[{},{}]}}",
                f(s.p0.x),
                f(s.p0.y),
                f(s.p1.x),
                f(s.p1.y),
                f(s.p2.x),
                f(s.p2.y),
            )?;
        }
        writeln!(out, "]}}")
    })
}

/// Format an `f32` deterministically. JSON does not natively allow
/// `NaN` / `Infinity`, so non-finite values collapse to `0`.
fn f(v: f32) -> String {
    if !v.is_finite() {
        return "0".to_string();
    }
    // Strip trailing zeros and a dangling `.` so integral values print
    // as integers, which keeps the output diff-friendly.
    let s = format!("{v:.6}");
    let trimmed = s.trim_end_matches('0').trim_end_matches('.');
    if trimmed.is_empty() || trimmed == "-" {
        "0".to_string()
    } else {
        trimmed.to_string()
    }
}
