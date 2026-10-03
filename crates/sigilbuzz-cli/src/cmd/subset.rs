//! `sigilbuzz subset`: wraps [`sigilbuzz_subset::subset`].
//!
//! The CLI accepts a list of raw glyph ids (`--gids 1,2,3` or
//! `--gids 0..=255`), a list of unicodes (`--unicodes A,B,U+1F600`),
//! and text whose characters to keep (`--text`, `--text-file`). The
//! characters are resolved to gids through the cmap, and everything is
//! merged before being fed to the subsetter so a caller can mix and
//! match. A character the font lacks is an error unless
//! `--skip-missing` is given. Defaults match `SubsetInput::default()`.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::PathBuf;

use clap::Args as ClapArgs;

use sigilbuzz::{Blob, Face};
use sigilbuzz_subset::{subset, SubsetInput};

use super::util::{parse_gid_spec, parse_unicode_list, read_font, status, CliResult};

/// Most skipped characters the status line lists by code point.
const SKIPPED_SHOWN: usize = 10;

/// Arguments for `sigilbuzz subset`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Path to the source font.
    pub font: PathBuf,
    /// Output path for the subset font.
    pub output: PathBuf,
    /// Comma-separated glyph ids or a range (`0..=255`, `0..256`, `0-255`).
    #[arg(long)]
    pub gids: Option<String>,
    /// Comma-separated unicode codepoints (`A,B,U+1F600`). Resolved
    /// against the source font's cmap.
    #[arg(long)]
    pub unicodes: Option<String>,
    /// Keep every character of this text. Each distinct character is
    /// resolved against the source font's cmap, like `--unicodes`.
    #[arg(long)]
    pub text: Option<String>,
    /// Keep every character of this UTF-8 text file, except line
    /// breaks (CR and LF) and a leading byte order mark.
    #[arg(long, value_name = "PATH")]
    pub text_file: Option<PathBuf>,
    /// Skip requested characters the font has no glyph for, and report
    /// how many, instead of failing.
    #[arg(long)]
    pub skip_missing: bool,
    /// Retain instructions / hints (default: drop).
    #[arg(long)]
    pub retain_hints: bool,
    /// Drop layout tables (`GSUB` / `GPOS` / `GDEF`). Defaults to
    /// keeping them where the closure permits.
    #[arg(long)]
    pub drop_layout: bool,
    /// Drop variable-font tables (`fvar`/`avar`/`gvar`/`HVAR`/`VVAR`).
    /// Defaults to keeping them.
    #[arg(long)]
    pub drop_variations: bool,
}

/// Runs `sigilbuzz subset`.
pub fn run(args: Args) -> CliResult {
    let bytes = read_font(&args.font)?;
    let blob = Blob::from_vec(bytes);
    let face = Face::parse(&blob, 0).map_err(|e| format!("parse face: {e:?}"))?;

    let mut gids: Vec<u16> = Vec::new();
    if let Some(spec) = &args.gids {
        gids.extend(parse_gid_spec(spec)?);
    }
    let chars = requested_chars(&args)?;
    let mut skipped: Vec<char> = Vec::new();
    if !chars.is_empty() {
        let cmap = face.cmap().map_err(|e| format!("read cmap: {e:?}"))?;
        for ch in chars {
            match cmap.glyph_id(ch) {
                Some(gid) => gids.push(gid),
                None if args.skip_missing => skipped.push(ch),
                None => {
                    return Err(format!(
                        "U+{:04X} has no cmap entry in the source font \
                         (pass --skip-missing to skip it)",
                        ch as u32
                    ));
                }
            }
        }
    }
    if !skipped.is_empty() {
        status(format_args!(
            "skipped {} character{} the font has no glyph for: {}",
            skipped.len(),
            if skipped.len() == 1 { "" } else { "s" },
            list_code_points(&skipped)
        ));
    }
    if gids.is_empty() {
        if skipped.is_empty() {
            return Err(
                "no glyphs selected: pass --gids, --unicodes, --text, and/or --text-file".into(),
            );
        }
        return Err("no glyphs selected: the font has none of the requested characters".into());
    }
    // De-dupe while preserving caller-supplied order so the resulting
    // gid_map is deterministic for an identical CLI invocation.
    gids.sort_unstable();
    gids.dedup();

    let input = SubsetInput {
        gids,
        retain_hints: args.retain_hints,
        drop_unhandled: true,
        retain_layout: !args.drop_layout,
        retain_variations: !args.drop_variations,
    };

    let out = subset(&face, &input).map_err(|e| format!("subset: {e}"))?;
    std::fs::write(&args.output, &out.bytes)
        .map_err(|e| format!("write {}: {e}", args.output.display()))?;
    status(format_args!(
        "wrote {} bytes ({} kept glyphs) to {}",
        out.bytes.len(),
        out.gid_map.len(),
        args.output.display()
    ));
    Ok(())
}

/// The distinct characters `--unicodes`, `--text` and `--text-file`
/// ask for, in code point order.
fn requested_chars(args: &Args) -> CliResult<BTreeSet<char>> {
    let mut chars = BTreeSet::new();
    if let Some(spec) = &args.unicodes {
        chars.extend(parse_unicode_list(spec)?);
    }
    if let Some(text) = &args.text {
        chars.extend(text.chars());
    }
    if let Some(path) = &args.text_file {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("could not read text file {}: {e}", path.display()))?;
        chars.extend(text_file_chars(&text));
    }
    Ok(chars)
}

/// The characters of a text file's contents that count as text: every
/// one but CR, LF, and a byte order mark at the start, which editors
/// add on their own.
fn text_file_chars(text: &str) -> impl Iterator<Item = char> + '_ {
    let text = text.strip_prefix('\u{FEFF}').unwrap_or(text);
    text.chars().filter(|&c| c != '\n' && c != '\r')
}

/// `U+XXXX` for each of the first [`SKIPPED_SHOWN`] characters, then a
/// count of the rest.
fn list_code_points(chars: &[char]) -> String {
    let mut out = String::new();
    for (i, ch) in chars.iter().take(SKIPPED_SHOWN).enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        let _ = write!(out, "U+{:04X}", u32::from(*ch));
    }
    if chars.len() > SKIPPED_SHOWN {
        let _ = write!(out, ", and {} more", chars.len() - SKIPPED_SHOWN);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{list_code_points, text_file_chars};

    #[test]
    fn text_files_lose_line_breaks_and_a_leading_bom() {
        let got: String = text_file_chars("\u{FEFF}a b\r\nc\n").collect();
        assert_eq!(got, "a bc");
        // A byte order mark past the start is a character like any other.
        let got: String = text_file_chars("x\u{FEFF}").collect();
        assert_eq!(got, "x\u{FEFF}");
    }

    #[test]
    fn long_skip_lists_are_cut() {
        assert_eq!(list_code_points(&['A', '\u{1F600}']), "U+0041, U+1F600");
        let many: Vec<char> = ('a'..='l').collect();
        let listed = list_code_points(&many);
        assert!(listed.starts_with("U+0061, U+0062"), "{listed}");
        assert!(listed.ends_with("U+006A, and 2 more"), "{listed}");
    }
}
