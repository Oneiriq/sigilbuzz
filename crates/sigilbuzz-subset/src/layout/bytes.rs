//! Byte-level readers shared by the layout rewriters and the closure
//! walker: Coverage and ClassDef enumeration (a null ClassDef offset reads
//! as the empty ClassDef) and Extension unwrapping.

use alloc::vec::Vec;

// === Byte-level helpers used by the rewriters and the closure walker. ===

/// The `(gid, class)` pairs of the ClassDef that `offset` points at
/// inside `sub`, class 0 left out. A null offset is the spec's empty
/// ClassDef (every glyph in class 0; fontmake leaves the backtrack
/// ClassDef of chained context format 2 null this way), so it yields
/// no pairs. An offset past the end of `sub` yields `None`.
pub(crate) fn classdef_pairs_at(sub: &[u8], offset: usize) -> Option<Vec<(u16, u16)>> {
    if offset == 0 {
        return Some(Vec::new());
    }
    sub.get(offset..).map(parse_classdef_pairs_from_bytes)
}

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

/// Best-effort enumeration of the glyphs covered by a Coverage table
/// given its raw bytes. Returns an empty vec on any parse failure.
///
/// Mirrors the helper in [`crate::closure`], exposed here so the
/// per-lookup-type rewriters in [`crate::gsub`] / [`crate::gpos`] can
/// share it without re-deriving the byte layout.
pub(crate) fn parse_coverage_glyphs(bytes: &[u8]) -> Vec<u16> {
    let mut out = Vec::new();
    if bytes.len() < 4 {
        return out;
    }
    let format = u16::from_be_bytes([bytes[0], bytes[1]]);
    let count = u16::from_be_bytes([bytes[2], bytes[3]]) as usize;
    match format {
        1 => {
            let need = 4 + count * 2;
            if bytes.len() < need {
                return out;
            }
            for i in 0..count {
                let off = 4 + i * 2;
                out.push(u16::from_be_bytes([bytes[off], bytes[off + 1]]));
            }
        }
        2 => {
            let need = 4 + count * 6;
            if bytes.len() < need {
                return out;
            }
            for i in 0..count {
                let off = 4 + i * 6;
                let start = u16::from_be_bytes([bytes[off], bytes[off + 1]]);
                let end = u16::from_be_bytes([bytes[off + 2], bytes[off + 3]]);
                for g in start..=end {
                    out.push(g);
                }
            }
        }
        _ => {}
    }
    out
}

/// Walks a ClassDef's raw bytes to enumerate every `(gid, class)`
/// pair, skipping class-0 entries.
pub(crate) fn parse_classdef_pairs_from_bytes(bytes: &[u8]) -> Vec<(u16, u16)> {
    let mut out = Vec::new();
    if bytes.len() < 2 {
        return out;
    }
    let format = u16::from_be_bytes([bytes[0], bytes[1]]);
    match format {
        1 => {
            // Format 1: u16 format, u16 startGlyphID, u16 glyphCount, u16 values[count].
            if bytes.len() < 6 {
                return out;
            }
            let start = u16::from_be_bytes([bytes[2], bytes[3]]);
            let count = u16::from_be_bytes([bytes[4], bytes[5]]) as usize;
            let need = 6 + count * 2;
            if bytes.len() < need {
                return out;
            }
            for i in 0..count {
                let off = 6 + i * 2;
                let class = u16::from_be_bytes([bytes[off], bytes[off + 1]]);
                if class == 0 {
                    continue;
                }
                let gid = start.saturating_add(i as u16);
                out.push((gid, class));
            }
        }
        2 => {
            // Format 2: u16 format, u16 rangeCount, RangeRecord[count]: u16 start, u16 end, u16 class.
            if bytes.len() < 4 {
                return out;
            }
            let count = u16::from_be_bytes([bytes[2], bytes[3]]) as usize;
            let need = 4 + count * 6;
            if bytes.len() < need {
                return out;
            }
            for i in 0..count {
                let off = 4 + i * 6;
                let start = u16::from_be_bytes([bytes[off], bytes[off + 1]]);
                let end = u16::from_be_bytes([bytes[off + 2], bytes[off + 3]]);
                let class = u16::from_be_bytes([bytes[off + 4], bytes[off + 5]]);
                if class == 0 {
                    continue;
                }
                for g in start..=end {
                    out.push((g, class));
                }
            }
        }
        _ => {}
    }
    out
}
