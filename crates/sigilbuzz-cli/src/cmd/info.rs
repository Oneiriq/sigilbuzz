//! `sigilbuzz info` — placeholder.

use std::path::PathBuf;

use clap::Args as ClapArgs;

use super::util::CliResult;

/// Arguments for `sigilbuzz info`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Path to the font file.
    pub font: PathBuf,
}

/// Runs `sigilbuzz info`.
pub fn run(_args: Args) -> CliResult {
    Err("info: not yet implemented in this commit".into())
}
