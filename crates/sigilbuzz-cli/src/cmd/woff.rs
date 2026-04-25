//! `sigilbuzz woff` — placeholder.

use std::path::PathBuf;

use clap::{Args as ClapArgs, Subcommand};

use super::util::CliResult;

/// Arguments for `sigilbuzz woff`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// `wrap` or `unwrap`.
    #[command(subcommand)]
    pub op: Op,
}

/// WOFF subcommands.
#[derive(Debug, Subcommand)]
pub enum Op {
    /// Wrap a TTF/OTF.
    Wrap {
        /// Input.
        input: PathBuf,
        /// Output.
        output: PathBuf,
        /// `woff1` or `woff2`.
        #[arg(long, default_value = "woff2")]
        format: String,
    },
    /// Unwrap a WOFF file.
    Unwrap {
        /// Input.
        input: PathBuf,
        /// Output.
        output: PathBuf,
    },
}

/// Runs `sigilbuzz woff`.
pub fn run(_args: Args) -> CliResult {
    Err("woff: not yet implemented in this commit".into())
}
