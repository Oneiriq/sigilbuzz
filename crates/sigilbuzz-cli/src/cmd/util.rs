//! Shared helpers used by more than one subcommand.
//!
//! Helpers grow alongside the subcommand that first needs them so the
//! scaffold commit stays small. Once a helper is referenced by more
//! than one subcommand it lives here.

use std::path::Path;

/// CLI-level error alias. Concrete errors are stringified at the
/// subcommand boundary so the dispatcher only has to print them.
pub type CliResult<T = ()> = Result<T, String>;

/// Loads a font file from disk into memory. Reports a friendly error
/// if the path can't be read.
pub fn read_font(path: &Path) -> CliResult<Vec<u8>> {
    std::fs::read(path).map_err(|e| format!("could not read font {}: {e}", path.display()))
}

/// Parses a `--features` argument: comma-separated `TAG[=VALUE]`
/// entries. `TAG` is exactly four ASCII bytes; `VALUE` defaults to
/// `1` (enable) when omitted, and `0` disables. A leading `-` on
/// the tag also disables (mirroring HarfBuzz syntax).
pub fn parse_feature_list(s: &str) -> CliResult<Vec<sigilbuzz::Feature>> {
    let mut out = Vec::new();
    for raw in s.split(',') {
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }
        let (negated, body) = if let Some(rest) = raw.strip_prefix('-') {
            (true, rest)
        } else if let Some(rest) = raw.strip_prefix('+') {
            (false, rest)
        } else {
            (false, raw)
        };
        let (tag_str, value) = match body.split_once('=') {
            Some((t, v)) => (
                t,
                v.parse::<u32>()
                    .map_err(|e| format!("bad feature value in '{raw}': {e}"))?,
            ),
            None => (body, if negated { 0 } else { 1 }),
        };
        if tag_str.len() != 4 || !tag_str.is_ascii() {
            return Err(format!(
                "feature tag '{tag_str}' must be exactly 4 ASCII bytes"
            ));
        }
        let mut tag = [b' '; 4];
        for (i, b) in tag_str.bytes().enumerate() {
            tag[i] = b;
        }
        let value = if negated { 0 } else { value };
        out.push(sigilbuzz::Feature { tag, value });
    }
    Ok(out)
}

/// Parses a writing direction string.
pub fn parse_direction(s: &str) -> CliResult<sigilbuzz::Direction> {
    match s.to_ascii_lowercase().as_str() {
        "ltr" => Ok(sigilbuzz::Direction::Ltr),
        "rtl" => Ok(sigilbuzz::Direction::Rtl),
        "ttb" => Ok(sigilbuzz::Direction::Ttb),
        "btt" => Ok(sigilbuzz::Direction::Btt),
        other => Err(format!(
            "unknown direction '{other}' (expected ltr/rtl/ttb/btt)"
        )),
    }
}
