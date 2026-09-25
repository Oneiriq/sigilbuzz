//! `sigilbuzz shape`: drives [`sigilbuzz::shape`] from the CLI.
//!
//! Output format mirrors `hb-shape`:
//!
//! ```text
//!   gid=NN advance=NN cluster=NN
//! ```
//!
//! one line per glyph, plus an optional `--json` flag for machine-
//! readable output. The shape pipeline always runs at the font's
//! design-unit size (size = 1.0 means we report raw advances). There
//! is no point-size knob in the CLI yet.

use std::io::{self, Write};
use std::path::PathBuf;

use clap::Args as ClapArgs;

use sigilbuzz::{shape, Blob, Buffer, Face, Font, Glyph};

use super::util::{parse_direction, parse_feature_list, read_font, with_stdout, CliResult};

/// Arguments for `sigilbuzz shape`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Path to the font file.
    pub font: PathBuf,
    /// Text to shape.
    pub text: String,
    /// Comma-separated feature overrides (e.g. `liga,-kern,smcp=1`).
    #[arg(long)]
    pub features: Option<String>,
    /// Writing direction (`ltr`, `rtl`, `ttb`, `btt`).
    #[arg(long)]
    pub direction: Option<String>,
    /// Script tag (informational; sigilbuzz auto-detects internally).
    #[arg(long)]
    pub script: Option<String>,
    /// Language tag (informational; sigilbuzz auto-detects internally).
    #[arg(long)]
    pub language: Option<String>,
    /// Emit JSON instead of one-line-per-glyph text.
    #[arg(long)]
    pub json: bool,
}

/// Runs `sigilbuzz shape`.
pub fn run(args: Args) -> CliResult {
    let bytes = read_font(&args.font)?;
    let blob = Blob::from_vec(bytes);
    let face = Face::parse(&blob, 0).map_err(|e| format!("parse face: {e:?}"))?;
    let font = Font::new(face, 1.0);

    let mut buffer = Buffer::new();
    buffer.set_text(&args.text);
    if let Some(d) = &args.direction {
        buffer.set_direction(parse_direction(d)?);
    }
    // script / language are accepted for hb-shape parity but the
    // shaping core auto-detects script per run, so we record them
    // only as informational fields in the JSON path. Storing them
    // would require a no-op Buffer setter; we intentionally do not
    // add one to the core just for CLI ergonomics.
    let _ = (&args.script, &args.language);

    let features = match args.features.as_deref() {
        Some(s) => parse_feature_list(s)?,
        None => Vec::new(),
    };

    let run = shape(&font, &buffer, &features).map_err(|e| format!("shape: {e:?}"))?;

    with_stdout(|out| {
        if args.json {
            write_json(out, &run.glyphs)
        } else {
            for g in &run.glyphs {
                writeln!(
                    out,
                    "gid={} advance={} cluster={}",
                    g.glyph_id, g.x_advance, g.cluster
                )?;
            }
            Ok(())
        }
    })
}

fn write_json(out: &mut dyn Write, glyphs: &[Glyph]) -> io::Result<()> {
    // Hand-rolled JSON emission to avoid a serde dep.
    write!(out, "[")?;
    for (i, g) in glyphs.iter().enumerate() {
        if i > 0 {
            write!(out, ",")?;
        }
        write!(
            out,
            "{{\"gid\":{},\"cluster\":{},\"x_advance\":{},\"y_advance\":{},\"x_offset\":{},\"y_offset\":{}}}",
            g.glyph_id, g.cluster, g.x_advance, g.y_advance, g.x_offset, g.y_offset
        )?;
    }
    writeln!(out, "]")
}
