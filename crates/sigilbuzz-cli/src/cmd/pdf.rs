//! `sigilbuzz pdf` — placeholder.

use std::path::PathBuf;

use clap::{Args as ClapArgs, Subcommand};

use super::util::CliResult;

/// Arguments for `sigilbuzz pdf`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Sub-flavour.
    #[command(subcommand)]
    pub op: Op,
}

/// PDF emitter sub-flavours.
#[derive(Debug, Subcommand)]
pub enum Op {
    /// Emit a Type 3 font.
    Type3 {
        /// Path to the source font.
        font: PathBuf,
        /// Output path.
        output: PathBuf,
        /// Gid set.
        #[arg(long)]
        gids: String,
    },
}

/// Runs `sigilbuzz pdf`.
pub fn run(_args: Args) -> CliResult {
    Err("pdf: not yet implemented in this commit".into())
}
