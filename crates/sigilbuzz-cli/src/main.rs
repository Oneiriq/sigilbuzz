//! sigilbuzz-cli: command-line driver for the sigilbuzz workspace.
//!
//! This binary is the `hb-shape` equivalent for the *whole* sigilbuzz
//! stack: shaping, subsetting, COLRv1 paint evaluation, GPU/Slug
//! encoding, WOFF wrap/unwrap, PDF font emission, SVG glyph emission,
//! and font-info dumps. Every subcommand wraps a companion crate's
//! public API directly. No logic lives here that does not belong in
//! the underlying library.
//!
//! The only new external runtime dependency is `clap`, scoped to this
//! binary (see `docs/deps.md`).

#![warn(missing_docs)]
#![forbid(unsafe_op_in_unsafe_fn)]

use std::io::Write as _;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

mod cmd;

/// Top-level CLI dispatcher.
#[derive(Debug, Parser)]
#[command(
    name = "sigilbuzz",
    version,
    about = "Command-line tool for sigilbuzz: shape, subset, paint, slug, woff, pdf, svg.",
    long_about = None,
)]
struct Cli {
    /// Subcommand to run.
    #[command(subcommand)]
    command: Cmd,
}

/// Subcommands exposed by the CLI.
#[derive(Debug, Subcommand)]
enum Cmd {
    /// Shape text against a font and print the resulting glyph stream.
    Shape(cmd::shape::Args),
    /// Subset a font down to a chosen glyph or codepoint set.
    Subset(cmd::subset::Args),
    /// Print the COLRv1 paint tree for a glyph as a flat DrawCmd stream.
    Paint(cmd::paint::Args),
    /// Encode a glyph for GPU rendering via the Slug algorithm.
    Slug(cmd::slug::Args),
    /// Wrap or unwrap WOFF1 / WOFF2 envelopes.
    Woff(cmd::woff::Args),
    /// Emit PDF font fragments (Type 3 for now).
    Pdf(cmd::pdf::Args),
    /// Emit SVG for a single glyph (outline-only or COLRv1 color).
    Svg(cmd::svg::Args),
    /// Dump a summary of the font's metadata and table list.
    Info(cmd::info::Args),
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let res = match cli.command {
        Cmd::Shape(a) => cmd::shape::run(a),
        Cmd::Subset(a) => cmd::subset::run(a),
        Cmd::Paint(a) => cmd::paint::run(a),
        Cmd::Slug(a) => cmd::slug::run(a),
        Cmd::Woff(a) => cmd::woff::run(a),
        Cmd::Pdf(a) => cmd::pdf::run(a),
        Cmd::Svg(a) => cmd::svg::run(a),
        Cmd::Info(a) => cmd::info::run(a),
    };
    match res {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            // `eprintln!` would panic if stderr is closed. The exit
            // code still reports the failure in that case.
            let _ = writeln!(std::io::stderr(), "sigilbuzz: {e}");
            ExitCode::FAILURE
        }
    }
}
