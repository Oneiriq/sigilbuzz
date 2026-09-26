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
//!
//! `--bidi` shapes the text as a bidirectional paragraph through
//! [`sigilbuzz::BidiParagraph`]: every embedding-level run in its own
//! direction, the runs in visual order, clusters indexing the input.

use std::io::{self, Write};
use std::path::PathBuf;

use clap::Args as ClapArgs;

use sigilbuzz::{shape, BidiParagraph, Blob, Buffer, Face, Font, Glyph, Language, UnicodeScript};

use super::util::{parse_direction, parse_feature_list, read_font, with_stdout, CliResult};

/// Parses a four-letter ISO 15924 code, case-insensitively. A code
/// sigilbuzz has no shaper bucket for (including `Zyyy`, `Zinh`) gives
/// `None`, which keeps per-run script segmentation.
fn parse_script(s: &str) -> CliResult<Option<UnicodeScript>> {
    let tag: [u8; 4] = s
        .as_bytes()
        .try_into()
        .ok()
        .filter(|t: &[u8; 4]| t.iter().all(u8::is_ascii_alphabetic))
        .ok_or_else(|| format!("invalid script '{s}' (expected a 4-letter ISO 15924 code)"))?;
    Ok(UnicodeScript::from_iso15924_tag(tag))
}

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
    /// Writing direction (`ltr`, `rtl`, `ttb`, `btt`). `rtl` and `btt`
    /// print the glyphs in visual order (reversed), like `hb-shape`.
    /// With `--bidi`, the paragraph direction (`ltr` or `rtl`) instead.
    #[arg(long)]
    pub direction: Option<String>,
    /// Shape the text as a bidirectional paragraph: run the Unicode
    /// bidi algorithm, shape each embedding-level run in logical order
    /// in its own direction, and print the runs in visual order.
    /// Clusters stay byte offsets into the text.
    #[arg(long)]
    pub bidi: bool,
    /// ISO 15924 script code (e.g. `Arab`, `deva`) to shape the whole
    /// text as, like `hb-shape --script`. Without it, or for a script
    /// sigilbuzz has no shaper for, the text is split into script runs.
    #[arg(long)]
    pub script: Option<String>,
    /// BCP 47 language tag (e.g. `tr`, `sr-Cyrl`) that selects the
    /// font's OpenType language system, like `hb-shape --language`.
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

    let direction = args.direction.as_deref().map(parse_direction).transpose()?;
    let mut buffer = Buffer::new();
    if let Some(s) = &args.script {
        buffer.set_script(parse_script(s)?);
    }
    if let Some(l) = &args.language {
        let language = Language::new(l).ok_or_else(|| format!("invalid language tag '{l}'"))?;
        buffer.set_language(Some(language));
    }

    let features = match args.features.as_deref() {
        Some(s) => parse_feature_list(s)?,
        None => Vec::new(),
    };

    let run = if args.bidi {
        if direction.is_some_and(|d| !d.is_horizontal()) {
            return Err("--bidi takes a paragraph direction of ltr or rtl".into());
        }
        BidiParagraph::new(&args.text, direction).shape(&font, &buffer, &features)
    } else {
        buffer.set_text(&args.text);
        if let Some(d) = direction {
            buffer.set_direction(d);
        }
        shape(&font, &buffer, &features)
    }
    .map_err(|e| format!("shape: {e:?}"))?;

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
