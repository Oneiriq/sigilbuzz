//! Byte-level readers shared by the layout rewriters and the closure
//! walker: Coverage and ClassDef enumeration (a null ClassDef offset reads
//! as the empty ClassDef) and Extension unwrapping.

use super::MAX_GLYPH_ENTRIES;
use alloc::vec::Vec;

// === Byte-level helpers used by the rewriters and the closure walker. ===

/// The lookup type and subtable an Extension subtable (GSUB type 7,
/// GPOS type 9) wraps:
///
/// ```text
///   u16      format = 1
///   u16      extensionLookupType
///   Offset32 extensionOffset        (from the Extension subtable)
/// ```
///
/// Errors are measured from the start of `sub`.
pub(crate) fn extension_target(sub: &[u8]) -> Result<(u16, &[u8]), sigilbuzz::Error> {
    let Some(header) = sub.get(..8) else {
        return Err(sigilbuzz::Error::Truncated {
            offset: 0,
            context: "Extension subtable truncated",
        });
    };
    if header[0..2] != [0, 1] {
        return Err(sigilbuzz::Error::Malformed {
            offset: 0,
            context: "unsupported Extension subtable format",
        });
    }
    let inner_type = u16::from_be_bytes([header[2], header[3]]);
    let inner_off = u32::from_be_bytes([header[4], header[5], header[6], header[7]]);
    let inner = usize::try_from(inner_off)
        .ok()
        .and_then(|off| sub.get(off..))
        .ok_or(sigilbuzz::Error::Malformed {
            offset: 4,
            context: "Extension offset past the end of the table",
        })?;
    Ok((inner_type, inner))
}

/// Reads a big-endian `u16` at `off`, or `None` past the end.
pub(crate) fn read_u16(bytes: &[u8], off: usize) -> Option<u16> {
    let chunk = bytes.get(off..)?.first_chunk::<2>()?;
    Some(u16::from_be_bytes(*chunk))
}

/// Reads a 4-byte tag at `off`, or `None` past the end.
pub(super) fn read_tag(bytes: &[u8], off: usize) -> Option<[u8; 4]> {
    bytes.get(off..)?.first_chunk::<4>().copied()
}

/// Best-effort enumeration of the glyphs covered by a Coverage table
/// given its raw bytes. Returns an empty vec on any parse failure.
///
/// Glyphs come back in table order, so position `i` in the result is
/// the coverage index a valid table assigns. The walk stops after
/// [`MAX_GLYPH_ENTRIES`] glyphs: a valid table never lists more, and
/// overlapping ranges in a malformed one could otherwise expand into
/// billions of entries.
///
/// Shared by the per-lookup-type rewriters in [`crate::gsub`] /
/// [`crate::gpos`] and the closure walker in [`crate::closure`].
pub(crate) fn parse_coverage_glyphs(bytes: &[u8]) -> Vec<u16> {
    let mut out = Vec::new();
    let (Some(format), Some(count)) = (read_u16(bytes, 0), read_u16(bytes, 2)) else {
        return out;
    };
    let count = usize::from(count);
    match format {
        1 => {
            let Some(glyphs) = bytes.get(4..4 + count * 2) else {
                return out;
            };
            out.extend(
                glyphs
                    .chunks_exact(2)
                    .map(|c| u16::from_be_bytes([c[0], c[1]])),
            );
        }
        2 => {
            let Some(records) = bytes.get(4..4 + count * 6) else {
                return out;
            };
            for rec in records.chunks_exact(6) {
                let start = u16::from_be_bytes([rec[0], rec[1]]);
                let end = u16::from_be_bytes([rec[2], rec[3]]);
                let room = MAX_GLYPH_ENTRIES.saturating_sub(out.len());
                if room == 0 {
                    break;
                }
                out.extend((start..=end).take(room));
            }
        }
        _ => {}
    }
    out
}

/// Walks a ClassDef's raw bytes to enumerate every `(gid, class)`
/// pair, skipping class-0 entries.
///
/// Pairs come back in table order. The walk stops after
/// [`MAX_GLYPH_ENTRIES`] pairs: a valid table never lists more, and
/// overlapping ranges in a malformed one could otherwise expand into
/// billions of entries.
pub(crate) fn parse_classdef_pairs_from_bytes(bytes: &[u8]) -> Vec<(u16, u16)> {
    let mut out = Vec::new();
    let Some(format) = read_u16(bytes, 0) else {
        return out;
    };
    match format {
        1 => {
            // Format 1: u16 format, u16 startGlyphID, u16 glyphCount, u16 values[count].
            let (Some(start), Some(count)) = (read_u16(bytes, 2), read_u16(bytes, 4)) else {
                return out;
            };
            let Some(values) = bytes.get(6..6 + usize::from(count) * 2) else {
                return out;
            };
            for (i, c) in values.chunks_exact(2).enumerate() {
                let class = u16::from_be_bytes([c[0], c[1]]);
                if class == 0 {
                    continue;
                }
                // `i < glyphCount <= u16::MAX`. A valid table never runs
                // past glyph 0xFFFF. A malformed one repeats that glyph.
                out.push((start.saturating_add(i as u16), class));
            }
        }
        2 => {
            // Format 2: u16 format, u16 rangeCount, RangeRecord[count]: u16 start, u16 end, u16 class.
            let Some(count) = read_u16(bytes, 2) else {
                return out;
            };
            let Some(records) = bytes.get(4..4 + usize::from(count) * 6) else {
                return out;
            };
            for r in records.chunks_exact(6) {
                let start = u16::from_be_bytes([r[0], r[1]]);
                let end = u16::from_be_bytes([r[2], r[3]]);
                let class = u16::from_be_bytes([r[4], r[5]]);
                if class == 0 {
                    continue;
                }
                let room = MAX_GLYPH_ENTRIES.saturating_sub(out.len());
                if room == 0 {
                    break;
                }
                out.extend((start..=end).take(room).map(|g| (g, class)));
            }
        }
        _ => {}
    }
    out
}
