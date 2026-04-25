//! `sigilbuzz pdf` — emit PDF font fragments.
//!
//! Today we only ship the `type3` flavour — `emit_type3_font` is the
//! one that produces a self-contained, CharProc-stream-based body
//! that is meaningful to dump on its own. The Type 1 and OTF-embedded
//! emitters in `sigilbuzz-pdf` produce fragments that cross-reference
//! indirect objects, so they need a full PDF assembler to be useful;
//! exposing them here would invite footguns. The CLI surface stays
//! intentionally narrow until we have a "compose into a complete PDF"
//! story.

use std::path::PathBuf;

use clap::{Args as ClapArgs, Subcommand};

use sigilbuzz::{Blob, Face};

use super::util::{parse_gid_spec, read_font, CliResult};

/// Arguments for `sigilbuzz pdf`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Sub-flavour — only `type3` is supported today.
    #[command(subcommand)]
    pub op: Op,
}

/// PDF emitter sub-flavours.
#[derive(Debug, Subcommand)]
pub enum Op {
    /// Emit a Type 3 font's CharProc stream for the chosen gid set.
    Type3 {
        /// Path to the source font.
        font: PathBuf,
        /// Output path. Receives a plain UTF-8 dump of the font dict
        /// fragments (FontBBox, FontMatrix, CharProcs, Encoding,
        /// Widths) — the consumer assembles this into a full PDF.
        output: PathBuf,
        /// Gid set as a comma list or range (`0..=255`, `0..256`, `0-255`).
        #[arg(long)]
        gids: String,
    },
}

/// Runs `sigilbuzz pdf`.
pub fn run(args: Args) -> CliResult {
    match args.op {
        Op::Type3 { font, output, gids } => {
            let bytes = read_font(&font)?;
            let blob = Blob::from_vec(bytes);
            let face = Face::parse(&blob, 0).map_err(|e| format!("parse face: {e:?}"))?;
            let gids = parse_gid_spec(&gids)?;
            let t3 = sigilbuzz_pdf::emit_type3_font(&face, &gids);

            // Plain-text dump. Each section is labelled so the consumer
            // can split it back out programmatically; CharProc bodies
            // are emitted as raw PDF content streams.
            let mut buf = String::new();
            use std::fmt::Write as _;
            let _ = writeln!(
                buf,
                "%% sigilbuzz Type 3 font fragment\n\
                 %% FontBBox: [{xmin} {ymin} {xmax} {ymax}]\n\
                 %% FontMatrix: [{m0} {m1} {m2} {m3} {m4} {m5}]\n\
                 %% Encoding entries: {enc}\n\
                 %% CharProcs: {cp}",
                xmin = t3.bbox.xmin,
                ymin = t3.bbox.ymin,
                xmax = t3.bbox.xmax,
                ymax = t3.bbox.ymax,
                m0 = t3.matrix[0],
                m1 = t3.matrix[1],
                m2 = t3.matrix[2],
                m3 = t3.matrix[3],
                m4 = t3.matrix[4],
                m5 = t3.matrix[5],
                enc = t3.encoding.len(),
                cp = t3.char_procs.len(),
            );
            for (code, name) in &t3.encoding {
                let _ = writeln!(buf, "%% encoding[{code}] = /{name}");
            }
            for (i, cp) in t3.char_procs.iter().enumerate() {
                let _ = writeln!(
                    buf,
                    "%% CharProc[{i}] /{name} width={w} bbox=[{xmin} {ymin} {xmax} {ymax}]\n\
                     <<\n/Length {len}\n>>\nstream",
                    name = cp.name,
                    w = cp.width,
                    xmin = cp.bbox.xmin,
                    ymin = cp.bbox.ymin,
                    xmax = cp.bbox.xmax,
                    ymax = cp.bbox.ymax,
                    len = cp.body.len(),
                );
                buf.push_str(&String::from_utf8_lossy(&cp.body));
                buf.push_str("endstream\n");
            }
            std::fs::write(&output, &buf)
                .map_err(|e| format!("write {}: {e}", output.display()))?;
            eprintln!(
                "wrote {} CharProcs ({} bytes) to {}",
                t3.char_procs.len(),
                buf.len(),
                output.display()
            );
        }
    }
    Ok(())
}
