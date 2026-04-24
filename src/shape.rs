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
//! - cmap → glyph id, then the full shaping pipeline in spec order.
//! - GSUB (lookup types 1, 4, 6 format 3, plus Extension type 7
//!   unwrapping): `ccmp`, `rlig`, `liga`, `clig`, `calt` run by
//!   default; any user-enabled tag with non-zero value flows
//!   through the same dispatcher. Chained-context lookups can
//!   invoke other lookups at specific positions inside the match
//!   window — the first recursive layer sigilbuzz supports.
//! - hmtx advance lookup, post-substitution so ligature glyphs get
//!   their own advance rather than the sum of their components.
//! - GPOS (lookup types 1, 2, 4, 5, 6, plus Extension type 9
//!   unwrapping): `kern` runs by default for pair adjustment,
//!   `mark` runs by default for mark-to-base attachment, `mkmk`
//!   for mark-to-mark stacking, and mark-to-ligature is dispatched
//!   via the same `mark` feature when the subtable is present.
//!   User-enabled GPOS tags flow through the same dispatcher.
//! - Legacy `kern` table as a fallback for fonts whose GPOS has no
//!   `kern` feature. Open Sans is the canonical example.
//!
//! Any default-on feature can be suppressed by a `Feature { tag,
//! value: 0 }` entry.
//!
//! # What is not here yet
//!
//! - Full Unicode NFC normalisation. sigilbuzz ships the
//!   composition half of NFC (opt-in via
//!   [`crate::Buffer::set_normalize_nfc`]); canonical
//!   decomposition and combining-class reordering do not run yet,
//!   so pathological inputs that need reordering fall through
//!   unchanged.
//! - GSUB contextual non-chained (type 5), multiple substitution
//!   (type 2), alternate (type 3), reverse chained (type 8),
//!   and the format 1/2 variants of type 6.
//! - GPOS cursive attachment (type 3) and contextual (types 7, 8).
//! - Right-to-left reordering — `buffer.direction()` is consulted
//!   but the output order is always logical = visual for now.

use alloc::borrow::Cow;
use alloc::vec::Vec;

use crate::buffer::{Buffer, Glyph, ShapedRun};
use crate::error::Result;
use crate::font::Font;
use crate::ot::arabic::{assign_joining_forms, JoiningForm};
use crate::tables::gdef::{Gdef, GlyphClass};
use crate::tables::gpos::{
    lookup_type as gpos_lt, MarkBasePos, MarkLigaPos, MarkMarkPos, PairPos, SinglePos,
};
use crate::tables::gsub::{
    lookup_type as gsub_lt, Alternate, ChainContext, Ligature, Multiple, Single,
};
use crate::tables::{Gpos, Gsub, KernTable};
use crate::unicode::{script_of, Script};

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
// The pipeline is deliberately a straight-line sequence of passes so
// the order is visible in one place; breaking it into five stage
// helpers would cost more in indirection than it buys in LOC.
#[allow(clippy::too_many_lines)]
pub fn shape(font: &Font<'_>, buffer: &Buffer, features: &[Feature]) -> Result<ShapedRun> {
    let want_kern = !feature_disabled(features, *b"kern");
    let want_liga = !feature_disabled(features, *b"liga");

    let face = font.face();
    let cmap = face.cmap()?;
    let hmtx = face.hmtx()?;

    let raw_text = buffer.text();
    if raw_text.is_empty() {
        return Ok(ShapedRun::default());
    }
    // NFC composition pass runs before cmap lookup so precomposed
    // forms (é, ñ, ...) find their precomposed glyphs instead of
    // the decomposed base + combining-mark pair. Opt-in; see
    // Buffer::set_normalize_nfc.
    let text: Cow<'_, str> = if buffer.normalize_nfc() {
        Cow::Owned(crate::unicode::normalize::compose_str(raw_text))
    } else {
        Cow::Borrowed(raw_text)
    };
    let text: &str = &text;

    // Step 1: codepoint → glyph id via cmap. Clusters are byte
    // offsets from the start of the text so later passes can track
    // which input characters coalesce into a single output glyph.
    //
    // Default-ignorable Unicode format characters (ZWJ, ZWNJ, and
    // the U+200E/U+200F bidi marks) drive the joining state machine
    // but should not render — HarfBuzz replaces their glyph id with
    // U+0020 (SPACE) after joining-form selection. sigilbuzz mirrors
    // that: record the space glyph once, then swap the
    // default-ignorable glyphs below. We keep the joining-type view
    // on the original codepoints so the state machine still sees
    // ZWJ/ZWNJ correctly.
    let space_gid = u32::from(cmap.glyph_id('\u{0020}').unwrap_or(0));
    let mut glyphs: Vec<Glyph> = Vec::with_capacity(text.len());
    for (cluster, ch) in text.char_indices() {
        let glyph_id = if is_default_ignorable(ch) {
            space_gid
        } else {
            u32::from(cmap.glyph_id(ch).unwrap_or(0))
        };
        glyphs.push(Glyph {
            glyph_id,
            cluster: cluster as u32,
            x_advance: 0, // filled in after substitutions settle
            y_advance: 0,
            x_offset: 0,
            y_offset: 0,
        });
    }

    // Step 1.5: Detect Arabic and compute per-glyph joining forms.
    // When any glyph in the run comes from an Arabic codepoint the
    // joining state machine decides which of init/medi/fina/isol
    // each Arabic position takes. Non-Arabic positions get None and
    // the positional features skip them. The forms vector stays
    // aligned with `glyphs` through substitutions because ligatures
    // and multiple-sub would violate that alignment only under
    // `ccmp`/`rlig`, which we apply *after* computing forms so the
    // state machine sees the pre-substitution sequence (correct per
    // spec: joining is decided on codepoints, not glyphs).
    let has_arabic = text.chars().any(|c| script_of(c) == Script::Arabic);
    let arabic_forms: Vec<JoiningForm> = if has_arabic {
        assign_joining_forms(text)
    } else {
        Vec::new()
    };

    // Step 2: GSUB passes. Default-on features mirror HarfBuzz's
    // Latin defaults so common text renders the same way without
    // the caller having to enumerate them. Order matches the spec:
    // `ccmp` (composition/decomposition) runs before ligatures and
    // before the required-ligature fallback, because later passes
    // operate on the composed glyph stream.
    //
    // When the run contains Arabic, the positional features
    // (`isol`/`init`/`medi`/`fina`) run *before* `rlig` and `liga`
    // so the ligature subtables see the post-joining glyph ids.
    // That is the order HarfBuzz uses and it is what Arabic fonts
    // are designed against: `rlig` typically collapses a sequence
    // like (lam-medi, alef-fina) into the lam-alef ligature.
    let gsub = face.gsub()?;
    if let Some(ref gsub) = gsub {
        if !feature_disabled(features, *b"ccmp") {
            apply_gsub_feature(gsub, &mut glyphs, *b"ccmp", 0, has_arabic);
        }
        if has_arabic && !arabic_forms.is_empty() {
            apply_arabic_positional_features(gsub, &mut glyphs, &arabic_forms);
        }
        if !feature_disabled(features, *b"rlig") {
            apply_gsub_feature(gsub, &mut glyphs, *b"rlig", 0, has_arabic);
        }
        if want_liga {
            apply_gsub_feature(gsub, &mut glyphs, *b"liga", 0, has_arabic);
        }
        // `clig` (contextual ligatures) and `calt` (contextual
        // alternates) run by default for Latin — HarfBuzz's Latin
        // fallback shaper turns both on. sigilbuzz follows suit.
        if !feature_disabled(features, *b"clig") {
            apply_gsub_feature(gsub, &mut glyphs, *b"clig", 0, has_arabic);
        }
        if !feature_disabled(features, *b"calt") {
            apply_gsub_feature(gsub, &mut glyphs, *b"calt", 0, has_arabic);
        }
        for feat in features {
            if feat.value == 0 {
                continue;
            }
            if is_handled_gsub_tag(feat.tag) {
                continue; // already handled above
            }
            // feature.value is 1-indexed per the spec; subtract one to
            // get the 0-indexed alternate slot sigilbuzz's Alternate
            // parser uses. Clamped via saturating_sub so value=1 still
            // picks the first alternate.
            let alternate_idx = (feat.value.saturating_sub(1)).min(u32::from(u16::MAX)) as u16;
            apply_gsub_feature(gsub, &mut glyphs, feat.tag, alternate_idx, has_arabic);
        }
    }

    // Step 3: hmtx advance lookup. Runs *after* GSUB so ligatures
    // receive their ligature-glyph advance, not the sum of their
    // component advances. Default-ignorable format characters keep
    // a zero advance — they rendered as space earlier, but must not
    // move the pen (HarfBuzz does the same).
    //
    // The cluster-to-character map is required to re-identify
    // default-ignorable positions by codepoint after GSUB may have
    // rewritten glyph ids. We iterate `text.char_indices()` and
    // look the character up by cluster offset.
    let default_ignorable_clusters: Vec<u32> = text
        .char_indices()
        .filter(|(_, c)| is_default_ignorable(*c))
        .map(|(i, _)| i as u32)
        .collect();
    for glyph in &mut glyphs {
        if default_ignorable_clusters.contains(&glyph.cluster) {
            glyph.x_advance = 0;
            continue;
        }
        let id = glyph.glyph_id as u16;
        glyph.x_advance = i32::from(hmtx.advance(id).unwrap_or(0));
    }

    // Step 4: GPOS passes. Kern first, then mark-to-base; then any
    // user-enabled GPOS features that flow through feature overrides.
    let gdef = face.gdef()?;
    let gpos = face.gpos()?;
    let gpos_kerned = if want_kern {
        match &gpos {
            Some(gpos) => {
                apply_gpos_feature(gpos, &mut glyphs, gdef.as_ref(), *b"kern", has_arabic)
            }
            None => false,
        }
    } else {
        false
    };
    if let Some(ref gpos) = gpos {
        if !feature_disabled(features, *b"mark") {
            apply_gpos_feature(gpos, &mut glyphs, gdef.as_ref(), *b"mark", has_arabic);
        }
        if !feature_disabled(features, *b"mkmk") {
            apply_gpos_feature(gpos, &mut glyphs, gdef.as_ref(), *b"mkmk", has_arabic);
        }
        // User-enabled features beyond the defaults flow through the
        // same dispatch. Skip tags already handled above so they do
        // not double-apply.
        for feat in features {
            if feat.value == 0 {
                continue;
            }
            if matches!(&feat.tag, b"kern" | b"mark" | b"mkmk" | b"liga") {
                continue;
            }
            apply_gpos_feature(gpos, &mut glyphs, gdef.as_ref(), feat.tag, has_arabic);
        }
    }

    // Legacy `kern` is a fallback: only runs when GPOS kern produced
    // no lookups. GPOS wins even with zero-delta hits — the spec's
    // design, not a sigilbuzz quirk.
    if want_kern && !gpos_kerned {
        if let Some(kern) = face.kern()? {
            apply_legacy_kern(&kern, &mut glyphs);
        }
    }

    Ok(ShapedRun { glyphs })
}

/// Returns `true` when the feature is explicitly disabled via
/// `Feature { tag, value: 0 }` in the override list.
fn feature_disabled(features: &[Feature], tag: [u8; 4]) -> bool {
    features.iter().any(|f| f.tag == tag && f.value == 0)
}

/// True for the small set of Unicode format characters the shaper
/// should render as space rather than their own glyph — the ones
/// whose job is to influence the shaping pipeline without carrying
/// a visual form. HarfBuzz calls these "default ignorable" and
/// rewrites them to the space glyph after shaping.
///
/// Coverage is deliberately narrow — only the characters that (a)
/// drive joining decisions and (b) would otherwise render as a
/// glyph in fonts like Amiri (which has drawable glyphs for ZWJ
/// variants). Other default-ignorable characters (e.g. the variation
/// selectors) pass through via their cmap mapping, which typically
/// hits `.notdef` and becomes invisible through another path.
const fn is_default_ignorable(ch: char) -> bool {
    matches!(
        ch as u32,
        0x200C  // ZERO WIDTH NON-JOINER
        | 0x200D  // ZERO WIDTH JOINER
        | 0x200E  // LEFT-TO-RIGHT MARK
        | 0x200F  // RIGHT-TO-LEFT MARK
        | 0x061C  // ARABIC LETTER MARK
    )
}

/// GSUB feature tags that `shape()` already dispatches by name,
/// so the user-override walk should skip them rather than
/// double-apply.
fn is_handled_gsub_tag(tag: [u8; 4]) -> bool {
    matches!(
        &tag,
        b"liga" | b"kern" | b"ccmp" | b"rlig" | b"clig" | b"calt"
    )
}

/// Applies every GSUB lookup reachable via the named feature tag
/// to the glyph run in place. Supports lookup types:
///
/// - 1 — Single substitution (`smcp`, `vert`, `salt`, `ss01`…)
/// - 2 — Multiple substitution (`ccmp` decomposition, some scripts)
/// - 3 — Alternate substitution (`salt`, `swsh`, `aalt`) — the
///   alternate index comes from the feature `value` (1-indexed,
///   clamped into the alternate set)
/// - 4 — Ligature substitution (`liga`, `dlig`, `rlig`)
/// - 6 — Chained context substitution (`calt`, `clig`, `init`,
///   `medi`, `fina`, `isol`) with recursive nested lookups
///
/// Extension (type 7) wrappers are unwrapped to the inner type.
/// Unknown lookup types are silently skipped so callers can enable
/// forward-compatible features without the run erroring out.
fn apply_gsub_feature(
    gsub: &Gsub<'_>,
    glyphs: &mut Vec<Glyph>,
    tag: [u8; 4],
    alternate_index: u16,
    prefer_arabic_script: bool,
) {
    if glyphs.is_empty() {
        return;
    }

    let Some(lookup_indices) = lookup_indices_for_feature(gsub, tag, prefer_arabic_script) else {
        return;
    };
    if lookup_indices.is_empty() {
        return;
    }

    for lookup_idx in lookup_indices {
        apply_gsub_lookup(gsub, lookup_idx, glyphs, alternate_index);
    }
}

/// Applies the four Arabic positional features — `isol`, `init`,
/// `medi`, `fina` — each restricted to the glyph positions whose
/// [`JoiningForm`] matches. The forms slice stays aligned with the
/// glyph run because we call this before any `ccmp`/`rlig`/`liga`
/// substitution has shrunk or expanded the stream (see the call
/// site in [`shape`]).
fn apply_arabic_positional_features(
    gsub: &Gsub<'_>,
    glyphs: &mut Vec<Glyph>,
    forms: &[JoiningForm],
) {
    for (form, tag) in [
        (JoiningForm::Isol, *b"isol"),
        (JoiningForm::Init, *b"init"),
        (JoiningForm::Medi, *b"medi"),
        (JoiningForm::Fina, *b"fina"),
    ] {
        let Some(lookup_indices) = lookup_indices_for_feature(gsub, tag, true) else {
            continue;
        };
        if lookup_indices.is_empty() {
            continue;
        }
        let mask: Vec<bool> = forms.iter().map(|&f| f == form).collect();
        for lookup_idx in lookup_indices {
            apply_gsub_lookup_masked(gsub, lookup_idx, glyphs, &mask);
        }
    }
}

/// Applies a single GSUB lookup only at positions where `mask[i]`
/// is true. Used by the Arabic positional pass — `isol` at positions
/// tagged `Isol`, `init` at `Init`, and so on. Only per-glyph
/// lookup types are masked here (SINGLE / MULTIPLE / ALTERNATE /
/// LIGATURE); chained-context lookups inside an Arabic positional
/// feature are uncommon and fall through to the unmasked dispatcher
/// so no functionality is lost.
fn apply_gsub_lookup_masked(
    gsub: &Gsub<'_>,
    lookup_idx: u16,
    glyphs: &mut Vec<Glyph>,
    mask: &[bool],
) {
    let lookup_list = gsub.lookup_list();
    let Some(lookup) = lookup_list.get(lookup_idx) else {
        return;
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
                for (i, glyph) in glyphs.iter_mut().enumerate() {
                    if !mask.get(i).copied().unwrap_or(false) {
                        continue;
                    }
                    let id = glyph.glyph_id as u16;
                    if let Some(out) = single.apply(id) {
                        glyph.glyph_id = u32::from(out);
                    }
                }
            }
            gsub_lt::ALTERNATE => {
                let Ok(alt) = Alternate::parse(inner_bytes) else {
                    continue;
                };
                for (i, glyph) in glyphs.iter_mut().enumerate() {
                    if !mask.get(i).copied().unwrap_or(false) {
                        continue;
                    }
                    let id = glyph.glyph_id as u16;
                    if let Some(out) = alt.apply(id, 0) {
                        glyph.glyph_id = u32::from(out);
                    }
                }
            }
            gsub_lt::LIGATURE => {
                let Ok(lig) = Ligature::parse(inner_bytes) else {
                    continue;
                };
                // For ligature lookups in a positional feature the
                // mask gates the *first* component; the rest of the
                // window is consumed as-is. Rare in practice — the
                // `rlig` feature is where ligation happens in Arabic,
                // not `init`/`medi`/`fina`/`isol` — but supported for
                // completeness.
                let mut i = 0;
                while i < glyphs.len() {
                    if !mask.get(i).copied().unwrap_or(false) {
                        i += 1;
                        continue;
                    }
                    let window: Vec<u16> =
                        glyphs[i..].iter().map(|g| g.glyph_id as u16).collect();
                    if let Some((lig_glyph, consumed)) = lig.apply(&window) {
                        glyphs[i].glyph_id = u32::from(lig_glyph);
                        glyphs.drain(i + 1..i + consumed);
                    } else {
                        i += 1;
                    }
                }
            }
            gsub_lt::MULTIPLE => {
                let Ok(m) = Multiple::parse(inner_bytes) else {
                    continue;
                };
                let mut i = 0;
                while i < glyphs.len() {
                    if !mask.get(i).copied().unwrap_or(false) {
                        i += 1;
                        continue;
                    }
                    let id = glyphs[i].glyph_id as u16;
                    if let Some(seq) = m.apply(id) {
                        if let Some(n) = expand_glyph_in_place(glyphs, i, &seq) {
                            i += n;
                            continue;
                        }
                    }
                    i += 1;
                }
            }
            // Chained-context inside a positional feature: run over
            // the whole glyph stream. Fonts that care about position
            // encode that via the chained rule's coverage, not via
            // our mask.
            gsub_lt::CHAINED_CONTEXT => {
                let Ok(ctx) = ChainContext::parse(inner_bytes) else {
                    continue;
                };
                apply_chain_context_subtable(gsub, &ctx, glyphs);
            }
            _ => {}
        }
    }
}

/// Applies a single GSUB lookup by index, walking its subtables in
/// spec order. Stops at the first subtable of a supported type that
/// runs on the full glyph run.
fn apply_gsub_lookup(
    gsub: &Gsub<'_>,
    lookup_idx: u16,
    glyphs: &mut Vec<Glyph>,
    alternate_index: u16,
) {
    let lookup_list = gsub.lookup_list();
    let Some(lookup) = lookup_list.get(lookup_idx) else {
        return;
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
            gsub_lt::MULTIPLE => {
                let Ok(m) = Multiple::parse(inner_bytes) else {
                    continue;
                };
                apply_multiple_subtable(&m, glyphs);
            }
            gsub_lt::ALTERNATE => {
                let Ok(alt) = Alternate::parse(inner_bytes) else {
                    continue;
                };
                apply_alternate_subtable(&alt, glyphs, alternate_index);
            }
            gsub_lt::LIGATURE => {
                let Ok(lig) = Ligature::parse(inner_bytes) else {
                    continue;
                };
                apply_liga_subtable(&lig, glyphs);
            }
            gsub_lt::CHAINED_CONTEXT => {
                let Ok(ctx) = ChainContext::parse(inner_bytes) else {
                    continue;
                };
                apply_chain_context_subtable(gsub, &ctx, glyphs);
            }
            _ => {}
        }
    }
}

/// Applies a nested GSUB lookup at one specific position in the
/// run. Returns the number of glyphs the nested lookup consumed
/// (1 for single substitution, N for ligature, 0 when the lookup
/// did not fire). Called from inside `apply_chain_context_subtable`.
fn apply_gsub_lookup_at(
    gsub: &Gsub<'_>,
    lookup_idx: u16,
    glyphs: &mut Vec<Glyph>,
    at: usize,
) -> usize {
    if at >= glyphs.len() {
        return 0;
    }
    let lookup_list = gsub.lookup_list();
    let Some(lookup) = lookup_list.get(lookup_idx) else {
        return 0;
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
                let id = glyphs[at].glyph_id as u16;
                if let Some(out) = single.apply(id) {
                    glyphs[at].glyph_id = u32::from(out);
                    return 1;
                }
            }
            gsub_lt::MULTIPLE => {
                let Ok(m) = Multiple::parse(inner_bytes) else {
                    continue;
                };
                let id = glyphs[at].glyph_id as u16;
                if let Some(seq) = m.apply(id) {
                    if let Some(n) = expand_glyph_in_place(glyphs, at, &seq) {
                        return n;
                    }
                }
            }
            gsub_lt::ALTERNATE => {
                let Ok(alt) = Alternate::parse(inner_bytes) else {
                    continue;
                };
                let id = glyphs[at].glyph_id as u16;
                // Nested alternate lookups pick index 0 — the
                // feature-value-based selection is a top-level
                // concept that does not propagate into context.
                if let Some(out) = alt.apply(id, 0) {
                    glyphs[at].glyph_id = u32::from(out);
                    return 1;
                }
            }
            gsub_lt::LIGATURE => {
                let Ok(lig) = Ligature::parse(inner_bytes) else {
                    continue;
                };
                let window: Vec<u16> = glyphs[at..].iter().map(|g| g.glyph_id as u16).collect();
                if let Some((out, consumed)) = lig.apply(&window) {
                    glyphs[at].glyph_id = u32::from(out);
                    glyphs.drain(at + 1..at + consumed);
                    return consumed;
                }
            }
            // Chained-context lookups nested inside another
            // chained-context rule are theoretically legal but rare;
            // skip for now so we do not risk infinite recursion
            // without a depth guard.
            _ => {}
        }
    }
    0
}

/// Replaces `glyphs[at]` with the given sequence in place. Cluster
/// is copied from the original glyph so every expanded sub-glyph
/// still points back to its source codepoint. Returns the output
/// length when `seq` is non-empty, `None` on a zero-length sequence
/// (which the spec forbids but we treat as a safe no-op).
fn expand_glyph_in_place(glyphs: &mut Vec<Glyph>, at: usize, seq: &[u16]) -> Option<usize> {
    if seq.is_empty() {
        return None;
    }
    let source_cluster = glyphs[at].cluster;
    glyphs[at].glyph_id = u32::from(seq[0]);
    for (i, &out_gid) in seq.iter().enumerate().skip(1) {
        glyphs.insert(
            at + i,
            Glyph {
                glyph_id: u32::from(out_gid),
                cluster: source_cluster,
                x_advance: 0,
                y_advance: 0,
                x_offset: 0,
                y_offset: 0,
            },
        );
    }
    Some(seq.len())
}

/// Scans the glyph run for a chained-context match and, on every
/// hit, applies each `SubstLookupRecord` in order at its declared
/// position inside the input window.
fn apply_chain_context_subtable(gsub: &Gsub<'_>, ctx: &ChainContext<'_>, glyphs: &mut Vec<Glyph>) {
    let (_, input_len, _) = ctx.context_len();
    let mut i = 0;
    while i < glyphs.len() {
        // Snapshot glyph ids each iteration because nested lookups
        // can mutate the stream. For typical run lengths (< 200
        // glyphs) this cost is invisible; a more optimal design
        // would update the snapshot incrementally.
        let ids: Vec<u16> = glyphs.iter().map(|g| g.glyph_id as u16).collect();
        if ctx.matches(&ids, i) {
            for rec in ctx.substitutions() {
                let at = i + rec.sequence_index as usize;
                apply_gsub_lookup_at(gsub, rec.lookup_list_index, glyphs, at);
            }
            // Advance past the input window. If the nested lookups
            // collapsed the window (ligature substitution), the
            // glyph stream shrank — we advance by at most one
            // position because the stream may now look different.
            i += input_len.max(1);
        } else {
            i += 1;
        }
    }
}

/// Walks the chosen LangSys and returns the sorted set of lookup
/// indices that the feature `tag` selects.
///
/// Script selection:
///
/// - `prefer_arabic_script == true` tries `arab` first, then `DFLT`,
///   then the first script in the list. Used when the run contains
///   Arabic so positional features resolve from the Arabic LangSys.
/// - `false` keeps the original DFLT → first-script order used by
///   the Latin path.
///
/// A return of `None` means no usable script exists at all.
/// `Some(empty)` means the chosen LangSys does not carry this
/// feature — caller skips the pass.
fn lookup_indices_for_feature(
    gsub: &Gsub<'_>,
    tag: [u8; 4],
    prefer_arabic_script: bool,
) -> Option<Vec<u16>> {
    let script_list = gsub.script_list();
    let script = if prefer_arabic_script {
        script_list
            .find(*b"arab")
            .or_else(|| script_list.find(*b"DFLT"))
            .or_else(|| script_list.iter().next().map(|(_, s)| s))?
    } else {
        script_list
            .find(*b"DFLT")
            .or_else(|| script_list.iter().next().map(|(_, s)| s))?
    };
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

fn apply_multiple_subtable(m: &Multiple<'_>, glyphs: &mut Vec<Glyph>) {
    let mut i = 0;
    while i < glyphs.len() {
        let id = glyphs[i].glyph_id as u16;
        if let Some(seq) = m.apply(id) {
            if let Some(n) = expand_glyph_in_place(glyphs, i, &seq) {
                i += n;
                continue;
            }
        }
        i += 1;
    }
}

fn apply_alternate_subtable(alt: &Alternate<'_>, glyphs: &mut [Glyph], alternate_index: u16) {
    for glyph in glyphs.iter_mut() {
        let id = glyph.glyph_id as u16;
        if let Some(out) = alt.apply(id, alternate_index) {
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

/// Applies every GPOS lookup reachable via the named feature tag
/// to the glyph run in place. Supports lookup types:
///
/// - 1 — Single adjustment (uniform or per-glyph ValueRecord)
/// - 2 — Pair adjustment (kern)
/// - 4 — Mark-to-base attachment (mark)
/// - 5 — Mark-to-ligature attachment (mark on ligature components)
/// - 6 — Mark-to-mark attachment (mkmk stacking)
/// - 9 — Extension (unwraps, re-dispatches)
///
/// Returns `true` when at least one subtable of a supported type
/// actually ran. Callers use this to decide whether to fall back
/// to the legacy `kern` table (for the `kern` feature specifically).
fn apply_gpos_feature(
    gpos: &Gpos<'_>,
    glyphs: &mut [Glyph],
    gdef: Option<&Gdef<'_>>,
    tag: [u8; 4],
    prefer_arabic_script: bool,
) -> bool {
    if glyphs.is_empty() {
        return false;
    }

    let Some(lookup_indices) = gpos_lookup_indices_for_feature(gpos, tag, prefer_arabic_script)
    else {
        return false;
    };
    if lookup_indices.is_empty() {
        return false;
    }

    let lookup_list = gpos.lookup_list();
    let mut ran_any = false;
    for lookup_idx in lookup_indices {
        let Some(lookup) = lookup_list.get(lookup_idx) else {
            continue;
        };
        let raw_lt = lookup.lookup_type();
        for sub_idx in 0..lookup.subtable_count() {
            let Some(bytes) = lookup.subtable_bytes(sub_idx) else {
                continue;
            };
            let (effective_lt, inner_bytes) = if raw_lt == gpos_lt::EXTENSION {
                match resolve_extension(bytes) {
                    Some((inner_type, inner)) => (inner_type, inner),
                    None => continue,
                }
            } else {
                (raw_lt, bytes)
            };

            match effective_lt {
                gpos_lt::SINGLE_ADJUSTMENT => {
                    let Ok(sp) = SinglePos::parse(inner_bytes) else {
                        continue;
                    };
                    apply_single_pos(&sp, glyphs);
                    ran_any = true;
                }
                gpos_lt::PAIR_ADJUSTMENT => {
                    let Ok(pp) = PairPos::parse(inner_bytes) else {
                        continue;
                    };
                    apply_pair_pos(&pp, glyphs);
                    ran_any = true;
                }
                gpos_lt::MARK_TO_BASE => {
                    let Ok(mbp) = MarkBasePos::parse(inner_bytes) else {
                        continue;
                    };
                    apply_mark_base(&mbp, glyphs, gdef);
                    ran_any = true;
                }
                gpos_lt::MARK_TO_LIGATURE => {
                    let Ok(mlp) = MarkLigaPos::parse(inner_bytes) else {
                        continue;
                    };
                    apply_mark_liga(&mlp, glyphs, gdef);
                    ran_any = true;
                }
                gpos_lt::MARK_TO_MARK => {
                    let Ok(mmp) = MarkMarkPos::parse(inner_bytes) else {
                        continue;
                    };
                    apply_mark_mark(&mmp, glyphs, gdef);
                    ran_any = true;
                }
                _ => {}
            }
        }
    }
    ran_any
}

/// Default-LangSys lookup-index collection for a GPOS feature tag,
/// mirroring the GSUB helper.
fn gpos_lookup_indices_for_feature(
    gpos: &Gpos<'_>,
    tag: [u8; 4],
    prefer_arabic_script: bool,
) -> Option<Vec<u16>> {
    let script_list = gpos.script_list();
    let script = if prefer_arabic_script {
        script_list
            .find(*b"arab")
            .or_else(|| script_list.find(*b"DFLT"))
            .or_else(|| script_list.iter().next().map(|(_, s)| s))?
    } else {
        script_list
            .find(*b"DFLT")
            .or_else(|| script_list.iter().next().map(|(_, s)| s))?
    };
    let lang_sys = script.default_lang_sys()?;

    let feature_list = gpos.feature_list();
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

/// Walks the run and applies the single-adjustment subtable to
/// every covered glyph. HarfBuzz ignores marks for single pos
/// only when the lookup flag says so; we match that in a future
/// pass.
fn apply_single_pos(sp: &SinglePos<'_>, glyphs: &mut [Glyph]) {
    for glyph in glyphs.iter_mut() {
        let id = glyph.glyph_id as u16;
        if let Some(v) = sp.adjustment(id) {
            glyph.x_offset += i32::from(v.x_placement);
            glyph.y_offset += i32::from(v.y_placement);
            glyph.x_advance += i32::from(v.x_advance);
            glyph.y_advance += i32::from(v.y_advance);
        }
    }
}

/// Walks the run and, for each mark glyph (per GDEF), finds the
/// nearest preceding base and attaches via the subtable's anchor
/// tables. Without GDEF we cannot distinguish marks from bases and
/// the pass is a no-op — that matches HarfBuzz's behaviour.
fn apply_mark_base(mbp: &MarkBasePos<'_>, glyphs: &mut [Glyph], gdef: Option<&Gdef<'_>>) {
    let Some(gdef) = gdef else {
        return;
    };

    for i in 0..glyphs.len() {
        let mark_gid = glyphs[i].glyph_id as u16;
        if !gdef.glyph_class(mark_gid).is_mark() {
            continue;
        }
        // Walk back to the nearest base. The immediate preceding
        // glyph might be another mark (diacritic stacking); skip
        // marks looking for the real base. Treat "class Other" as
        // base-ish so exotic fonts do not silently drop marks.
        let Some(base_i) = (0..i).rev().find(|&j| {
            let cls = gdef.glyph_class(glyphs[j].glyph_id as u16);
            cls != GlyphClass::Mark
        }) else {
            continue;
        };
        let base_gid = glyphs[base_i].glyph_id as u16;
        let Some(attach) = mbp.attach(mark_gid, base_gid) else {
            continue;
        };

        // Accumulate the advance between the base and the mark so
        // the delta accounts for any glyphs (e.g. stacked marks)
        // that sat in between.
        let mut walked_advance: i32 = 0;
        for glyph in &glyphs[base_i..i] {
            walked_advance += glyph.x_advance;
        }

        let dx = i32::from(attach.base_anchor.x) - i32::from(attach.mark_anchor.x) - walked_advance;
        let dy = i32::from(attach.base_anchor.y) - i32::from(attach.mark_anchor.y);
        glyphs[i].x_offset += dx;
        glyphs[i].y_offset += dy;
        // Marks do not advance the pen — replace whatever hmtx
        // reported with zero so successive text lines up.
        glyphs[i].x_advance = 0;
    }
}

/// Walks the run and, for each mark glyph, attaches it to the
/// nearest preceding *ligature* base. Component selection uses a
/// cluster-delta heuristic (how many input codepoints after the
/// ligature's first cluster the mark belongs to); this is accurate
/// for the common case where each codepoint after the base owns
/// exactly one component, and degrades gracefully (falls through
/// to a base-component anchor) when the subtable only carries
/// anchors for lower component indices.
fn apply_mark_liga(mlp: &MarkLigaPos<'_>, glyphs: &mut [Glyph], gdef: Option<&Gdef<'_>>) {
    let Some(gdef) = gdef else {
        return;
    };

    for i in 0..glyphs.len() {
        let mark_gid = glyphs[i].glyph_id as u16;
        if !gdef.glyph_class(mark_gid).is_mark() {
            continue;
        }
        let Some(base_i) = (0..i).rev().find(|&j| {
            let cls = gdef.glyph_class(glyphs[j].glyph_id as u16);
            cls != GlyphClass::Mark
        }) else {
            continue;
        };
        let base_gid = glyphs[base_i].glyph_id as u16;

        // Derive a component index from the difference in cluster
        // values. A single-component ligature collapses to 0.
        let cluster_delta = glyphs[i].cluster.saturating_sub(glyphs[base_i].cluster);
        let component_index = cluster_delta.min(u32::from(u16::MAX)) as u16;

        // Try the computed component first; fall back to 0 so marks
        // on fonts that only anchor component 0 still land somewhere
        // sane instead of being dropped silently.
        let attach = mlp
            .attach(mark_gid, base_gid, component_index)
            .or_else(|| mlp.attach(mark_gid, base_gid, 0));
        let Some(attach) = attach else {
            continue;
        };

        let mut walked_advance: i32 = 0;
        for glyph in &glyphs[base_i..i] {
            walked_advance += glyph.x_advance;
        }
        let dx = i32::from(attach.base_anchor.x) - i32::from(attach.mark_anchor.x) - walked_advance;
        let dy = i32::from(attach.base_anchor.y) - i32::from(attach.mark_anchor.y);
        glyphs[i].x_offset += dx;
        glyphs[i].y_offset += dy;
        glyphs[i].x_advance = 0;
    }
}

/// Walks the run and stacks each mark glyph onto the immediately
/// preceding mark glyph, using the subtable's mark1/mark2 anchor
/// pair. The previous glyph must itself be a mark (per GDEF) for
/// this lookup to fire; otherwise mark-to-base handles the case.
fn apply_mark_mark(mmp: &MarkMarkPos<'_>, glyphs: &mut [Glyph], gdef: Option<&Gdef<'_>>) {
    let Some(gdef) = gdef else {
        return;
    };

    for i in 1..glyphs.len() {
        let mark1_gid = glyphs[i].glyph_id as u16;
        if !gdef.glyph_class(mark1_gid).is_mark() {
            continue;
        }
        let mark2_gid = glyphs[i - 1].glyph_id as u16;
        if !gdef.glyph_class(mark2_gid).is_mark() {
            continue;
        }
        let Some(attach) = mmp.attach(mark1_gid, mark2_gid) else {
            continue;
        };

        // The lower mark has already been placed (by mark-to-base or
        // a prior mark-to-mark). Its x_offset/y_offset encode where
        // it sits relative to its own origin, so we stack the upper
        // mark relative to that position. The lower mark's advance
        // is zero (marks do not advance), so we only need to add its
        // own offsets to the attachment delta.
        let lower_mark_x = glyphs[i - 1].x_offset;
        let lower_mark_y = glyphs[i - 1].y_offset;
        let dx = i32::from(attach.base_anchor.x) - i32::from(attach.mark_anchor.x) + lower_mark_x;
        let dy = i32::from(attach.base_anchor.y) - i32::from(attach.mark_anchor.y) + lower_mark_y;
        glyphs[i].x_offset += dx;
        glyphs[i].y_offset += dy;
        glyphs[i].x_advance = 0;
    }
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
    fn normalize_nfc_flag_collapses_decomposed_input() {
        // Test font has no cmap entry for 'e', combining acute, or
        // precomposed 'é', so every path ends up at .notdef. The
        // meaningful difference is glyph count: NFC off → 2 glyphs
        // (e + combining acute both go to .notdef); NFC on → 1 glyph
        // (the pair composes to 'é' before cmap).
        let data = build_shapeable_font();
        let blob = Blob::new(&data);
        let face = Face::parse(&blob, 0).unwrap();
        let font = Font::new(face, 16.0);

        let mut buffer = Buffer::new();
        buffer.push_str("e\u{0301}");
        let before = shape(&font, &buffer, &[]).unwrap();
        assert_eq!(before.len(), 2);

        buffer.set_normalize_nfc(true);
        let after = shape(&font, &buffer, &[]).unwrap();
        assert_eq!(after.len(), 1);
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
