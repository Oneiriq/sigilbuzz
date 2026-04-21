//! The public shaping entry point.
//!
//! # Pipeline
//!
//! ```text
//!   buffer.text  →  split into chars (cluster = UTF-8 byte offset)
//!                →  cmap.glyph_id(ch)  (falls back to .notdef when missing)
//!                →  hmtx.advance(gid)  (advance in font design units)
//!                →  Glyph { glyph_id, cluster, x_advance, ... }
//! ```
//!
//! Advances are emitted in the font's design units (i.e. the grid
//! defined by `head.unitsPerEm`). Callers that want pixels can scale
//! by `font.size() / font.units_per_em()` at render time. Keeping the
//! shaper output in design units matches `rustybuzz`'s default and
//! preserves determinism — every intermediate value is an integer.
//!
//! # What is here
//!
//! - cmap + hmtx: every character becomes a glyph with a design-unit
//!   advance.
//! - GPOS pair adjustment (`kern` feature) when a font carries it
//!   through GPOS — including lookups wrapped in Extension (type 9)
//!   containers. Disable via a `Feature { tag: b"kern", value: 0 }`
//!   entry.
//!
//! # What is not here yet
//!
//! - GSUB feature evaluation (ligatures, contextual alternates).
//! - Legacy `kern` table — many older fonts (Open Sans among them)
//!   carry their kerning in the pre-OpenType `kern` table rather
//!   than in GPOS. The shaper ignores it today; that is the next
//!   task in M2.
//! - Right-to-left reordering (M4). `buffer.direction()` is consulted
//!   but the output order is always logical = visual for now.
//! - Mark attachment, cursive attachment — M4.

use alloc::vec::Vec;

use crate::buffer::{Buffer, Glyph, ShapedRun};
use crate::error::Result;
use crate::font::Font;
use crate::tables::gpos::{lookup_type, PairPos};
use crate::tables::Gpos;

/// One entry in a feature list passed to [`shape`]. The tag is a
/// four-byte OpenType feature tag (e.g. `b"liga"`, `b"kern"`, `b"smcp"`);
/// the value is interpreted per-feature — typically `0` disables and
/// any non-zero value enables.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Feature {
    /// Four-byte feature tag.
    pub tag: [u8; 4],
    /// Feature value. Zero means off; non-zero means on (or a
    /// feature-specific selector for alternates).
    pub value: u32,
}

/// Shapes `buffer` against `font` with optional feature overrides.
///
/// Feature tags passed here are accepted for forward compatibility
/// but currently ignored — GSUB / GPOS evaluation lands in M2.
///
/// # Errors
///
/// Returns an error if the font is missing any of the tables required
/// for basic shaping (`cmap`, `head`, `maxp`, `hhea`, `hmtx`) or if
/// one of them is malformed.
pub fn shape(font: &Font<'_>, buffer: &Buffer, features: &[Feature]) -> Result<ShapedRun> {
    // GSUB feature overrides still land in M2 work. For now we
    // consume `features` only to drive the kern/no-kern choice in
    // the GPOS pass below.
    let want_kern = !features.iter().any(|f| f.tag == *b"kern" && f.value == 0);

    let face = font.face();
    let cmap = face.cmap()?;
    let hmtx = face.hmtx()?;

    let text = buffer.text();
    if text.is_empty() {
        return Ok(ShapedRun::default());
    }

    let mut glyphs = Vec::with_capacity(text.len());
    for (cluster, ch) in text.char_indices() {
        // Miss-to-notdef fallback: callers that want to surface
        // tofu handle the zero glyph themselves in their renderer;
        // shapers conventionally fall through to .notdef (id 0).
        let glyph_id = cmap.glyph_id(ch).unwrap_or(0);
        let advance = hmtx.advance(glyph_id).unwrap_or(0);

        glyphs.push(Glyph {
            glyph_id: u32::from(glyph_id),
            cluster: cluster as u32,
            x_advance: i32::from(advance),
            y_advance: 0,
            x_offset: 0,
            y_offset: 0,
        });
    }

    if want_kern {
        if let Some(gpos) = face.gpos()? {
            apply_kern(&gpos, &mut glyphs);
        }
    }

    Ok(ShapedRun { glyphs })
}

/// Applies every pair-adjustment lookup reachable via the `kern`
/// feature to the current glyph run. The spec says to union lookup
/// indices across all matching features, sort ascending, and
/// evaluate in that order — which is what this does. Non-pair
/// lookup types are silently skipped; they land with later milestones.
fn apply_kern(gpos: &Gpos<'_>, glyphs: &mut [Glyph]) {
    if glyphs.len() < 2 {
        return;
    }

    // Locate a reasonable LangSys. Latin fonts universally carry
    // DFLT; fall back to the first script if not present. Non-Latin
    // scripts need a caller-supplied selection in a future revision.
    let script_list = gpos.script_list();
    let script = script_list
        .find(*b"DFLT")
        .or_else(|| script_list.iter().next().map(|(_, s)| s));
    let Some(script) = script else {
        return;
    };
    let Some(lang_sys) = script.default_lang_sys() else {
        return;
    };

    // Collect lookup indices from every `kern` feature the LangSys
    // exposes. A font may have more than one.
    let feature_list = gpos.feature_list();
    let mut lookup_indices: Vec<u16> = Vec::new();
    for feat_idx in lang_sys.feature_indices() {
        let Some((tag, feature)) = feature_list.get(feat_idx) else {
            continue;
        };
        if tag != *b"kern" {
            continue;
        }
        for idx in feature.lookup_indices() {
            if !lookup_indices.contains(&idx) {
                lookup_indices.push(idx);
            }
        }
    }
    lookup_indices.sort_unstable();

    if lookup_indices.is_empty() {
        return;
    }

    let lookup_list = gpos.lookup_list();
    for lookup_idx in lookup_indices {
        let Some(lookup) = lookup_list.get(lookup_idx) else {
            continue;
        };
        let lt = lookup.lookup_type();
        if lt != lookup_type::PAIR_ADJUSTMENT && lt != lookup_type::EXTENSION {
            // Any other lookup type — single-adjustment, mark
            // attachment, contextual positioning — is skipped for
            // now. Later milestones plug in their handlers.
            continue;
        }
        for sub_idx in 0..lookup.subtable_count() {
            let Some(bytes) = lookup.subtable_bytes(sub_idx) else {
                continue;
            };
            let inner_bytes = if lt == lookup_type::EXTENSION {
                match resolve_extension(bytes) {
                    Some((inner_type, inner)) if inner_type == lookup_type::PAIR_ADJUSTMENT => {
                        inner
                    }
                    // Extension pointing at a non-pair lookup — skip.
                    _ => continue,
                }
            } else {
                bytes
            };
            let Ok(pp) = PairPos::parse(inner_bytes) else {
                continue;
            };
            apply_pair_pos(&pp, glyphs);
        }
    }
}

/// Resolves a GPOS/GSUB type-9 Extension subtable to its inner
/// lookup type and its inner byte slice. Layout:
///
/// ```text
///   u16  posFormat         (must be 1)
///   u16  extensionLookupType
///   u32  extensionOffset   (relative to the Extension subtable)
/// ```
///
/// The inner offset is u32 — that is why Extension exists, to reach
/// past the 64k limit a plain Offset16 imposes.
fn resolve_extension(bytes: &[u8]) -> Option<(u16, &[u8])> {
    if bytes.len() < 8 {
        return None;
    }
    let format = u16::from_be_bytes([bytes[0], bytes[1]]);
    if format != 1 {
        return None;
    }
    let inner_type = u16::from_be_bytes([bytes[2], bytes[3]]);
    let inner_off = u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
    bytes.get(inner_off..).map(|inner| (inner_type, inner))
}

fn apply_pair_pos(pp: &PairPos<'_>, glyphs: &mut [Glyph]) {
    for i in 0..glyphs.len().saturating_sub(1) {
        let first = glyphs[i].glyph_id as u16;
        let second = glyphs[i + 1].glyph_id as u16;
        if let Some((v1, v2)) = pp.lookup(first, second) {
            glyphs[i].x_advance += i32::from(v1.x_advance);
            glyphs[i].x_offset += i32::from(v1.x_placement);
            glyphs[i].y_offset += i32::from(v1.y_placement);
            glyphs[i + 1].x_advance += i32::from(v2.x_advance);
            glyphs[i + 1].x_offset += i32::from(v2.x_placement);
            glyphs[i + 1].y_offset += i32::from(v2.y_placement);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blob::Blob;
    use crate::buffer::Buffer;
    use crate::face::Face;
    use crate::font::Font;
    use crate::tables::cmap::{build_cmap_wrapper, build_format4};
    use alloc::vec::Vec;

    /// Minimal font with head / maxp / hhea / hmtx / cmap sufficient
    /// for `shape()` to run against real ASCII text. Glyph 0 is
    /// `.notdef` (advance 0); glyph 1 is 'A' (advance 500); glyph 2
    /// is 'B' (advance 600); glyph 3 is 'C' (advance 700).
    fn build_shapeable_font() -> Vec<u8> {
        // head table.
        let mut head = Vec::new();
        head.extend_from_slice(&1u16.to_be_bytes()); // majorVersion
        head.extend_from_slice(&0u16.to_be_bytes()); // minorVersion
        head.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // fontRevision
        head.extend_from_slice(&0u32.to_be_bytes()); // checksumAdjustment
        head.extend_from_slice(&0x5F0F_3CF5u32.to_be_bytes()); // magic
        head.extend_from_slice(&0u16.to_be_bytes()); // flags
        head.extend_from_slice(&1000u16.to_be_bytes()); // unitsPerEm
        head.extend_from_slice(&[0; 8 + 8 + 8 + 2 + 2 + 2]); // dates + bboxes + macStyle + ppem + hint
        head.extend_from_slice(&0i16.to_be_bytes()); // indexToLocFormat
        head.extend_from_slice(&0i16.to_be_bytes()); // glyphDataFormat

        // maxp 0.5 — 4 glyphs.
        let mut maxp = Vec::new();
        maxp.extend_from_slice(&0x0000_5000u32.to_be_bytes());
        maxp.extend_from_slice(&4u16.to_be_bytes());

        // hhea — numberOfHMetrics = 4.
        let mut hhea = Vec::new();
        hhea.extend_from_slice(&1u16.to_be_bytes()); // majorVersion
        hhea.extend_from_slice(&0u16.to_be_bytes()); // minorVersion
        hhea.extend_from_slice(&800i16.to_be_bytes()); // ascent
        hhea.extend_from_slice(&(-200i16).to_be_bytes()); // descent
        hhea.extend_from_slice(&0i16.to_be_bytes()); // lineGap
        hhea.extend_from_slice(&[0; 14]); // advanceWidthMax + six more
        hhea.extend_from_slice(&[0; 8]); // four reserved
        hhea.extend_from_slice(&0i16.to_be_bytes()); // metricDataFormat
        hhea.extend_from_slice(&4u16.to_be_bytes()); // numberOfHMetrics

        // hmtx — (advance, lsb) x 4.
        let mut hmtx = Vec::new();
        for (adv, lsb) in &[(0u16, 0i16), (500, 0), (600, 0), (700, 0)] {
            hmtx.extend_from_slice(&adv.to_be_bytes());
            hmtx.extend_from_slice(&lsb.to_be_bytes());
        }

        // cmap — format 4 mapping 'A'..='C' to glyphs 1..=3.
        // idDelta = -64 gives: 'A' (0x41) -> 1, 'B' -> 2, 'C' -> 3.
        let cmap_sub = build_format4(&[(b'A' as u16, b'C' as u16, -64)]);
        let cmap = build_cmap_wrapper(&[(3, 1, cmap_sub)]);

        // Now assemble the SFNT directory with all five tables.
        let tables: Vec<([u8; 4], Vec<u8>)> = alloc::vec![
            (*b"cmap", cmap),
            (*b"head", head),
            (*b"hhea", hhea),
            (*b"hmtx", hmtx),
            (*b"maxp", maxp),
        ];
        assemble_sfnt(&tables)
    }

    fn assemble_sfnt(tables: &[([u8; 4], Vec<u8>)]) -> Vec<u8> {
        let header_len = 12 + tables.len() * 16;
        let mut body_offset = header_len;
        let mut offsets = Vec::with_capacity(tables.len());
        for (_tag, body) in tables {
            offsets.push(body_offset);
            body_offset += body.len();
        }

        let mut out = Vec::new();
        out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        out.extend_from_slice(&(tables.len() as u16).to_be_bytes());
        out.extend_from_slice(&[0; 6]);

        for ((tag, body), off) in tables.iter().zip(offsets.iter()) {
            out.extend_from_slice(tag);
            out.extend_from_slice(&0u32.to_be_bytes()); // checksum
            out.extend_from_slice(&(*off as u32).to_be_bytes());
            out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        }
        for (_, body) in tables {
            out.extend_from_slice(body);
        }
        out
    }

    #[test]
    fn shape_empty_text_returns_no_glyphs() {
        let data = build_shapeable_font();
        let blob = Blob::new(&data);
        let face = Face::parse(&blob, 0).unwrap();
        let font = Font::new(face, 16.0);
        let buffer = Buffer::new();

        let shaped = shape(&font, &buffer, &[]).unwrap();
        assert!(shaped.is_empty());
    }

    #[test]
    fn shape_maps_chars_to_glyph_ids_and_advances() {
        let data = build_shapeable_font();
        let blob = Blob::new(&data);
        let face = Face::parse(&blob, 0).unwrap();
        let font = Font::new(face, 16.0);
        let mut buffer = Buffer::new();
        buffer.push_str("AB");

        let shaped = shape(&font, &buffer, &[]).unwrap();
        assert_eq!(shaped.len(), 2);
        assert_eq!(shaped.glyphs[0].glyph_id, 1); // 'A'
        assert_eq!(shaped.glyphs[0].x_advance, 500);
        assert_eq!(shaped.glyphs[0].cluster, 0);
        assert_eq!(shaped.glyphs[1].glyph_id, 2); // 'B'
        assert_eq!(shaped.glyphs[1].x_advance, 600);
        assert_eq!(shaped.glyphs[1].cluster, 1);
    }

    #[test]
    fn unmappable_chars_fall_back_to_notdef_with_zero_advance() {
        let data = build_shapeable_font();
        let blob = Blob::new(&data);
        let face = Face::parse(&blob, 0).unwrap();
        let font = Font::new(face, 16.0);
        let mut buffer = Buffer::new();
        // 'Z' is not in the font's cmap.
        buffer.push_str("AZ");

        let shaped = shape(&font, &buffer, &[]).unwrap();
        assert_eq!(shaped.glyphs[0].glyph_id, 1);
        assert_eq!(shaped.glyphs[0].x_advance, 500);
        assert_eq!(shaped.glyphs[1].glyph_id, 0); // .notdef
        assert_eq!(shaped.glyphs[1].x_advance, 0);
    }

    #[test]
    fn clusters_are_utf8_byte_offsets() {
        let data = build_shapeable_font();
        let blob = Blob::new(&data);
        let face = Face::parse(&blob, 0).unwrap();
        let font = Font::new(face, 16.0);
        let mut buffer = Buffer::new();
        // 'é' is two bytes in UTF-8, so the second glyph's cluster
        // skips from 0 past the two-byte character.
        buffer.push_str("éA");

        let shaped = shape(&font, &buffer, &[]).unwrap();
        assert_eq!(shaped.len(), 2);
        assert_eq!(shaped.glyphs[0].cluster, 0);
        assert_eq!(shaped.glyphs[1].cluster, 2);
    }

    #[test]
    fn feature_slice_is_accepted_but_ignored_today() {
        let data = build_shapeable_font();
        let blob = Blob::new(&data);
        let face = Face::parse(&blob, 0).unwrap();
        let font = Font::new(face, 16.0);
        let mut buffer = Buffer::new();
        buffer.push_str("A");

        // Valid feature tag, non-zero value. Should parse fine and
        // not affect the output until M2.
        let features = [Feature {
            tag: *b"liga",
            value: 1,
        }];
        let shaped = shape(&font, &buffer, &features).unwrap();
        assert_eq!(shaped.len(), 1);
        assert_eq!(shaped.glyphs[0].glyph_id, 1);
    }

    #[test]
    fn resolve_extension_decodes_inner_offset() {
        // format=1, inner_type=2, inner_off=8, then payload "inner".
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1u16.to_be_bytes());
        bytes.extend_from_slice(&2u16.to_be_bytes());
        bytes.extend_from_slice(&8u32.to_be_bytes());
        bytes.extend_from_slice(b"inner");
        let (inner_type, slice) = resolve_extension(&bytes).unwrap();
        assert_eq!(inner_type, 2);
        assert_eq!(&slice[..5], b"inner");
    }

    #[test]
    fn resolve_extension_rejects_bad_format() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&9u16.to_be_bytes());
        bytes.extend_from_slice(&2u16.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());
        assert!(resolve_extension(&bytes).is_none());
    }

    #[test]
    fn resolve_extension_rejects_short_header() {
        let bytes = [0u8; 4];
        assert!(resolve_extension(&bytes).is_none());
    }

    #[test]
    fn resolve_extension_rejects_offset_past_end() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1u16.to_be_bytes());
        bytes.extend_from_slice(&2u16.to_be_bytes());
        bytes.extend_from_slice(&9999u32.to_be_bytes());
        assert!(resolve_extension(&bytes).is_none());
    }
}
