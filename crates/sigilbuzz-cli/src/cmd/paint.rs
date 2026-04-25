//! `sigilbuzz paint` — placeholder.

use std::path::PathBuf;

use clap::Args as ClapArgs;

use super::util::CliResult;

/// Arguments for `sigilbuzz paint`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Path to the font file.
    pub font: PathBuf,
    /// Glyph id.
    pub gid: u16,
}

/// Runs `sigilbuzz paint`.
pub fn run(_args: Args) -> CliResult {
    Err("paint: not yet implemented in this commit".into())
}
