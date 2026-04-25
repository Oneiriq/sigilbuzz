//! `sigilbuzz svg` — emit a self-contained `<svg>` document for one
//! glyph.
//!
//! Wraps [`sigilbuzz_svg::glyph_to_svg`] for the outline-only path and
//! [`sigilbuzz_svg::glyph_to_svg_color`] for `--color`.

use std::path::PathBuf;

use clap::Args as ClapArgs;

use sigilbuzz::{Blob, Face};

use super::util::{read_font, CliResult};

/// Arguments for `sigilbuzz svg`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Path to the font file.
    pub font: PathBuf,
    /// Glyph id to render.
    pub gid: u16,
    /// Output SVG path.
    pub output: PathBuf,
    /// Use COLRv1 colour rendering when the font carries a paint tree
    /// for this glyph; falls back to outline-only when it does not.
    #[arg(long)]
    pub color: bool,
}

/// Runs `sigilbuzz svg`.
pub fn run(args: Args) -> CliResult {
    let bytes = read_font(&args.font)?;
    let blob = Blob::from_vec(bytes);
    let face = Face::parse(&blob, 0).map_err(|e| format!("parse face: {e:?}"))?;

    let svg = if args.color {
        sigilbuzz_svg::glyph_to_svg_color(&face, args.gid)
            .or_else(|| sigilbuzz_svg::glyph_to_svg(&face, args.gid))
    } else {
        sigilbuzz_svg::glyph_to_svg(&face, args.gid)
    };
    let svg = svg.ok_or_else(|| {
        format!(
            "gid {} has no outline (whitespace, missing, or out of range)",
            args.gid
        )
    })?;
    std::fs::write(&args.output, &svg)
        .map_err(|e| format!("write {}: {e}", args.output.display()))?;
    eprintln!("wrote {} bytes to {}", svg.len(), args.output.display());
    Ok(())
}
