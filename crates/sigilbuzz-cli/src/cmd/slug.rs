//! `sigilbuzz slug` — placeholder.

use std::path::PathBuf;

use clap::Args as ClapArgs;

use super::util::CliResult;

/// Arguments for `sigilbuzz slug`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Path to the font file.
    pub font: PathBuf,
    /// Glyph id.
    pub gid: u16,
    /// Override band count.
    #[arg(long)]
    pub bands: Option<u32>,
    /// Cubic-flattening tolerance.
    #[arg(long)]
    pub cubic_tolerance: Option<f32>,
}

/// Runs `sigilbuzz slug`.
pub fn run(_args: Args) -> CliResult {
    Err("slug: not yet implemented in this commit".into())
}
