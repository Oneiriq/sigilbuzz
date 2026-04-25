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

/// Parses a comma-separated list of `u16` glyph ids. Accepts decimal
/// or `0x`-prefixed hex. Empty entries are rejected so a stray `,`
/// is caught at parse time.
pub fn parse_gid_list(s: &str) -> CliResult<Vec<u16>> {
    let mut out = Vec::new();
    for part in s.split(',') {
        let trimmed = part.trim();
        if trimmed.is_empty() {
            return Err("gid list contains an empty entry".to_string());
        }
        let v = if let Some(hex) = trimmed.strip_prefix("0x").or_else(|| trimmed.strip_prefix("0X"))
        {
            u16::from_str_radix(hex, 16)
        } else {
            trimmed.parse::<u16>()
        }
        .map_err(|e| format!("bad gid '{trimmed}': {e}"))?;
        out.push(v);
    }
    Ok(out)
}

/// Parses `--gids` allowing a single inclusive range like `0..=255`,
/// `0..256`, or `0-255` in addition to comma lists. Range bounds may
/// use the same hex prefix as the comma form.
pub fn parse_gid_spec(s: &str) -> CliResult<Vec<u16>> {
    if let Some((lo, hi)) = split_range(s) {
        let lo = parse_one_u16(lo)?;
        let hi = parse_one_u16(hi)?;
        if hi < lo {
            return Err(format!("gid range {lo}..{hi} is reversed"));
        }
        return Ok((lo..=hi).collect());
    }
    parse_gid_list(s)
}

fn split_range(s: &str) -> Option<(&str, &str)> {
    // `..=`, `..`, then `-`. The `-` form is only accepted when both
    // sides are non-empty after split — `0-255` works, `-1` does not.
    if let Some((a, b)) = s.split_once("..=") {
        return Some((a, b));
    }
    if let Some((a, b)) = s.split_once("..") {
        return Some((a, b));
    }
    if let Some((a, b)) = s.split_once('-') {
        if !a.is_empty() && !b.is_empty() {
            return Some((a, b));
        }
    }
    None
}

fn parse_one_u16(s: &str) -> CliResult<u16> {
    let trimmed = s.trim();
    if let Some(hex) = trimmed.strip_prefix("0x").or_else(|| trimmed.strip_prefix("0X")) {
        u16::from_str_radix(hex, 16).map_err(|e| format!("bad number '{trimmed}': {e}"))
    } else {
        trimmed
            .parse::<u16>()
            .map_err(|e| format!("bad number '{trimmed}': {e}"))
    }
}

/// Parses a comma-separated list of unicode codepoints. Accepts
/// `U+1234`, `0x1234`, decimal numbers, or single literal characters.
pub fn parse_unicode_list(s: &str) -> CliResult<Vec<char>> {
    let mut out = Vec::new();
    for part in s.split(',') {
        let trimmed = part.trim();
        if trimmed.is_empty() {
            return Err("unicode list contains an empty entry".to_string());
        }
        let cp = parse_one_codepoint(trimmed)?;
        let ch = char::from_u32(cp)
            .ok_or_else(|| format!("codepoint U+{cp:04X} is not a valid char"))?;
        out.push(ch);
    }
    Ok(out)
}

fn parse_one_codepoint(s: &str) -> CliResult<u32> {
    if let Some(hex) = s.strip_prefix("U+").or_else(|| s.strip_prefix("u+")) {
        return u32::from_str_radix(hex, 16).map_err(|e| format!("bad U+ literal '{s}': {e}"));
    }
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        return u32::from_str_radix(hex, 16).map_err(|e| format!("bad 0x literal '{s}': {e}"));
    }
    // Single-character literal.
    let mut chars = s.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return Ok(c as u32);
    }
    s.parse::<u32>()
        .map_err(|e| format!("bad codepoint '{s}': {e}"))
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
