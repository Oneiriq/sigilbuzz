//! `sigilbuzz subset` — placeholder.

use std::path::PathBuf;

use clap::Args as ClapArgs;

use super::util::CliResult;

/// Arguments for `sigilbuzz subset`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Path to the source font.
    pub font: PathBuf,
    /// Output path.
    pub output: PathBuf,
    /// Glyph ids.
    #[arg(long)]
    pub gids: Option<String>,
    /// Unicode codepoints.
    #[arg(long)]
    pub unicodes: Option<String>,
    /// Retain hints.
    #[arg(long)]
    pub retain_hints: bool,
    /// Drop layout tables.
    #[arg(long)]
    pub drop_layout: bool,
    /// Drop variation tables.
    #[arg(long)]
    pub drop_variations: bool,
}

/// Runs `sigilbuzz subset`.
pub fn run(_args: Args) -> CliResult {
    Err("subset: not yet implemented in this commit".into())
}
