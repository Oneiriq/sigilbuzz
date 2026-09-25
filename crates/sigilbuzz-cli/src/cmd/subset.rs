//! `sigilbuzz subset`: wraps [`sigilbuzz_subset::subset`].
//!
//! The CLI accepts either a list of raw glyph ids (`--gids 1,2,3` or
//! `--gids 0..=255`) or a list of unicodes (`--unicodes A,B,U+1F600`)
//! that the cmap is consulted to resolve to gids. Both inputs are
//! merged before being fed to the subsetter so a caller can mix-and-
//! match. Defaults match `SubsetInput::default()`.

use std::path::PathBuf;

use clap::Args as ClapArgs;

use sigilbuzz::{Blob, Face};
use sigilbuzz_subset::{subset, SubsetInput};

use super::util::{parse_gid_spec, parse_unicode_list, read_font, CliResult};

/// Arguments for `sigilbuzz subset`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Path to the source font.
    pub font: PathBuf,
    /// Output path for the subset font.
    pub output: PathBuf,
    /// Comma-separated glyph ids or a range (`0..=255`, `0..256`, `0-255`).
    #[arg(long)]
    pub gids: Option<String>,
    /// Comma-separated unicode codepoints (`A,B,U+1F600`). Resolved
    /// against the source font's cmap.
    #[arg(long)]
    pub unicodes: Option<String>,
    /// Retain instructions / hints (default: drop).
    #[arg(long)]
    pub retain_hints: bool,
    /// Drop layout tables (`GSUB` / `GPOS` / `GDEF`). Defaults to
    /// keeping them where the closure permits.
    #[arg(long)]
    pub drop_layout: bool,
    /// Drop variable-font tables (`fvar`/`avar`/`gvar`/`HVAR`).
    /// Defaults to keeping them.
    #[arg(long)]
    pub drop_variations: bool,
}

/// Runs `sigilbuzz subset`.
pub fn run(args: Args) -> CliResult {
    let bytes = read_font(&args.font)?;
    let blob = Blob::from_vec(bytes);
    let face = Face::parse(&blob, 0).map_err(|e| format!("parse face: {e:?}"))?;

    let mut gids: Vec<u16> = Vec::new();
    if let Some(spec) = &args.gids {
        gids.extend(parse_gid_spec(spec)?);
    }
    if let Some(spec) = &args.unicodes {
        let chars = parse_unicode_list(spec)?;
        let cmap = face.cmap().map_err(|e| format!("read cmap: {e:?}"))?;
        for ch in chars {
            match cmap.glyph_id(ch) {
                Some(gid) => gids.push(gid),
                None => {
                    return Err(format!(
                        "U+{:04X} has no cmap entry in the source font",
                        ch as u32
                    ));
                }
            }
        }
    }
    if gids.is_empty() {
        return Err("no glyphs selected: pass --gids and/or --unicodes".into());
    }
    // De-dupe while preserving caller-supplied order so the resulting
    // gid_map is deterministic for an identical CLI invocation.
    gids.sort_unstable();
    gids.dedup();

    let input = SubsetInput {
        gids,
        retain_hints: args.retain_hints,
        drop_unhandled: true,
        retain_layout: !args.drop_layout,
        retain_variations: !args.drop_variations,
    };

    let out = subset(&face, &input).map_err(|e| format!("subset: {e}"))?;
    std::fs::write(&args.output, &out.bytes)
        .map_err(|e| format!("write {}: {e}", args.output.display()))?;
    eprintln!(
        "wrote {} bytes ({} kept glyphs) to {}",
        out.bytes.len(),
        out.gid_map.len(),
        args.output.display()
    );
    Ok(())
}
