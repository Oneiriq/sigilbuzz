//! `sigilbuzz shape` — placeholder. Real implementation lands in the
//! follow-up commit.

use std::path::PathBuf;

use clap::Args as ClapArgs;

use super::util::CliResult;

/// Arguments for `sigilbuzz shape`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Path to the font file.
    pub font: PathBuf,
    /// Text to shape.
    pub text: String,
    /// Comma-separated feature overrides.
    #[arg(long)]
    pub features: Option<String>,
    /// Writing direction (ltr/rtl/ttb/btt).
    #[arg(long)]
    pub direction: Option<String>,
    /// Script tag (informational).
    #[arg(long)]
    pub script: Option<String>,
    /// Language tag (informational).
    #[arg(long)]
    pub language: Option<String>,
    /// Emit JSON.
    #[arg(long)]
    pub json: bool,
}

/// Runs `sigilbuzz shape`.
pub fn run(_args: Args) -> CliResult {
    Err("shape: not yet implemented in this commit".into())
}
