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
//! - GSUB ligature substitution (`liga` feature, lookup type 4)
//!   runs before advance lookup so ligature glyphs contribute their
//!   own advance instead of the sum of their components.
//! - GPOS pair adjustment (`kern` feature) when a font carries it
//!   through GPOS — including lookups wrapped in Extension (type 9)
//!   containers.
//! - Legacy `kern` table (Microsoft/OpenType version 0, format 0,
//!   horizontal) as a fallback for fonts whose GPOS has no `kern`
//!   feature. Open Sans is the canonical example.
//!
//! Either feature can be suppressed by a `Feature { tag: <tag>,
//! value: 0 }` entry passed to [`shape`].
//!
//! # What is not here yet
//!
//! - Other GSUB lookup types (single, multiple, alternate,
//!   contextual).
//! - Right-to-left reordering (M4). `buffer.direction()` is consulted
//!   but the output order is always logical = visual for now.
//! - Mark attachment, cursive attachment — M4.

use alloc::vec::Vec;

use crate::buffer::{Buffer, Glyph, ShapedRun};
use crate::error::Result;
use crate::font::Font;
use crate::tables::gpos::{lookup_type as gpos_lt, PairPos};
use crate::tables::gsub::{lookup_type as gsub_lt, Ligature, Single};
use crate::tables::{Gpos, Gsub, KernTable};

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
/// Feature tags with `value: 0` disable the corresponding feature
/// for this call. Non-zero values enable a feature if the font
/// supports it; unknown tags are accepted and ignored rather than
/// returning an error.
///
/// # Errors
///
/// Returns an error if the font is missing any of the tables required
/// for basic shaping (`cmap`, `maxp`, `hhea`, `hmtx`) or if one of
/// them is malformed.
pub fn shape(font: &Font<'_>, buffer: &Buffer, features: &[Feature]) -> Result<ShapedRun> {
    let want_kern = !feature_disabled(features, *b"kern");
    let want_liga = !feature_disabled(features, *b"liga");

    let face = font.face();
    let cmap = face.cmap()?;
    let hmtx = face.hmtx()?;

    let text = buffer.text();
    if text.is_empty() {
        return Ok(ShapedRun::default());
    }

    // Step 1: codepoint → glyph id via cmap. Clusters are byte
    // offsets from the start of the text so later passes can track
    // which input characters coalesce into a single output glyph.
    let mut glyphs: Vec<Glyph> = Vec::with_capacity(text.len());
    for (cluster, ch) in text.char_indices() {
        let glyph_id = cmap.glyph_id(ch).unwrap_or(0);
        glyphs.push(Glyph {
            glyph_id: u32::from(glyph_id),
            cluster: cluster as u32,
            x_advance: 0, // filled in after substitutions settle
            y_advance: 0,
            x_offset: 0,
            y_offset: 0,
        });
    }

    // Step 2: GSUB passes. Lookups are applied per feature, in the
    // spec's order: required features, then default-on features,
    // then user-enabled features. Feature tags the user opts into
    // via `features` flow through the same machinery — they just
    // need to be carried by the font's LangSys feature list.
    let gsub = face.gsub()?;
    if let Some(ref gsub) = gsub {
        if want_liga {
            apply_gsub_feature(gsub, &mut glyphs, *b"liga");
        }
        for feat in features {
            if feat.value == 0 {
                continue;
            }
            if feat.tag == *b"liga" || feat.tag == *b"kern" {
                continue; // already handled or GPOS territory
            }
            apply_gsub_feature(gsub, &mut glyphs, feat.tag);
        }
    }

    // Step 3: hmtx advance lookup. Runs *after* GSUB so ligatures
    // receive their ligature-glyph advance, not the sum of their
    // component advances.
    for glyph in &mut glyphs {
        let id = glyph.glyph_id as u16;
        glyph.x_advance = i32::from(hmtx.advance(id).unwrap_or(0));
    }

    if want_kern {
        let gpos_kerned = match face.gpos()? {
            Some(gpos) => apply_kern(&gpos, &mut glyphs),
            None => false,
        };
        // Legacy `kern` is a fallback: if GPOS already kerned the run
        // (even with zero-delta hits), the spec says GPOS wins and
        // we do not stack a second round on top.
        if !gpos_kerned {
            if let Some(kern) = face.kern()? {
                apply_legacy_kern(&kern, &mut glyphs);
            }
        }
    }

    Ok(ShapedRun { glyphs })
}

/// Returns `true` when the feature is explicitly disabled via
/// `Feature { tag, value: 0 }` in the override list.
fn feature_disabled(features: &[Feature], tag: [u8; 4]) -> bool {
    features.iter().any(|f| f.tag == tag && f.value == 0)
}

/// Applies every GSUB lookup reachable via the named feature tag
/// to the glyph run in place. Supports lookup types:
///
/// - 1 — Single substitution (`smcp`, `vert`, `salt`, `ss01`…)
/// - 4 — Ligature substitution (`liga`, `dlig`)
///
/// Extension (type 7) wrappers are unwrapped to the inner type.
/// Unknown lookup types are silently skipped so callers can enable
/// forward-compatible features without the run erroring out.
fn apply_gsub_feature(gsub: &Gsub<'_>, glyphs: &mut Vec<Glyph>, tag: [u8; 4]) {
    if glyphs.is_empty() {
        return;
    }

    let Some(lookup_indices) = lookup_indices_for_feature(gsub, tag) else {
        return;
    };
    if lookup_indices.is_empty() {
        return;
    }

    let lookup_list = gsub.lookup_list();
    for lookup_idx in lookup_indices {
        let Some(lookup) = lookup_list.get(lookup_idx) else {
            continue;
        };
        let raw_lt = lookup.lookup_type();
        for sub_idx in 0..lookup.subtable_count() {
            let Some(bytes) = lookup.subtable_bytes(sub_idx) else {
                continue;
            };
            let (effective_lt, inner_bytes) = if raw_lt == gsub_lt::EXTENSION {
                match resolve_extension(bytes) {
                    Some((inner_type, inner)) => (inner_type, inner),
                    None => continue,
                }
            } else {
                (raw_lt, bytes)
            };

            match effective_lt {
                gsub_lt::SINGLE => {
                    let Ok(single) = Single::parse(inner_bytes) else {
                        continue;
                    };
                    apply_single_subtable(&single, glyphs);
                }
                gsub_lt::LIGATURE => {
                    let Ok(lig) = Ligature::parse(inner_bytes) else {
                        continue;
                    };
                    apply_liga_subtable(&lig, glyphs);
                }
                _ => {}
            }
        }
    }
}

/// Walks the default LangSys (DFLT → first script) and returns the
/// sorted set of lookup indices that the feature `tag` selects. A
/// return value of `None` means no usable script, `Some(empty)`
/// means the LangSys does not carry this feature.
fn lookup_indices_for_feature(gsub: &Gsub<'_>, tag: [u8; 4]) -> Option<Vec<u16>> {
    let script_list = gsub.script_list();
    let script = script_list
        .find(*b"DFLT")
        .or_else(|| script_list.iter().next().map(|(_, s)| s))?;
    let lang_sys = script.default_lang_sys()?;

    let feature_list = gsub.feature_list();
    let mut indices: Vec<u16> = Vec::new();
    for feat_idx in lang_sys.feature_indices() {
        let Some((feat_tag, feature)) = feature_list.get(feat_idx) else {
            continue;
        };
        if feat_tag != tag {
            continue;
        }
        for idx in feature.lookup_indices() {
            if !indices.contains(&idx) {
                indices.push(idx);
            }
        }
    }
    indices.sort_unstable();
    Some(indices)
}

fn apply_single_subtable(single: &Single<'_>, glyphs: &mut [Glyph]) {
    for glyph in glyphs.iter_mut() {
        let id = glyph.glyph_id as u16;
        if let Some(out) = single.apply(id) {
            glyph.glyph_id = u32::from(out);
        }
    }
}

fn apply_liga_subtable(lig: &Ligature<'_>, glyphs: &mut Vec<Glyph>) {
    let mut i = 0;
    // Work on a scratch u16 view so lookups don't re-derive ids.
    // Re-synthesised inside the loop after each substitution so the
    // window reflects the post-replacement run.
    while i < glyphs.len() {
        let window: Vec<u16> = glyphs[i..].iter().map(|g| g.glyph_id as u16).collect();
        if let Some((lig_glyph, consumed)) = lig.apply(&window) {
            // Merge the consumed range: keep the cluster of the
            // first component (the leftmost character that fed the
            // ligature), replace the glyph id, drop the tail.
            glyphs[i].glyph_id = u32::from(lig_glyph);
            glyphs.drain(i + 1..i + consumed);
            // Stay on `i` — a ligature output might itself be the
            // first component of a longer ligature further along.
        } else {
            i += 1;
        }
    }
}

/// Applies every pair-adjustment lookup reachable via the `kern`
/// feature to the current glyph run. The spec says to union lookup
/// indices across all matching features, sort ascending, and
/// evaluate in that order — which is what this does. Non-pair
/// lookup types are silently skipped; they land with later milestones.
///
/// Returns `true` when at least one GPOS pair-adjustment subtable
/// actually ran against the glyph run. Callers use the boolean to
/// decide whether to fall through to the legacy `kern` table or
/// leave the run as-is. GPOS winning — even with zero-delta hits —
/// is the spec's design, not a sigilbuzz quirk.
fn apply_kern(gpos: &Gpos<'_>, glyphs: &mut [Glyph]) -> bool {
    if glyphs.len() < 2 {
        return false;
    }

    // Locate a reasonable LangSys. Latin fonts universally carry
    // DFLT; fall back to the first script if not present. Non-Latin
    // scripts need a caller-supplied selection in a future revision.
    let script_list = gpos.script_list();
    let script = script_list
        .find(*b"DFLT")
        .or_else(|| script_list.iter().next().map(|(_, s)| s));
    let Some(script) = script else {
        return false;
    };
    let Some(lang_sys) = script.default_lang_sys() else {
        return false;
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
        return false;
    }

    let lookup_list = gpos.lookup_list();
    let mut ran_any = false;
    for lookup_idx in lookup_indices {
        let Some(lookup) = lookup_list.get(lookup_idx) else {
            continue;
        };
        let lt = lookup.lookup_type();
        if lt != gpos_lt::PAIR_ADJUSTMENT && lt != gpos_lt::EXTENSION {
            // Any other lookup type — single-adjustment, mark
            // attachment, contextual positioning — is skipped for
            // now. Later milestones plug in their handlers.
            continue;
        }
        for sub_idx in 0..lookup.subtable_count() {
            let Some(bytes) = lookup.subtable_bytes(sub_idx) else {
                continue;
            };
            let inner_bytes = if lt == gpos_lt::EXTENSION {
                match resolve_extension(bytes) {
                    Some((inner_type, inner)) if inner_type == gpos_lt::PAIR_ADJUSTMENT => inner,
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
            ran_any = true;
        }
    }
    ran_any
}

/// Applies deltas from the legacy `kern` table to the glyph run.
///
/// HarfBuzz (and therefore rustybuzz) does not apply the whole
/// delta to the left glyph — it splits it roughly in half across
/// the pair, with the bigger share landing on the left:
///
/// ```text
///   half          = delta / 2            // truncating toward zero
///   left.advance  += delta - half        // e.g. -21 when delta=-41
///   right.advance += half                // e.g. -20 when delta=-41
/// ```
///
/// sigilbuzz matches that so legacy-kerned output lines up with
/// rustybuzz byte-for-byte; the two-sided distribution also keeps
/// clustering less visible if a renderer quantises advances.
fn apply_legacy_kern(kern: &KernTable<'_>, glyphs: &mut [Glyph]) {
    if glyphs.len() < 2 {
        return;
    }
    for i in 0..glyphs.len() - 1 {
        let left = glyphs[i].glyph_id as u16;
        let right = glyphs[i + 1].glyph_id as u16;
        let delta = i32::from(kern.kern(left, right));
        if delta != 0 {
            let half = delta / 2;
            glyphs[i].x_advance += delta - half;
            glyphs[i + 1].x_advance += half;
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
