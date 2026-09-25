//! Shared helpers used by more than one subcommand.
//!
//! Helpers grow alongside the subcommand that first needs them so the
//! scaffold commit stays small. Once a helper is referenced by more
//! than one subcommand it lives here.

use std::fmt::Write as _;
use std::path::Path;

/// CLI-level error alias. Concrete errors are stringified at the
/// subcommand boundary so the dispatcher only has to print them.
pub type CliResult<T = ()> = Result<T, String>;

/// Renders a 4-byte tag as ASCII, escaping non-printable bytes.
pub fn tag_to_string(tag: [u8; 4]) -> String {
    let mut out = String::with_capacity(4);
    for b in tag {
        if (0x20..=0x7E).contains(&b) {
            out.push(b as char);
        } else {
            let _ = write!(out, "\\x{b:02X}");
        }
    }
    out
}

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
        let v = if let Some(hex) = trimmed
            .strip_prefix("0x")
            .or_else(|| trimmed.strip_prefix("0X"))
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

/// Parses `--gids` allowing a single range like `0..=255`,
/// `0..256`, or `0-255` in addition to comma lists. Range bounds may
/// use the same hex prefix as the comma form.
///
/// The three range forms follow Rust convention: `..=` and `-` are
/// inclusive, `..` is exclusive (Rust's `Range::end` is exclusive
/// too). All three produce identical glyph sets when expressed
/// equivalently, e.g. `0..=255`, `0..256`, and `0-255` all expand
/// to gids 0..=255.
pub fn parse_gid_spec(s: &str) -> CliResult<Vec<u16>> {
    if let Some((lo, hi, inclusive)) = split_range(s) {
        let lo = parse_one_u16(lo)?;
        let hi = parse_one_u16(hi)?;
        if inclusive {
            if hi < lo {
                return Err(format!("gid range {lo}..={hi} is reversed"));
            }
            return Ok((lo..=hi).collect());
        }
        // Exclusive: hi == lo means an empty range. hi < lo is
        // reversed (and would otherwise underflow when we subtract
        // one to find the inclusive upper bound).
        if hi < lo {
            return Err(format!("gid range {lo}..{hi} is reversed"));
        }
        return Ok((lo..hi).collect());
    }
    parse_gid_list(s)
}

/// Returns `(lo, hi, inclusive)` for a single range spec, or `None`
/// if `s` is not in range form. `inclusive == false` matches Rust's
/// half-open `..` (hi excluded); `inclusive == true` matches `..=`
/// and the dash form.
fn split_range(s: &str) -> Option<(&str, &str, bool)> {
    // `..=` first because `..` is a prefix; `-` last so a leading
    // `-` (which would be an invalid u16 anyway) doesn't masquerade
    // as a range delimiter.
    if let Some((a, b)) = s.split_once("..=") {
        return Some((a, b, true));
    }
    if let Some((a, b)) = s.split_once("..") {
        return Some((a, b, false));
    }
    if let Some((a, b)) = s.split_once('-') {
        if !a.is_empty() && !b.is_empty() {
            return Some((a, b, true));
        }
    }
    None
}

fn parse_one_u16(s: &str) -> CliResult<u16> {
    let trimmed = s.trim();
    if let Some(hex) = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gid_spec_inclusive_range_includes_upper_bound() {
        assert_eq!(parse_gid_spec("0..=3").unwrap(), vec![0, 1, 2, 3]);
        assert_eq!(parse_gid_spec("10..=10").unwrap(), vec![10]);
    }

    #[test]
    fn gid_spec_dash_range_is_inclusive() {
        // `0-255` is the established CLI shorthand and matches `..=`.
        assert_eq!(
            parse_gid_spec("0-3").unwrap(),
            vec![0, 1, 2, 3],
            "dash form must include the upper bound to match the documented \
             equivalence with `0..=3`"
        );
    }

    #[test]
    fn gid_spec_exclusive_range_excludes_upper_bound() {
        // The documented equivalence is "0..=255 == 0..256 == 0-255".
        // Treating `..` as inclusive (the pre-fix behavior) made
        // `0..256` produce 257 gids (one off) and would silently
        // pass gid 256 to the subsetter for fonts where 255 was
        // intended to be the last kept gid.
        assert_eq!(parse_gid_spec("0..4").unwrap(), vec![0, 1, 2, 3]);
        assert_eq!(
            parse_gid_spec("5..5").unwrap(),
            Vec::<u16>::new(),
            "Rust's `..` is half-open: `5..5` is empty"
        );
    }

    #[test]
    fn gid_spec_three_forms_are_equivalent() {
        // The doc-comment promises `0..=N`, `0..(N+1)`, and `0-N`
        // all denote the same glyph set.
        let inclusive = parse_gid_spec("0..=255").unwrap();
        let exclusive = parse_gid_spec("0..256").unwrap();
        let dashed = parse_gid_spec("0-255").unwrap();
        assert_eq!(inclusive, exclusive);
        assert_eq!(inclusive, dashed);
        assert_eq!(inclusive.len(), 256);
    }

    #[test]
    fn gid_spec_reversed_range_errors() {
        assert!(parse_gid_spec("5..3").is_err());
        assert!(parse_gid_spec("5..=3").is_err());
        assert!(parse_gid_spec("5-3").is_err());
    }

    #[test]
    fn gid_spec_falls_back_to_comma_list() {
        assert_eq!(parse_gid_spec("1,2,5").unwrap(), vec![1, 2, 5]);
        assert_eq!(parse_gid_spec("0xFF,0x10").unwrap(), vec![255, 16]);
    }
}
