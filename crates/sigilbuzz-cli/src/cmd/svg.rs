//! `sigilbuzz svg` — placeholder.

use std::path::PathBuf;

use clap::Args as ClapArgs;

use super::util::CliResult;

/// Arguments for `sigilbuzz svg`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Path to the font file.
    pub font: PathBuf,
    /// Glyph id.
    pub gid: u16,
    /// Output path.
    pub output: PathBuf,
    /// COLRv1 colour mode.
    #[arg(long)]
    pub color: bool,
}

/// Runs `sigilbuzz svg`.
pub fn run(_args: Args) -> CliResult {
    Err("svg: not yet implemented in this commit".into())
}
