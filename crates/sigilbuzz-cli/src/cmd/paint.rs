//! `sigilbuzz paint`: prints the COLRv1 paint tree for a glyph.
//!
//! Output is one [`DrawCmd`] per line in
//! evaluation order, formatted with Rust's default `{:?}` for the
//! transform / paint payload. The format is intended for human
//! inspection / golden-test diffing; consumers wanting structured
//! data should drive [`sigilbuzz_paint::evaluate`] from Rust directly.

use std::path::PathBuf;

use clap::Args as ClapArgs;

use sigilbuzz::{Blob, Face};
use sigilbuzz_paint::{evaluate, DrawCmd};

use super::util::{read_font, status, with_stdout, CliResult};

/// Arguments for `sigilbuzz paint`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Path to the font file.
    pub font: PathBuf,
    /// Glyph id whose paint tree to walk.
    pub gid: u16,
}

/// Runs `sigilbuzz paint`.
pub fn run(args: Args) -> CliResult {
    let bytes = read_font(&args.font)?;
    let blob = Blob::from_vec(bytes);
    let face = Face::parse(&blob, 0).map_err(|e| format!("parse face: {e:?}"))?;

    let cmds = evaluate(&face, args.gid);
    if cmds.is_empty() {
        status(format_args!(
            "gid {} has no COLRv1 paint tree (or font carries no COLR table)",
            args.gid
        ));
        return Ok(());
    }
    with_stdout(|out| {
        for cmd in &cmds {
            match cmd {
                DrawCmd::FillGlyph {
                    gid,
                    transform,
                    paint,
                } => writeln!(
                    out,
                    "FillGlyph gid={gid} transform={transform:?} paint={paint:?}"
                )?,
                DrawCmd::PushLayer { composite_mode } => {
                    writeln!(out, "PushLayer mode={composite_mode:?}")?;
                }
                DrawCmd::PopLayer => writeln!(out, "PopLayer")?,
            }
        }
        Ok(())
    })
}
