//! `sigilbuzz woff`: wrap or unwrap WOFF1 / WOFF2 envelopes.
//!
//! Wraps [`sigilbuzz_woff::wrap_woff1`] / [`sigilbuzz_woff::wrap_woff2`]
//! and the matching unwrap functions. The unwrap path auto-detects
//! the input format from its 4-byte magic (`wOFF` for WOFF1, `wOF2`
//! for WOFF2) so the caller never has to declare it.

use std::path::PathBuf;

use clap::{Args as ClapArgs, Subcommand};

use super::util::{status, CliResult};

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
    /// Wrap a TTF/OTF into a WOFF1 / WOFF2 envelope.
    Wrap {
        /// Input SFNT (TTF/OTF).
        input: PathBuf,
        /// Output WOFF file.
        output: PathBuf,
        /// `woff1` or `woff2` (default: `woff2`).
        #[arg(long, default_value = "woff2")]
        format: String,
    },
    /// Unwrap a WOFF1 / WOFF2 envelope back to a TTF/OTF.
    Unwrap {
        /// Input WOFF file.
        input: PathBuf,
        /// Output SFNT path.
        output: PathBuf,
    },
}

/// Runs `sigilbuzz woff`.
pub fn run(args: Args) -> CliResult {
    match args.op {
        Op::Wrap {
            input,
            output,
            format,
        } => {
            let sfnt =
                std::fs::read(&input).map_err(|e| format!("read {}: {e}", input.display()))?;
            let wrapped =
                match format.to_ascii_lowercase().as_str() {
                    "woff1" => sigilbuzz_woff::wrap_woff1(&sfnt)
                        .map_err(|e| format!("wrap woff1: {e:?}"))?,
                    "woff2" => sigilbuzz_woff::wrap_woff2(&sfnt)
                        .map_err(|e| format!("wrap woff2: {e:?}"))?,
                    other => return Err(format!("unknown woff format '{other}'")),
                };
            std::fs::write(&output, &wrapped)
                .map_err(|e| format!("write {}: {e}", output.display()))?;
            status(format_args!(
                "wrote {} bytes to {}",
                wrapped.len(),
                output.display()
            ));
        }
        Op::Unwrap { input, output } => {
            let bytes =
                std::fs::read(&input).map_err(|e| format!("read {}: {e}", input.display()))?;
            let sfnt = match magic_of(&bytes) {
                Some(b"wOFF") => sigilbuzz_woff::unwrap_woff1(&bytes)
                    .map_err(|e| format!("unwrap woff1: {e:?}"))?,
                Some(b"wOF2") => sigilbuzz_woff::unwrap_woff2(&bytes)
                    .map_err(|e| format!("unwrap woff2: {e:?}"))?,
                Some(magic) => {
                    return Err(format!(
                        "unknown WOFF magic {:?}; expected wOFF or wOF2",
                        core::str::from_utf8(magic).unwrap_or("????")
                    ));
                }
                None => return Err("input too short to carry a WOFF magic".into()),
            };
            std::fs::write(&output, &sfnt)
                .map_err(|e| format!("write {}: {e}", output.display()))?;
            status(format_args!(
                "wrote {} bytes to {}",
                sfnt.len(),
                output.display()
            ));
        }
    }
    Ok(())
}

fn magic_of(bytes: &[u8]) -> Option<&[u8; 4]> {
    bytes.get(..4)?.try_into().ok()
}
