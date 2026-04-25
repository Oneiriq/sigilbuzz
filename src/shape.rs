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

use crate::buffer::{script_priority_for, unicode_prop, Buffer, Glyph, ShapedRun};
use crate::error::Result;
use crate::font::Font;
use crate::ot::arabic::{assign_joining_forms, JoiningForm};
use crate::tables::gdef::{Gdef, GlyphClass};
use crate::tables::gpos::{
    lookup_type as gpos_lt, resolve_variation_delta, ChainContextPos, ContextPos, MarkBasePos,
    MarkLigaPos, MarkMarkPos, PairPos, SinglePos, ValueRecord,
};
use crate::tables::gsub::{
    lookup_type as gsub_lt, Alternate, ChainContextAny, Context as GsubContext, Ligature, Multiple,
    ReverseChain, Single,
};
use crate::tables::layout::{Lookup, MatchFilter, SequenceLookupRecord};
use crate::tables::variation_store::ItemVariationStore;
use crate::tables::{Gpos, Gsub, KernTable, Kerx, Morx};
use crate::unicode::{script_of, Script};

/// Variable-font context threaded through every GPOS apply site.
///
/// Decoupling this from the GPOS tables themselves means every
/// apply function keeps the same shape for variable and static
/// fonts; static callers pass [`VarCtx::none`] and every resolver
/// short-circuits to zero without reading the store.
#[derive(Debug, Clone, Copy)]
struct VarCtx<'a> {
    /// Normalised axis coords. Empty for the default instance.
    coords: &'a [f32],
    /// GDEF's shared `ItemVariationStore`. Required for every
    /// `VariationIndex` the ValueRecord's device slots point at.
    store: Option<&'a ItemVariationStore<'a>>,
}

impl VarCtx<'_> {
    /// Builds a static-instance context — no coords, no store.
    /// Every downstream resolver produces a zero delta. Used by
    /// callers (and tests) that need to invoke a GPOS apply site
    /// without having a font-coords view in hand.
    #[allow(dead_code)]
    const fn none() -> Self {
        Self {
            coords: &[],
            store: None,
        }
    }

    /// True when the context can actually produce a non-zero delta:
    /// coords must be non-empty *and* a store must be attached.
    /// Callers use this to skip the resolver work entirely for the
    /// common default-instance case.
    #[inline]
    fn is_active(&self) -> bool {
        !self.coords.is_empty() && self.store.is_some()
    }

    /// Resolves the variation delta for one `(subtable, device_off)`
    /// pair against the active coords and store. When inactive,
    /// returns zero without touching the subtable bytes.
    #[inline]
    fn resolve(&self, subtable: &[u8], device_off: u16) -> i32 {
        if !self.is_active() || device_off == 0 {
            return 0;
        }
        resolve_variation_delta(subtable, device_off, self.store, self.coords)
    }
}

/// Applies one `ValueRecord` to `glyph`, folding in any
/// Device/VariationIndex deltas the `subtable` carries for this
/// record. `subtable` is the enclosing PairPos/SinglePos subtable
/// bytes — Device/VariationIndex sub-offsets are always rooted
/// there.
#[allow(clippy::similar_names)]
fn apply_value_record(glyph: &mut Glyph, v: &ValueRecord, subtable: &[u8], var: &VarCtx<'_>) {
    let dx_place = var.resolve(subtable, v.x_placement_device_off);
    let dy_place = var.resolve(subtable, v.y_placement_device_off);
    let dx_adv = var.resolve(subtable, v.x_advance_device_off);
    let dy_adv = var.resolve(subtable, v.y_advance_device_off);
    glyph.x_offset += i32::from(v.x_placement) + dx_place;
    glyph.y_offset += i32::from(v.y_placement) + dy_place;
    glyph.x_advance += i32::from(v.x_advance) + dx_adv;
    glyph.y_advance += i32::from(v.y_advance) + dy_adv;
}

/// Maximum recursion depth for nested-lookup dispatch. Matches the
/// limit HarfBuzz uses (`HB_MAX_NESTING_LEVEL = 16`); any deeper and
/// we assume the font is pathological (a cycle in the LookupList)
/// and stop rather than overflow the stack.
const MAX_NESTED_DEPTH: u8 = 16;

/// One pre-parsed GSUB subtable, ready to drive a cursor walk.
///
/// `apply_gsub_lookup` parses the lookup's subtables once into this
/// enum and reuses the parsed views across every cursor step. Without
/// the cache, ChainContext / Context format-3 parsing allocates three
/// or four `Vec<Coverage>` and a `Vec<SubstLookupRecord>` on every
/// cursor — `O(N × subtables)` allocations for a single feature, the
/// lion's share of the Devanagari regression.
enum ParsedGsubSubtable<'a> {
    Single(Single<'a>),
    Multiple(Multiple<'a>),
    Alternate(Alternate<'a>),
    Ligature(Ligature<'a>),
    Context(GsubContext<'a>),
    ChainContext(ChainContextAny<'a>),
    ReverseChained(ReverseChain<'a>),
}

/// Parses the subtables of a single `Lookup` — handling the Extension
/// type-7 unwrap so the caller never sees raw lookup type 7. Returns
/// the parsed list in spec order; subtables that fail to parse are
/// silently dropped, matching the per-cursor behaviour the inline
/// `apply_gsub_lookup_at` walker had before the cache was introduced.
fn parse_lookup_subtables<'a>(lookup: &Lookup<'a>, raw_lt: u16) -> Vec<ParsedGsubSubtable<'a>> {
    let count = lookup.subtable_count() as usize;
    let mut out: Vec<ParsedGsubSubtable<'a>> = Vec::with_capacity(count);
    for sub_idx in 0..lookup.subtable_count() {
        let Some(bytes) = lookup.subtable_bytes(sub_idx) else {
            continue;
        };
        let (effective_lt, inner_bytes) = if raw_lt == gsub_lt::EXTENSION {
            match resolve_extension(bytes) {
                Some(pair) => pair,
                None => continue,
            }
        } else {
            (raw_lt, bytes)
        };
        let parsed = match effective_lt {
            gsub_lt::SINGLE => Single::parse(inner_bytes)
                .ok()
                .map(ParsedGsubSubtable::Single),
            gsub_lt::MULTIPLE => Multiple::parse(inner_bytes)
                .ok()
                .map(ParsedGsubSubtable::Multiple),
            gsub_lt::ALTERNATE => Alternate::parse(inner_bytes)
                .ok()
                .map(ParsedGsubSubtable::Alternate),
            gsub_lt::LIGATURE => Ligature::parse(inner_bytes)
                .ok()
                .map(ParsedGsubSubtable::Ligature),
            gsub_lt::CONTEXT => GsubContext::parse(inner_bytes)
                .ok()
                .map(ParsedGsubSubtable::Context),
            gsub_lt::CHAINED_CONTEXT => ChainContextAny::parse(inner_bytes)
                .ok()
                .map(ParsedGsubSubtable::ChainContext),
            gsub_lt::REVERSE_CHAINED => ReverseChain::parse(inner_bytes)
                .ok()
                .map(ParsedGsubSubtable::ReverseChained),
            _ => None,
        };
        if let Some(p) = parsed {
            out.push(p);
        }
    }
    out
}

/// Returns the "primary" coverage table for a parsed subtable — the
/// coverage on the cursor glyph. Used by the run-level `would_apply`
/// precheck and by the cursor digest in `apply_gsub_lookup`. `None`
/// means the subtable's coverage isn't a single `Coverage` table
/// (chain-context format 1/2, reverse-chain, ...) and the cursor
/// walker has to fall back to per-position dispatch.
fn primary_coverage_of<'a, 'b>(
    sub: &'b ParsedGsubSubtable<'a>,
) -> Option<&'b crate::tables::layout::Coverage<'a>> {
    match sub {
        ParsedGsubSubtable::Single(
            Single::Delta { coverage, .. } | Single::Explicit { coverage, .. },
        ) => Some(coverage),
        ParsedGsubSubtable::Multiple(m) => Some(m.coverage()),
        ParsedGsubSubtable::Alternate(a) => Some(a.coverage()),
        ParsedGsubSubtable::Ligature(l) => Some(l.coverage()),
        ParsedGsubSubtable::ChainContext(ChainContextAny::Format3(c3)) => c3.input_first_coverage(),
        ParsedGsubSubtable::Context(GsubContext::Format3(c3)) => c3.input().first(),
        _ => None,
    }
}

/// Reports whether at least one glyph in `ids` could trigger any
/// subtable in `parsed` — a fast pre-filter so the cursor walk in
/// `apply_gsub_lookup` skips lookups whose coverage doesn't intersect
/// the run at all. Mirrors HarfBuzz's `would_apply` skip; returns
/// `true` conservatively when a subtable doesn't expose its primary
/// coverage cheaply.
fn lookup_might_apply(parsed: &[ParsedGsubSubtable<'_>], ids: &[u16]) -> bool {
    if ids.is_empty() {
        return false;
    }
    for sub in parsed {
        match primary_coverage_of(sub) {
            None => return true,
            Some(cov) => {
                for &id in ids {
                    if cov.contains(id) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// True when every subtable in `parsed` exposes a single primary
/// coverage we can intersect with the run. When that holds, the
/// cursor walker can use the "digest" path: skip any cursor whose
/// glyph isn't in the union of those coverages, instead of trying
/// every subtable at every cursor.
fn parsed_has_full_digest(parsed: &[ParsedGsubSubtable<'_>]) -> bool {
    parsed.iter().all(|s| primary_coverage_of(s).is_some())
}

/// True when `glyphs[i]` is in any of `parsed`'s primary coverages.
/// Caller has already established that every subtable exposes one
/// (`parsed_has_full_digest`). Falling out of the digest path back to
/// the per-position walker happens at the caller level.
fn cursor_in_digest(parsed: &[ParsedGsubSubtable<'_>], id: u16) -> bool {
    for sub in parsed {
        if let Some(cov) = primary_coverage_of(sub) {
            if cov.contains(id) {
                return true;
            }
        }
    }
    false
}

/// Cursor-position dispatch over a pre-parsed subtable list. Mirrors
/// the inner loop of `apply_gsub_lookup_at` but without the
/// per-cursor parse cost. Returns the input span the matching subtable
/// consumed (1 for Single/Alternate, N for Ligature, the input window
/// length for Context / Chain / Reverse), or 0 when no subtable fired.
#[allow(clippy::too_many_arguments)]
fn apply_parsed_lookup_at(
    gsub: &Gsub<'_>,
    parsed: &[ParsedGsubSubtable<'_>],
    filter: &MatchFilter<'_>,
    glyphs: &mut Vec<Glyph>,
    ids: &mut GlyphIds,
    gdef: Option<&Gdef<'_>>,
    at: usize,
    depth: u8,
    alternate_index: u16,
) -> usize {
    if at >= glyphs.len() {
        return 0;
    }
    for sub in parsed {
        match sub {
            ParsedGsubSubtable::Single(single) => {
                let id = glyphs[at].glyph_id as u16;
                if filter.is_skipped(id) {
                    continue;
                }
                if let Some(out) = single.apply(id) {
                    glyphs[at].glyph_id = u32::from(out);
                    ids.set(at, out);
                    return 1;
                }
            }
            ParsedGsubSubtable::Multiple(m) => {
                let id = glyphs[at].glyph_id as u16;
                if filter.is_skipped(id) {
                    continue;
                }
                if let Some(seq) = m.apply(id) {
                    if let Some(n) = expand_glyph_in_place(glyphs, at, &seq) {
                        ids.resync(glyphs);
                        return n;
                    }
                }
            }
            ParsedGsubSubtable::Alternate(alt) => {
                let id = glyphs[at].glyph_id as u16;
                if filter.is_skipped(id) {
                    continue;
                }
                if let Some(out) = alt.apply(id, alternate_index) {
                    glyphs[at].glyph_id = u32::from(out);
                    ids.set(at, out);
                    return 1;
                }
            }
            ParsedGsubSubtable::Ligature(lig) => {
                if let Some((out, positions)) = lig.apply_filtered(&ids.as_slice()[at..], filter) {
                    glyphs[at].glyph_id = u32::from(out);
                    drain_ligature_components(glyphs, at, &positions);
                    ids.resync(glyphs);
                    // Ligature emits 1 glyph from N matched components.
                    // The cursor must advance past the ligature output
                    // *and* any skipped (filtered) glyphs that survived
                    // inside the matched window — in HarfBuzz's
                    // input/output buffer model that is `idx + span` in
                    // INPUT space; in our in-place model the buffer
                    // already shrunk by `(positions.len() - 1)` glyphs,
                    // so the equivalent NEW-buffer advance is
                    // `span - (positions.len() - 1)` = `1 + skipped`.
                    //
                    // Returning the raw input span over-advances by the
                    // number of consumed components, which silently skips
                    // the next-letter slot — visible as Mongolian's calt
                    // marker-pass leaking marker glyphs on 3+ letter
                    // chains (#118).
                    let span = positions.last().copied().map_or(0, |p| p + 1);
                    let advance = 1 + span.saturating_sub(positions.len());
                    return advance;
                }
            }
            ParsedGsubSubtable::Context(ctx) => {
                let ran =
                    apply_gsub_context_at(gsub, ctx, glyphs, ids, gdef, filter, at, depth + 1);
                if ran > 0 {
                    return ran;
                }
            }
            ParsedGsubSubtable::ChainContext(chain) => {
                let ran = apply_gsub_chain_context_at(
                    gsub,
                    chain,
                    glyphs,
                    ids,
                    gdef,
                    filter,
                    at,
                    depth + 1,
                );
                if ran > 0 {
                    return ran;
                }
            }
            ParsedGsubSubtable::ReverseChained(rc) => {
                if let Some(out) = rc.apply(ids.as_slice(), at) {
                    glyphs[at].glyph_id = u32::from(out);
                    ids.set(at, out);
                    return 1;
                }
            }
        }
    }
    0
}

/// Mirror buffer of `glyph_id`s, kept in lockstep with the live
/// `Vec<Glyph>` that GSUB drivers mutate. The matchers for context /
/// chained-context / reverse-chain / ligature subtables all want a
/// flat `&[u16]` for backtrack/lookahead/window scanning; before this
/// shadow buffer existed every cursor step rebuilt that slice via
/// `glyphs.iter().map(...).collect()`, which is `O(N²)` for a feature
/// that fires on every glyph. We keep the shadow in sync manually
/// after each substitution — Single/Alternate touch one slot,
/// Ligature/Multiple change length and trigger a full resync.
#[derive(Debug)]
struct GlyphIds {
    ids: Vec<u16>,
}

impl GlyphIds {
    fn from_glyphs(glyphs: &[Glyph]) -> Self {
        let mut ids = Vec::with_capacity(glyphs.len());
        for g in glyphs {
            ids.push(g.glyph_id as u16);
        }
        Self { ids }
    }

    fn as_slice(&self) -> &[u16] {
        &self.ids
    }

    /// Single-slot update; the glyph at `at` gained a new id but the
    /// stream length is unchanged. Caller has already written to the
    /// `Glyph` struct.
    fn set(&mut self, at: usize, gid: u16) {
        if at < self.ids.len() {
            self.ids[at] = gid;
        }
    }

    /// Length-changing substitution (ligature drain, multiple-sub
    /// expansion). Cheaper than maintaining diff edits inside every
    /// driver — these substitutions are far less common than context
    /// matches anyway.
    fn resync(&mut self, glyphs: &[Glyph]) {
        self.ids.clear();
        for g in glyphs {
            self.ids.push(g.glyph_id as u16);
        }
    }
}

/// Builds a [`MatchFilter`] scoped to one lookup — honouring its
/// `LookupFlag`, GDEF-backed glyph classes, and the optional
/// `markFilteringSet` trailer when the font carries one.
fn filter_for_lookup<'a>(lookup: &Lookup<'a>, gdef: Option<&'a Gdef<'a>>) -> MatchFilter<'a> {
    MatchFilter::for_lookup(lookup.flag(), gdef, lookup.mark_filtering_set())
}

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
/// Pre-iterates `text` and applies Hangul NFC jamo composition in a
/// single pass: a Leading jamo (L) followed by a Vowel jamo (V) and
/// optionally a Trailing jamo (T) collapses into the matching
/// precomposed syllable in U+AC00..U+D7A3 *when* the font carries a
/// cmap entry for the precomposed codepoint. HarfBuzz / rustybuzz do
/// exactly this, so matching the behaviour is mandatory for byte-
/// parity on modern Korean corpora.
///
/// Extended-B trailing jamo (U+D7CB..U+D7FB) abort composition of
/// the whole L+V+T triple so the font's `ljmo` / `vjmo` / `tjmo`
/// features can shape each jamo on its own — matches rustybuzz.
///
/// Returns a vector of `(byte_offset, char)` pairs that replaces the
/// normal `text.char_indices()` sequence in the main shaping loop.
/// The byte offset is the offset of the FIRST codepoint in the
/// composed cluster — the L for an L+V / L+V+T composition — so
/// downstream cluster tracking still maps glyphs back to the
/// original UTF-8 stream.
fn hangul_compose(
    text: &str,
    cmap: &crate::tables::cmap::Cmap<'_>,
) -> alloc::vec::Vec<(u32, char)> {
    let mut out = alloc::vec::Vec::with_capacity(text.len());
    // Gate: HarfBuzz / rustybuzz select the Hangul shaper on a
    // per-run basis and the Hangul preprocessor (the NFC compose)
    // runs only when that shaper is active. sigilbuzz's segmenter
    // splits scripts but the Hangul preprocessor still has to see
    // the segment's L+V(+T) window to run — so we gate on "the
    // buffer is Hangul / whitespace / default-ignorable only".
    // Mixed-script buffers (e.g. "Hi " + jamo) bypass composition;
    // the jamo runs through its own segment under `hang` but stays
    // as L + V glyphs, matching rustybuzz.
    let compose_enabled = text.chars().all(|c| {
        let cp = c as u32;
        matches!(crate::unicode::script_of(c), crate::unicode::Script::Hangul)
            || c == ' '
            || (0x200B..=0x200D).contains(&cp)
            || cp == 0xFEFF
    });
    let mut it = text.char_indices().peekable();
    while let Some((byte_offset, ch)) = it.next() {
        if !compose_enabled {
            out.push((byte_offset as u32, ch));
            continue;
        }
        // L jamo range: U+1100..U+1112 (the 19 modern leading
        // consonants). Extended-A (U+A960..) do NOT compose — they
        // stay as jamo so `ljmo` picks them up.
        let l_index = if (0x1100..=0x1112).contains(&(ch as u32)) {
            Some((ch as u32) - 0x1100)
        } else {
            None
        };
        if let Some(l) = l_index {
            if let Some(&(_, next_ch)) = it.peek() {
                // V jamo range: U+1161..U+1175 (21 modern vowels).
                if (0x1161..=0x1175).contains(&(next_ch as u32)) {
                    let v = (next_ch as u32) - 0x1161;
                    // Peek past V to detect the trailing jamo, if any.
                    // HarfBuzz's rule: only compose when the whole
                    // run is in the modern range. Extended-B T
                    // (U+D7CB..U+D7FB) aborts composition entirely.
                    let mut clone = it.clone();
                    clone.next(); // skip V
                    let t_info = match clone.peek() {
                        Some(&(_, c)) if (0x11A8..=0x11C2).contains(&(c as u32)) => {
                            Some(Some((c as u32) - 0x11A7))
                        }
                        Some(&(_, c)) if (0xD7CB..=0xD7FB).contains(&(c as u32)) => Some(None),
                        _ => None,
                    };
                    if matches!(t_info, Some(None)) {
                        // Extended-B T blocks composition; emit each
                        // jamo as-is. L and V are consumed here; the
                        // T gets emitted naturally on the next
                        // iteration.
                        out.push((byte_offset as u32, ch));
                        out.push((byte_offset as u32, next_ch));
                        it.next(); // consume V
                        continue;
                    }
                    let t = t_info.and_then(|o| o).unwrap_or(0);
                    it.next(); // consume V
                    if t != 0 {
                        it.next(); // consume modern T
                    }
                    let syllable_cp = 0xAC00 + (l * 21 + v) * 28 + t;
                    if let Some(ch_composed) = core::char::from_u32(syllable_cp) {
                        if cmap.glyph_id(ch_composed).is_some() {
                            out.push((byte_offset as u32, ch_composed));
                            continue;
                        }
                    }
                    // Fallback: emit each jamo as-is.
                    out.push((byte_offset as u32, ch));
                    let v_ch = core::char::from_u32(0x1161 + v).unwrap_or(ch);
                    out.push((byte_offset as u32, v_ch));
                    if t != 0 {
                        let t_ch = core::char::from_u32(0x11A7 + t).unwrap_or(ch);
                        out.push((byte_offset as u32, t_ch));
                    }
                    continue;
                }
            }
        }
        out.push((byte_offset as u32, ch));
    }
    out
}

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
    // Vertical layout: explicit when the buffer direction is TTB/BTT,
    // *implicit* when the run is dominantly Mongolian and the caller
    // left the direction at the default LTR. Mongolian's traditional
    // writing axis is top-to-bottom; auto-vertical here lets simple
    // callers shape Mongolian without having to know the default.
    // Consumers who want horizontal Mongolian must set the direction
    // to RTL (vertical-rotated) or pass a non-Mongolian-dominant run.
    let mongolian_dominant = buffer
        .text()
        .chars()
        .any(|c| crate::unicode::script_of(c) == crate::unicode::Script::Mongolian)
        && buffer
            .text()
            .chars()
            .find(|c| !matches!(crate::unicode::script_of(*c), crate::unicode::Script::Other))
            .is_some_and(|c| crate::unicode::script_of(c) == crate::unicode::Script::Mongolian);
    let is_vertical = if buffer.direction() == crate::buffer::Direction::Ltr && mongolian_dominant {
        true
    } else {
        !buffer.direction().is_horizontal()
    };

    let face = font.face();
    let cmap = face.cmap()?;
    let hmtx = face.hmtx()?;
    // Vertical metrics and origin overrides are optional; only look
    // them up when the caller has asked for vertical layout so
    // horizontal callers keep the cheap "hmtx only" path.
    let vmtx = if is_vertical { face.vmtx()? } else { None };

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
    //
    // We also capture the raw `char` list alongside the glyphs so
    // the Indic shaper can consult Unicode properties per-codepoint
    // without re-scanning the UTF-8 stream.
    let space_gid = u32::from(cmap.glyph_id('\u{0020}').unwrap_or(0));
    let mut glyphs: Vec<Glyph> = Vec::with_capacity(text.len());
    let mut codepoints: Vec<char> = Vec::with_capacity(text.len());
    // Preprocess Hangul Jamo NFC composition: L + V (+ optional T)
    // sequences collapse into the precomposed syllable in
    // U+AC00..U+D7A3 when the font carries a cmap entry for the
    // precomposed codepoint. The Jamo sub-blocks outside the modern
    // compositional range (Extended-A L, Extended-B T) suppress
    // composition so the font's `ljmo` / `vjmo` / `tjmo` features can
    // shape each jamo independently — matches HarfBuzz / rustybuzz.
    //
    // Returns `(byte_offset, char)` pairs; the byte offset is always
    // the first codepoint of the composed cluster, so cluster
    // tracking stays aligned with the original UTF-8 stream.
    let composed_chars: Vec<(u32, char)> = hangul_compose(text, &cmap);
    for (cluster, ch) in composed_chars.iter().copied() {
        let cluster = cluster as usize;
        // Khmer split-vowel decomposition. HarfBuzz's USE
        // preprocessing hook splits U+17C4 / U+17C5 into a
        // pre-base component (sign-e) and a post-base component
        // (sign-aa / sign-au) so the syllable machine can see the
        // pre-base part directly. sigilbuzz does it at codepoint
        // push time, before the cmap lookup, so the rest of the
        // pipeline never sees the composed form.
        if matches!(ch, '\u{17C4}' | '\u{17C5}') {
            let (pre, post) = if ch == '\u{17C4}' {
                ('\u{17C1}', '\u{17B6}')
            } else {
                ('\u{17C1}', '\u{17B7}')
            };
            for &component in &[pre, post] {
                let gid = u32::from(cmap.glyph_id(component).unwrap_or(0));
                let glyph = Glyph::new(gid, cluster as u32);
                glyphs.push(glyph);
                codepoints.push(component);
            }
            continue;
        }
        // Thai sara am (U+0E33) and Lao lao am (U+0EB3). HarfBuzz
        // decomposes these composed vowels into
        // `nikkhahit / niggahita + sara aa` at buffer-prep time,
        // before shape enters the state machine — the font's
        // mark-positioning tables target the decomposed pair, not
        // the composed codepoint. We do the same here so the cmap
        // lookup lands on the two components and every downstream
        // pass (GSUB, GPOS, cluster merge) sees the decomposed form
        // rustybuzz does.
        if matches!(ch, '\u{0E33}' | '\u{0EB3}') {
            let (pre, post) = if ch == '\u{0E33}' {
                // Thai sara am → nikkhahit (U+0E4D) + sara aa (U+0E32).
                ('\u{0E4D}', '\u{0E32}')
            } else {
                // Lao lao am → niggahita (U+0ECD) + sara aa (U+0EB2).
                ('\u{0ECD}', '\u{0EB2}')
            };
            for &component in &[pre, post] {
                let gid = u32::from(cmap.glyph_id(component).unwrap_or(0));
                let glyph = Glyph::new(gid, cluster as u32);
                glyphs.push(glyph);
                codepoints.push(component);
            }
            continue;
        }
        // Tamil and Sinhala split-matra decomposition. These matras
        // decompose into a pre-base + post-base (occasionally
        // three-part) sequence. HarfBuzz's Indic shaper runs this
        // before syllable reordering so the pre-base half can be
        // picked up by the positional-category reorder. sigilbuzz
        // does it at codepoint push time — same entry point as Khmer
        // — so downstream passes never see the composed form.
        if let Some(parts) = crate::ot::indic::split_matra_decompose(ch) {
            for &component in parts {
                let gid = u32::from(cmap.glyph_id(component).unwrap_or(0));
                let glyph = Glyph::new(gid, cluster as u32);
                glyphs.push(glyph);
                codepoints.push(component);
            }
            // Script detection for the segmenter below runs off the
            // `codepoints` vec (not the original text), so pushing
            // the decomposed components is all we need. The components
            // keep their parent's script (Tamil / Sinhala) because
            // they come from the same Unicode block.
            continue;
        }
        let glyph_id = if is_default_ignorable(ch) {
            space_gid
        } else {
            u32::from(cmap.glyph_id(ch).unwrap_or(0))
        };
        // `unicode_props` is set once here and survives ligation /
        // multiple-sub / final-reorder untouched. The Indic shaper
        // relies on this to distinguish a ZWJ-triggered reph mode
        // from an implicit one without re-scanning the text.
        let mut props: u16 = 0;
        if is_default_ignorable(ch) {
            props |= unicode_prop::DEFAULT_IGNORABLE;
        }
        if ch == '\u{200D}' {
            props |= unicode_prop::JOINER;
        } else if ch == '\u{200C}' {
            props |= unicode_prop::NON_JOINER;
        }
        let mut glyph = Glyph::new(glyph_id, cluster as u32);
        glyph.unicode_props = props;
        glyphs.push(glyph);
        codepoints.push(ch);
    }

    // Step 1.5: Segment the run into maximal same-script spans. Each
    // segment carries its own script priority (e.g. Arabic `arab` ->
    // DFLT, Hebrew `hebr` -> DFLT), its codepoint range in the
    // `codepoints` vec we just filled, and — after we finish GSUB
    // below — its post-substitution glyph range. Pre-GSUB the two
    // ranges coincide because cmap is 1:1 (Khmer's split-vowel
    // preprocessor above added both codepoints and glyphs in lockstep,
    // so the 1:1 invariant still holds here).
    //
    // Running each segment through its own cmap → pre-shaper → GSUB
    // → GPOS chain is what lets mixed-script runs like `Hi שלום`
    // dispatch the Hebrew half under `hebr` features and the Latin
    // half under DFLT in a single call. The pre-segmenter implementation
    // resolved one global priority and missed script-specific lookups
    // on whichever half lost the tie-break.
    let segments = build_segments(&codepoints);

    // Dominant script — the first non-COMMON/INHERITED script in the
    // buffer. HarfBuzz (and rustybuzz) compute this once in
    // `guess_segment_properties` and use it to select a single shaper
    // for the whole run; features the shaper activates only fire when
    // the buffer's dominant script matches. sigilbuzz's per-segment
    // dispatch still runs each segment under its own script priority
    // (Hebrew half under `hebr`, Latin half under DFLT), but the
    // complex-shaper pre-pass for Old Hangul needs the dominant-script
    // gate to match HarfBuzz: a mixed `Hi 가` run hands `ljmo`/`vjmo`
    // the jamo segment under HarfBuzz's default shaper (no positional
    // variant forms picked), not the Hangul shaper. Gating the Jamo
    // pre-pass on dominant-script is the smallest knob that keeps
    // parity clean on pure Hangul runs while matching HarfBuzz on
    // Latin-majority mixed runs.
    let dominant_script: Option<Script> = codepoints
        .iter()
        .copied()
        .find(|&c| !is_common_for_segmentation(c))
        .map(script_of);

    let gsub = face.gsub()?;
    // GDEF is consulted up-front so the LookupFlag skip-iterator has
    // it available for every GSUB context match. GPOS reuses the same
    // handle further down.
    let gdef = face.gdef()?;

    // Arabic joining forms are computed once, from the full text,
    // because the state machine depends on surrounding letters (the
    // previous/next Arabic joining-type). A segment-local view would
    // lose the cross-boundary context — but in sigilbuzz every Arabic
    // segment is bounded by non-Arabic neighbours anyway, so global
    // computation is both correct and cheaper than recomputing per
    // segment.
    let has_arabic = codepoints.iter().any(|&c| script_of(c) == Script::Arabic);
    let arabic_forms: Vec<JoiningForm> = if has_arabic {
        assign_joining_forms(text)
    } else {
        Vec::new()
    };

    // Step 2 (per segment): pre-shaper → GSUB. We build the result by
    // concatenating per-segment processed glyph sub-vecs; each segment
    // processes its own slice of codepoints/glyphs so contextual
    // lookups in one script can never see the other script's glyphs
    // as context. Track the post-GSUB glyph range for each segment so
    // the downstream GPOS pass can dispatch under the same priority.
    let mut processed_glyphs: Vec<Glyph> = Vec::with_capacity(glyphs.len());
    let mut seg_glyph_ranges: Vec<ProcessedSegment> = Vec::with_capacity(segments.len());

    for seg in &segments {
        // Take an owned sub-vec of this segment's glyphs so ligature
        // substitution can shrink or multiple-sub can grow the slice
        // without touching the rest of the run.
        let seg_glyphs_src = glyphs[seg.cp_range.clone()].to_vec();
        let seg_cps = &codepoints[seg.cp_range.clone()];
        let mut seg_glyphs = seg_glyphs_src;

        // Per-script pre-shapers. Each is gated on the segment's
        // resolved script so a Hebrew segment never runs the Indic
        // state machine, and vice versa.
        if let Some(config) = crate::ot::indic::indic_config_for(seg.script) {
            crate::ot::indic::shape_indic(
                gsub.as_ref(),
                gdef.as_ref(),
                seg_cps,
                &mut seg_glyphs,
                &config,
            );
        }
        if seg.script == Script::Khmer {
            crate::ot::use_shaper::shape_khmer(
                gsub.as_ref(),
                gdef.as_ref(),
                seg_cps,
                &mut seg_glyphs,
            );
        }
        if seg.script == Script::Tibetan && dominant_script == Some(Script::Tibetan) {
            crate::ot::tibetan::shape_tibetan(
                gsub.as_ref(),
                gdef.as_ref(),
                seg_cps,
                &mut seg_glyphs,
            );
        }
        if seg.script == Script::Mongolian && dominant_script == Some(Script::Mongolian) {
            crate::ot::mongolian::shape_mongolian(
                gsub.as_ref(),
                gdef.as_ref(),
                seg_cps,
                &mut seg_glyphs,
            );
        }
        if seg.script == Script::Myanmar {
            crate::ot::use_shaper::shape_myanmar(
                gsub.as_ref(),
                gdef.as_ref(),
                seg_cps,
                &mut seg_glyphs,
            );
        }
        if seg.script == Script::Thai {
            crate::ot::use_shaper::shape_thai(
                gsub.as_ref(),
                gdef.as_ref(),
                seg_cps,
                &mut seg_glyphs,
            );
        }
        if seg.script == Script::Lao {
            crate::ot::use_shaper::shape_lao(
                gsub.as_ref(),
                gdef.as_ref(),
                seg_cps,
                &mut seg_glyphs,
            );
        }
        if seg.script == Script::NKo {
            crate::ot::use_shaper::shape_nko(
                gsub.as_ref(),
                gdef.as_ref(),
                seg_cps,
                &mut seg_glyphs,
            );
        }
        if seg.script == Script::Buginese {
            crate::ot::use_shaper::shape_buginese(
                gsub.as_ref(),
                gdef.as_ref(),
                seg_cps,
                &mut seg_glyphs,
            );
        }
        if seg.script == Script::TaiTham {
            crate::ot::use_shaper::shape_tai_tham(
                gsub.as_ref(),
                gdef.as_ref(),
                seg_cps,
                &mut seg_glyphs,
            );
        }
        if seg.script == Script::Balinese {
            crate::ot::use_shaper::shape_balinese(
                gsub.as_ref(),
                gdef.as_ref(),
                seg_cps,
                &mut seg_glyphs,
            );
        }
        if seg.script == Script::Sundanese {
            crate::ot::use_shaper::shape_sundanese(
                gsub.as_ref(),
                gdef.as_ref(),
                seg_cps,
                &mut seg_glyphs,
            );
        }
        if seg.script == Script::Lepcha {
            crate::ot::use_shaper::shape_lepcha(
                gsub.as_ref(),
                gdef.as_ref(),
                seg_cps,
                &mut seg_glyphs,
            );
        }
        if seg.script == Script::Limbu {
            crate::ot::use_shaper::shape_limbu(
                gsub.as_ref(),
                gdef.as_ref(),
                seg_cps,
                &mut seg_glyphs,
            );
        }
        if seg.script == Script::Cham {
            crate::ot::use_shaper::shape_cham(
                gsub.as_ref(),
                gdef.as_ref(),
                seg_cps,
                &mut seg_glyphs,
            );
        }
        if seg.script == Script::Brahmi {
            crate::ot::use_shaper::shape_brahmi(
                gsub.as_ref(),
                gdef.as_ref(),
                seg_cps,
                &mut seg_glyphs,
            );
        }
        if seg.script == Script::Sharada {
            crate::ot::use_shaper::shape_sharada(
                gsub.as_ref(),
                gdef.as_ref(),
                seg_cps,
                &mut seg_glyphs,
            );
        }
        if seg.script == Script::Khojki {
            crate::ot::use_shaper::shape_khojki(
                gsub.as_ref(),
                gdef.as_ref(),
                seg_cps,
                &mut seg_glyphs,
            );
        }
        if seg.script == Script::Tirhuta {
            crate::ot::use_shaper::shape_tirhuta(
                gsub.as_ref(),
                gdef.as_ref(),
                seg_cps,
                &mut seg_glyphs,
            );
        }
        if seg.script == Script::Modi {
            crate::ot::use_shaper::shape_modi(
                gsub.as_ref(),
                gdef.as_ref(),
                seg_cps,
                &mut seg_glyphs,
            );
        }
        // Hangul routes through USE only for Jamo-decomposed text.
        // Precomposed syllables (U+AC00..U+D7A3) still pass through
        // the default GSUB/GPOS chain — `ljmo`/`vjmo`/`tjmo` are
        // no-ops on them, so running the pipeline is harmless but
        // wasteful. Additionally gate on the buffer's dominant
        // script: HarfBuzz picks one shaper for the whole run based
        // on the first non-COMMON script, so a Latin-majority mix
        // like `Hi \u{1100}\u{1161}` shapes the jamo under the
        // default shaper (no positional variant forms). sigilbuzz
        // matches that here so mixed runs round-trip glyph-for-glyph.
        if seg.script == Script::Hangul
            && dominant_script == Some(Script::Hangul)
            && seg_cps.iter().any(|&c| crate::unicode::is_hangul_jamo(c))
        {
            crate::ot::use_shaper::shape_hangul(
                gsub.as_ref(),
                gdef.as_ref(),
                seg_cps,
                &mut seg_glyphs,
            );
        }

        if let Some(ref gsub) = gsub {
            // Arabic positional + default GSUB for this segment.
            let seg_arabic_active = seg.script == Script::Arabic && !arabic_forms.is_empty();
            if seg_arabic_active {
                // ccmp must run before positional features so any
                // composition/decomposition has settled first.
                if !feature_disabled(features, *b"ccmp") {
                    apply_gsub_feature(
                        gsub,
                        &mut seg_glyphs,
                        gdef.as_ref(),
                        *b"ccmp",
                        0,
                        seg.script_priority,
                    );
                }
                // Arabic positional pass consumes only the segment's
                // slice of the forms vector — cps/glyphs are 1:1 at
                // this point (ccmp can rewrite ids but not lengths in
                // practice for Arabic), so the slice aligns.
                let forms_slice = &arabic_forms[seg.cp_range.clone()];
                apply_arabic_positional_features(gsub, &mut seg_glyphs, gdef.as_ref(), forms_slice);
            }
            run_default_gsub(
                gsub,
                &mut seg_glyphs,
                gdef.as_ref(),
                features,
                want_liga,
                is_vertical,
                seg.script_priority,
                seg_arabic_active,
            );
        }

        let start = processed_glyphs.len();
        processed_glyphs.extend(seg_glyphs);
        let end = processed_glyphs.len();
        seg_glyph_ranges.push(ProcessedSegment {
            range: start..end,
            script_priority: seg.script_priority,
        });
    }

    // Reassemble — segments were concatenated in left-to-right order
    // so the buffer's visual ordering survives the round-trip.
    glyphs = processed_glyphs;

    // AAT fallback. Consulted only when the font has no GSUB at all
    // — that is how HarfBuzz decides between OpenType and AAT, and
    // matches the issue scope. Legacy macOS Zapfino, older Apple
    // Chancery variants, and most third-party AAT-only fonts land
    // here. Runs after the segmented GSUB pass so a font that
    // carries both only exercises the AAT path when the OpenType
    // side is absent.
    if gsub.is_none() {
        if let Some(morx) = face.morx()? {
            apply_morx(&morx, &mut glyphs);
        }
    }

    // Step 3: advance lookup. Runs *after* GSUB so ligatures receive
    // their ligature-glyph advance, not the sum of component advances.
    // Horizontal layout pulls from hmtx and drives the pen along X;
    // vertical layout pulls from vmtx (when present) and drives the
    // pen along Y, while x_advance stays zero so the glyphs stack
    // rather than walk right. In the horizontal path, a non-empty
    // Font coord slice combined with an HVAR table adjusts each
    // advance by the per-coord delta.
    //
    // Default-ignorable format characters (ZWJ, ZWNJ, bidi marks)
    // keep a zero advance in either axis — they rendered as space
    // earlier, but must not move the pen (HarfBuzz does the same).
    // We walk the character stream once to collect their cluster
    // offsets and then skip them in the advance pass.
    let default_ignorable_clusters: Vec<u32> = text
        .char_indices()
        .filter(|(_, c)| is_default_ignorable(*c))
        .map(|(i, _)| i as u32)
        .collect();
    if is_vertical {
        if let Some(ref vmtx) = vmtx {
            for glyph in &mut glyphs {
                if default_ignorable_clusters.contains(&glyph.cluster) {
                    continue;
                }
                let id = glyph.glyph_id as u16;
                // HarfBuzz convention: vertical y_advance is negative
                // for top-to-bottom flow, so the pen moves downward.
                let raw = i32::from(vmtx.advance(id).unwrap_or(0));
                glyph.y_advance = if buffer.direction().is_forward() {
                    -raw
                } else {
                    raw
                };
                glyph.x_advance = 0;
            }
        } else {
            // No vmtx: fall back to an em-square advance so the run
            // still stacks deterministically. Use the hhea-reported
            // line height as a reasonable default.
            let hhea = face.hhea()?;
            let fallback = (hhea.ascent as i32) - (hhea.descent as i32);
            for glyph in &mut glyphs {
                if default_ignorable_clusters.contains(&glyph.cluster) {
                    continue;
                }
                glyph.y_advance = if buffer.direction().is_forward() {
                    -fallback
                } else {
                    fallback
                };
                glyph.x_advance = 0;
            }
        }
    } else {
        let coords = font.coords();
        let hvar = if coords.is_empty() {
            None
        } else {
            face.hvar()?
        };
        for glyph in &mut glyphs {
            if default_ignorable_clusters.contains(&glyph.cluster) {
                glyph.x_advance = 0;
                continue;
            }
            let id = glyph.glyph_id as u16;
            let base = i32::from(hmtx.advance(id).unwrap_or(0));
            glyph.x_advance = if let Some(ref hvar) = hvar {
                let delta = hvar.advance_delta(id, coords);
                // Round-to-nearest without pulling in libm — the
                // delta arithmetic is small, so add-0.5 / subtract-0.5
                // suffices.
                let rounded = if delta >= 0.0 {
                    (delta + 0.5) as i32
                } else {
                    (delta - 0.5) as i32
                };
                base.saturating_add(rounded)
            } else {
                base
            };
        }
    }

    // Mark-width zeroing — phase 1 (early). The USE shaper and the
    // Myanmar shaper both ship with
    // `zero_width_marks = HB_OT_SHAPE_ZERO_WIDTH_MARKS_BY_GDEF_EARLY`
    // in HarfBuzz, so before the GPOS pass every glyph that GDEF
    // tags as a mark gets its advance forced to zero. A subsequent
    // chained-context kern (e.g. Noto Sans Limbu's `kern` lookup
    // that adds a back-positioning advance to specific mark glyphs)
    // then writes the *delta* onto the cleared advance, not on top
    // of the hmtx default — so the final value matches rustybuzz.
    // Without this pass sigilbuzz double-applies the hmtx advance
    // for any mark that participates in a GPOS chained-context
    // lookup, which is the 0.7.0 Limbu mark-advance follow-up.
    //
    // HarfBuzz selects exactly one shaper per buffer based on the
    // first non-common codepoint, so the gate is on the dominant
    // script — a Latin-majority `Hi <limbu>` run uses the default
    // shaper (late-zero) on every segment, including the Limbu one.
    let dominant_zeroes_early = dominant_script.is_some_and(zeroes_marks_early);
    if dominant_zeroes_early {
        if let Some(ref gdef) = gdef {
            for seg_out in &seg_glyph_ranges {
                for glyph in &mut glyphs[seg_out.range.clone()] {
                    if gdef.glyph_class(glyph.glyph_id as u16).is_mark() {
                        glyph.x_advance = 0;
                    }
                }
            }
        }
    }

    // Step 4: GPOS passes — per segment, so each segment dispatches
    // under its own script-tag priority. Kern first, then `dist`
    // (pre-mark), mark, mkmk, then user-enabled GPOS features. A GPOS
    // kern hit on *any* segment inhibits the legacy-kern fallback —
    // matches the spec: the modern table wins whenever it carries any
    // usable data for the run.
    let gpos = face.gpos()?;
    // Build the variable-font resolution context once. Passing this
    // through every GPOS apply site is what lets VariationIndex
    // deltas inside a ValueRecord actually respond to the user's
    // axis coords — before PR #13 the deltas were read, parsed, and
    // thrown away, so kerning was frozen at the default instance.
    let var = VarCtx {
        coords: font.coords(),
        store: gdef.as_ref().and_then(|g| g.item_variation_store()),
    };
    let mut gpos_kerned = false;
    if let Some(ref gpos) = gpos {
        for seg_out in &seg_glyph_ranges {
            if seg_out.range.is_empty() {
                continue;
            }
            let priority = seg_out.script_priority;
            let seg_slice = &mut glyphs[seg_out.range.clone()];
            if want_kern {
                let ran =
                    apply_gpos_feature(gpos, seg_slice, gdef.as_ref(), *b"kern", priority, &var);
                if ran {
                    gpos_kerned = true;
                }
            }
            // `dist` — distance adjustments the Indic / USE shapers
            // rely on for conjunct forms. Keyed on the segment's
            // resolved script priority (Devanagari → dev2/deva/DFLT,
            // Khmer → khmr/khm2/DFLT, etc.), and skipped for scripts
            // that never ship a `dist` feature.
            if !feature_disabled(features, *b"dist") {
                apply_gpos_feature_in_scripts_with_var(
                    gpos,
                    seg_slice,
                    gdef.as_ref(),
                    *b"dist",
                    priority,
                    &var,
                );
            }
            if !feature_disabled(features, *b"mark") {
                apply_gpos_feature(gpos, seg_slice, gdef.as_ref(), *b"mark", priority, &var);
            }
            if !feature_disabled(features, *b"mkmk") {
                apply_gpos_feature(gpos, seg_slice, gdef.as_ref(), *b"mkmk", priority, &var);
            }
            // User-enabled features beyond the defaults flow through
            // the same dispatch. Skip tags already handled above so
            // they do not double-apply.
            for feat in features {
                if feat.value == 0 {
                    continue;
                }
                if matches!(&feat.tag, b"kern" | b"mark" | b"mkmk" | b"liga") {
                    continue;
                }
                apply_gpos_feature(gpos, seg_slice, gdef.as_ref(), feat.tag, priority, &var);
            }
        }
    }

    // Legacy `kern` is a fallback: only runs when GPOS kern produced
    // no lookups on any segment. GPOS wins even with zero-delta hits
    // — the spec's design, not a sigilbuzz quirk.
    let mut legacy_kerned = false;
    if want_kern && !gpos_kerned {
        if let Some(kern) = face.kern()? {
            apply_legacy_kern(&kern, &mut glyphs);
            legacy_kerned = true;
        }
    }
    // AAT `kerx` is the last-resort fallback: GPOS kern absent AND
    // legacy `kern` absent. In practice fonts ship one of the three,
    // not several; keeping legacy ahead preserves existing
    // behaviour and matches HarfBuzz ordering.
    if want_kern && !gpos_kerned && !legacy_kerned {
        if let Some(kerx) = face.kerx()? {
            apply_kerx(&kerx, &mut glyphs);
        }
    }

    // Mark-width zeroing — phase 2 (late). HarfBuzz's default,
    // Arabic, Hebrew, Thai and Lao shapers ship
    // `zero_width_marks = HB_OT_SHAPE_ZERO_WIDTH_MARKS_BY_GDEF_LATE`:
    // mark advances are zeroed *after* GPOS, so any chained-context
    // kern that wrote a positioning delta on top of the hmtx default
    // is preserved while the underlying mark advance still ends at
    // zero. The gate is on the dominant script — a `Hi <limbu>`
    // mixed run with a Latin majority takes this late-zero path on
    // every segment, including Limbu.
    //
    // Indic / Khmer / Hangul / Myanmar are excluded — their shapers
    // ship `HB_OT_SHAPE_ZERO_WIDTH_MARKS_NONE`, so post-base matras
    // keep their hmtx advance through the entire pipeline (they are
    // bases dressed up as marks for OpenType GDEF reasons).
    let dominant_zeroes_late = dominant_script.is_some_and(zeroes_marks_late);
    if dominant_zeroes_late {
        if let Some(ref gdef) = gdef {
            for seg_out in &seg_glyph_ranges {
                for glyph in &mut glyphs[seg_out.range.clone()] {
                    if gdef.glyph_class(glyph.glyph_id as u16).is_mark() {
                        glyph.x_advance = 0;
                    }
                }
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

/// HarfBuzz "zero mark widths early" set — the dominant scripts whose
/// shaper sets `zero_width_marks = HB_OT_SHAPE_ZERO_WIDTH_MARKS_BY_GDEF_EARLY`
/// (USE shaper + Myanmar). Used to gate the early-zero pass that fires
/// before GPOS, so chained-context kern lookups that write a mark's
/// repositioning advance write a *delta* onto a cleared baseline rather
/// than doubling the hmtx default.
fn zeroes_marks_early(script: Script) -> bool {
    matches!(
        script,
        // USE-routed scripts (HarfBuzz's USE shaper, EARLY).
        Script::NKo
            | Script::Buginese
            | Script::TaiTham
            | Script::Balinese
            | Script::Sundanese
            | Script::Lepcha
            | Script::Limbu
            | Script::Cham
            | Script::Mongolian
            // Myanmar shaper, also EARLY.
            | Script::Myanmar
    )
}

/// HarfBuzz "zero mark widths late" set — dominant scripts whose
/// shaper sets `zero_width_marks = HB_OT_SHAPE_ZERO_WIDTH_MARKS_BY_GDEF_LATE`
/// (default / Arabic / Hebrew / Thai / Lao). The dominant-script gate
/// reads this when deciding whether to zero mark advances after GPOS.
/// Indic / Khmer / Hangul are explicitly excluded — their shapers ship
/// `HB_OT_SHAPE_ZERO_WIDTH_MARKS_NONE`, so a post-base matra (a "mark"
/// in GDEF terms but a base in shaping terms) must retain its hmtx
/// advance through the entire pipeline.
fn zeroes_marks_late(script: Script) -> bool {
    matches!(
        script,
        // Arabic + Arabic-joining family (HarfBuzz Arabic shaper).
        Script::Arabic
            | Script::Hebrew
            // Thai/Lao shapers ship LATE explicitly.
            | Script::Thai
            | Script::Lao
            // Latin / common / "Other" all reach the default shaper,
            // which is LATE.
            | Script::Latin
            | Script::Other
    )
}

/// One shape-time segment: a maximal run of codepoints that share a
/// resolved script. `cp_range` is a half-open range into the
/// post-cmap `codepoints` vector (not into the buffer text, because
/// Khmer split-vowel preprocessing can insert synthetic codepoints).
/// Pre-GSUB, `glyphs[cp_range]` covers exactly the same glyphs.
#[derive(Debug)]
struct Segment {
    cp_range: core::ops::Range<usize>,
    script: Script,
    script_priority: &'static [[u8; 4]],
}

/// Post-GSUB slice of the fully-assembled `glyphs` vector — one per
/// pre-shaped segment. The GPOS loop reads this back to dispatch
/// kern / mark / mkmk / dist / user-enabled features against the
/// correct script tag priority for each slice.
#[derive(Debug)]
struct ProcessedSegment {
    range: core::ops::Range<usize>,
    script_priority: &'static [[u8; 4]],
}

/// Splits the post-cmap codepoint stream into [`Segment`]s whose
/// scripts agree with the buffer-level [`crate::buffer::Buffer::script_runs`]
/// segmentation: COMMON codepoints (ASCII space/digits/punctuation,
/// ZWJ/ZWNJ/bidi marks) extend whichever real-script segment ran
/// before them, and a leading COMMON-only run takes the raw script
/// of its first codepoint (typically `Script::Latin` via the ASCII
/// table). Always returns at least one segment covering the whole
/// `codepoints` range for a non-empty input.
fn build_segments(codepoints: &[char]) -> Vec<Segment> {
    let mut segments: Vec<Segment> = Vec::new();
    if codepoints.is_empty() {
        return segments;
    }
    let mut current_start = 0usize;
    let mut current_script: Option<Script> = None;
    for (i, &ch) in codepoints.iter().enumerate() {
        let raw = script_of(ch);
        let resolved = if is_common_for_segmentation(ch) {
            current_script.unwrap_or(raw)
        } else {
            raw
        };
        match current_script {
            Some(s) if s == resolved => {}
            Some(s) => {
                segments.push(Segment {
                    cp_range: current_start..i,
                    script: s,
                    script_priority: script_priority_for(s),
                });
                current_start = i;
                current_script = Some(resolved);
            }
            None => {
                current_script = Some(resolved);
            }
        }
    }
    if let Some(s) = current_script {
        segments.push(Segment {
            cp_range: current_start..codepoints.len(),
            script: s,
            script_priority: script_priority_for(s),
        });
    }
    segments
}

/// Shape-time COMMON / INHERITED predicate: stays in lockstep with
/// the buffer-level `is_common_or_inherited` in `buffer.rs`. Kept
/// inside `shape.rs` so the Khmer-split synthetic codepoints — which
/// never land in the buffer's text — still segment correctly.
const fn is_common_for_segmentation(ch: char) -> bool {
    let cp = ch as u32;
    matches!(
        cp,
        0x0000..=0x002F
        | 0x0030..=0x0040
        | 0x005B..=0x0060
        | 0x007B..=0x007F
        | 0x00A0..=0x00BF
        | 0x200C | 0x200D | 0x200E | 0x200F | 0x061C
        // INHERITED combining-mark blocks — must extend the preceding
        // real-script segment so GSUB dispatches under the right
        // priority. Matches `buffer::is_common_or_inherited`.
        | 0x0300..=0x036F
        | 0x1DC0..=0x1DFF
        | 0x20D0..=0x20FF
        | 0xFE20..=0xFE2F
    )
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
        | 0x061C // ARABIC LETTER MARK
    )
}

/// Runs the default GSUB feature chain and any user-enabled extras.
/// Order matches the spec: `ccmp` → `rlig` → `liga` → `clig` →
/// `calt`, then `vrt2` / `vert` for vertical runs. HarfBuzz's Latin
/// fallback shaper turns the horizontal list on by default;
/// sigilbuzz follows suit. User-enabled features beyond that list
/// are dispatched afterwards, respecting their 1-indexed
/// alternate-selector value.
#[allow(clippy::too_many_arguments)]
fn run_default_gsub(
    gsub: &Gsub<'_>,
    glyphs: &mut Vec<Glyph>,
    gdef: Option<&Gdef<'_>>,
    features: &[Feature],
    want_liga: bool,
    is_vertical: bool,
    script_priority: &[[u8; 4]],
    arabic_positional_already_ran: bool,
) {
    // ccmp already ran before the Arabic positional pass; avoid
    // double-applying it here.
    if !arabic_positional_already_ran && !feature_disabled(features, *b"ccmp") {
        apply_gsub_feature(gsub, glyphs, gdef, *b"ccmp", 0, script_priority);
    }
    if !feature_disabled(features, *b"rlig") {
        apply_gsub_feature(gsub, glyphs, gdef, *b"rlig", 0, script_priority);
    }
    if want_liga {
        apply_gsub_feature(gsub, glyphs, gdef, *b"liga", 0, script_priority);
    }
    if !feature_disabled(features, *b"clig") {
        apply_gsub_feature(gsub, glyphs, gdef, *b"clig", 0, script_priority);
    }
    // `calt` and `rclt` together: HarfBuzz's default horizontal
    // feature list enables both, and Mongolian fonts in particular
    // ship the same lookup set under both tags (calt for legacy,
    // rclt for required-contextual). Naively running each tag's
    // lookups in turn double-applies on those fonts. Mirror
    // HarfBuzz's "each lookup runs once per pass" rule by collecting
    // both lookup index lists, deduplicating, and applying the
    // union in ascending lookup-index order — the same order the
    // GSUB FeatureList walks them.
    if !feature_disabled(features, *b"calt") || !feature_disabled(features, *b"rclt") {
        let mut indices: Vec<u16> = Vec::new();
        if !feature_disabled(features, *b"calt") {
            if let Some(idxs) =
                lookup_indices_for_feature_in_scripts(gsub, *b"calt", script_priority)
            {
                indices.extend(idxs);
            }
        }
        if !feature_disabled(features, *b"rclt") {
            if let Some(idxs) =
                lookup_indices_for_feature_in_scripts(gsub, *b"rclt", script_priority)
            {
                indices.extend(idxs);
            }
        }
        // Sort + dedup so each lookup is applied at most once and in
        // ascending index order (matching HarfBuzz's pass walk).
        indices.sort_unstable();
        indices.dedup();
        for lookup_idx in indices {
            apply_gsub_lookup(gsub, lookup_idx, glyphs, gdef, 0);
        }
    }
    // Vertical writing: HarfBuzz auto-enables `vrt2` when the font
    // carries it, otherwise falls back to `vert`. The two tags
    // cannot be active together — `vrt2` (Vertical Alternates &
    // Rotation) is the superset, so prefer it.
    if is_vertical {
        let has_vrt2 = feature_present(gsub, *b"vrt2");
        if has_vrt2 && !feature_disabled(features, *b"vrt2") {
            apply_gsub_feature(gsub, glyphs, gdef, *b"vrt2", 0, script_priority);
        } else if !feature_disabled(features, *b"vert") {
            apply_gsub_feature(gsub, glyphs, gdef, *b"vert", 0, script_priority);
        }
    }
    for feat in features {
        if feat.value == 0 {
            continue;
        }
        if is_handled_gsub_tag(feat.tag) {
            continue;
        }
        let alternate_idx = (feat.value.saturating_sub(1)).min(u32::from(u16::MAX)) as u16;
        apply_gsub_feature(gsub, glyphs, gdef, feat.tag, alternate_idx, script_priority);
    }
}

/// GSUB feature tags that `shape()` already dispatches by name,
/// so the user-override walk should skip them rather than
/// double-apply.
fn is_handled_gsub_tag(tag: [u8; 4]) -> bool {
    matches!(
        &tag,
        b"liga" | b"kern" | b"ccmp" | b"rlig" | b"clig" | b"calt" | b"rclt" | b"vert" | b"vrt2"
    )
}

/// Returns `true` when the GSUB default-LangSys advertises the named
/// feature tag. Used by the vertical-writing dispatcher to decide
/// between `vrt2` (preferred if present) and `vert` (fallback).
fn feature_present(gsub: &Gsub<'_>, tag: [u8; 4]) -> bool {
    // Vertical-writing probe runs before we know the script — use the
    // Latin-style script order (DFLT → first) to match the previous
    // behaviour. Arabic fonts do not ship vert/vrt2, so this choice is
    // not observable in practice.
    lookup_indices_for_feature_in_scripts(gsub, tag, &[*b"DFLT"]).is_some_and(|v| !v.is_empty())
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
pub(crate) fn apply_gsub_feature(
    gsub: &Gsub<'_>,
    glyphs: &mut Vec<Glyph>,
    gdef: Option<&Gdef<'_>>,
    tag: [u8; 4],
    alternate_index: u16,
    script_priority: &[[u8; 4]],
) {
    // Callers resolve the priority from run analysis: `arab > DFLT`
    // for Arabic, `hebr > DFLT` for Hebrew, `dev2 > deva > DFLT` for
    // Devanagari, plain `DFLT` otherwise. The script-priority walker
    // inside `apply_gsub_feature_in_scripts` falls back when the
    // preferred script does not carry the feature, so mixed-script
    // runs still find the lookup under DFLT.
    apply_gsub_feature_in_scripts(gsub, glyphs, gdef, tag, alternate_index, script_priority);
}

/// Same as [`apply_gsub_feature`] but walks the supplied script-tag
/// priority list instead of just DFLT. The Indic shaper needs this
/// because Devanagari fonts expose their reordering features under
/// `deva`/`dev2` and leave DFLT with only the "universal" subset.
///
/// Falls back to the first script in the list if none of the
/// requested tags are present, matching the prior DFLT-fallback
/// behaviour.
pub(crate) fn apply_gsub_feature_in_scripts(
    gsub: &Gsub<'_>,
    glyphs: &mut Vec<Glyph>,
    gdef: Option<&Gdef<'_>>,
    tag: [u8; 4],
    alternate_index: u16,
    script_priority: &[[u8; 4]],
) {
    if glyphs.is_empty() {
        return;
    }

    let Some(lookup_indices) = lookup_indices_for_feature_in_scripts(gsub, tag, script_priority)
    else {
        return;
    };
    if lookup_indices.is_empty() {
        return;
    }

    for lookup_idx in lookup_indices {
        apply_gsub_lookup(gsub, lookup_idx, glyphs, gdef, alternate_index);
    }
}

/// Applies a single feature's lookups only at glyph positions where
/// `mask[i]` is true. Used by the Indic shaper to gate `half` off
/// on consonants whose post-halant partner is already going to be
/// consumed by `blwf` — mirrors HarfBuzz's per-glyph feature mask
/// machinery at the one spot sigilbuzz currently needs it.
///
/// Shares the masked lookup dispatcher with Arabic
/// positional features; lookups that don't understand the mask
/// (chaining-context interior) fall through to the unmasked
/// dispatcher, matching the behaviour documented on
/// [`apply_gsub_lookup_masked`].
pub(crate) fn apply_gsub_feature_masked(
    gsub: &Gsub<'_>,
    glyphs: &mut Vec<Glyph>,
    gdef: Option<&Gdef<'_>>,
    tag: [u8; 4],
    script_priority: &[[u8; 4]],
    mask: &[bool],
) {
    if glyphs.is_empty() {
        return;
    }
    let Some(lookup_indices) = lookup_indices_for_feature_in_scripts(gsub, tag, script_priority)
    else {
        return;
    };
    if lookup_indices.is_empty() {
        return;
    }
    for lookup_idx in lookup_indices {
        apply_gsub_lookup_masked(gsub, lookup_idx, glyphs, gdef, mask);
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
    gdef: Option<&Gdef<'_>>,
    forms: &[JoiningForm],
) {
    for (form, tag) in [
        (JoiningForm::Isol, *b"isol"),
        (JoiningForm::Init, *b"init"),
        (JoiningForm::Medi, *b"medi"),
        (JoiningForm::Fina, *b"fina"),
    ] {
        let Some(lookup_indices) =
            lookup_indices_for_feature_in_scripts(gsub, tag, &[*b"arab", *b"DFLT"])
        else {
            continue;
        };
        if lookup_indices.is_empty() {
            continue;
        }
        let mask: Vec<bool> = forms.iter().map(|&f| f == form).collect();
        for lookup_idx in lookup_indices {
            apply_gsub_lookup_masked(gsub, lookup_idx, glyphs, gdef, &mask);
        }
    }
}

/// Applies a single GSUB lookup only at positions where `mask[i]`
/// is true. Used by the Arabic positional pass — `isol` at positions
/// tagged `Isol`, `init` at `Init`, and so on, and by the Indic
/// shaper for `half`/`pref`/`pres` gating.
///
/// Per-glyph lookup types (SINGLE / MULTIPLE / ALTERNATE / LIGATURE)
/// only fire when the mask at the cursor position is true.
/// Chained-context lookups inside a positional feature run over the
/// full glyph stream — the rules' coverage already encodes their
/// positional intent.
///
/// Like [`apply_gsub_lookup`], this walks the cursor once and tries
/// the lookup's subtables in spec order, taking the first match.
fn apply_gsub_lookup_masked(
    gsub: &Gsub<'_>,
    lookup_idx: u16,
    glyphs: &mut Vec<Glyph>,
    gdef: Option<&Gdef<'_>>,
    mask: &[bool],
) {
    let lookup_list = gsub.lookup_list();
    let Some(lookup) = lookup_list.get(lookup_idx) else {
        return;
    };
    let raw_lt = lookup.lookup_type();
    let effective_lt = if raw_lt == gsub_lt::EXTENSION {
        lookup
            .subtable_bytes(0)
            .and_then(resolve_extension)
            .map_or(raw_lt, |(inner, _)| inner)
    } else {
        raw_lt
    };

    // Chained-context inside a positional feature ignores the mask:
    // the rule itself encodes positional intent via its input coverage
    // (post-positional glyph ids tagged init/medi/fina/...). Defer to
    // the unmasked driver so the cursor walk + first-subtable-wins
    // semantics still apply.
    if effective_lt == gsub_lt::CHAINED_CONTEXT || effective_lt == gsub_lt::CONTEXT {
        apply_gsub_lookup(gsub, lookup_idx, glyphs, gdef, 0);
        return;
    }

    let parsed = parse_lookup_subtables(&lookup, raw_lt);
    if parsed.is_empty() {
        return;
    }
    let filter = filter_for_lookup(&lookup, gdef);
    let mut ids = GlyphIds::from_glyphs(glyphs);
    let mut i = 0;
    while i < glyphs.len() {
        if !mask.get(i).copied().unwrap_or(false) {
            i += 1;
            continue;
        }
        let consumed =
            apply_parsed_lookup_at(gsub, &parsed, &filter, glyphs, &mut ids, gdef, i, 0, 0);
        if consumed > 0 {
            i += consumed;
        } else {
            i += 1;
        }
    }
}

/// Applies a single GSUB lookup by index. Mirrors HarfBuzz's
/// `apply_forward`: walks the glyph run cursor-by-cursor, and at each
/// cursor tries the lookup's subtables in spec order, taking the
/// first subtable that matches and advancing the cursor past the
/// consumed input window. The previous implementation walked each
/// subtable across the whole run independently — that re-fired later
/// subtables on positions that an earlier one had already matched
/// (with `SubstCount=0`, common in Amiri's `rlig`), producing
/// glyph-id divergences from rustybuzz on Allah / bism-Allah and
/// other Quranic-grade vocalised forms (issue #21).
///
/// Reverse-chained lookups (type 8) iterate right-to-left and are
/// not a per-cursor "first match" thing — they substitute coverage-
/// matched glyphs in place, with each lookup's subtables walking the
/// frozen-prefix snapshot. Detected via lookup type and dispatched
/// separately.
fn apply_gsub_lookup(
    gsub: &Gsub<'_>,
    lookup_idx: u16,
    glyphs: &mut Vec<Glyph>,
    gdef: Option<&Gdef<'_>>,
    alternate_index: u16,
) {
    let lookup_list = gsub.lookup_list();
    let Some(lookup) = lookup_list.get(lookup_idx) else {
        return;
    };
    let raw_lt = lookup.lookup_type();
    let effective_lt = if raw_lt == gsub_lt::EXTENSION {
        // Peek at the first subtable to see what the extension wraps;
        // a lookup's subtables all share a type so this is sufficient.
        lookup
            .subtable_bytes(0)
            .and_then(resolve_extension)
            .map_or(raw_lt, |(inner, _)| inner)
    } else {
        raw_lt
    };

    if effective_lt == gsub_lt::REVERSE_CHAINED {
        // Reverse chain walks right-to-left and is in-place; keep the
        // existing per-subtable driver since cursor semantics differ
        // from forward lookups.
        for sub_idx in 0..lookup.subtable_count() {
            let Some(bytes) = lookup.subtable_bytes(sub_idx) else {
                continue;
            };
            let inner_bytes = if raw_lt == gsub_lt::EXTENSION {
                match resolve_extension(bytes) {
                    Some((_, inner)) => inner,
                    None => continue,
                }
            } else {
                bytes
            };
            let Ok(rc) = ReverseChain::parse(inner_bytes) else {
                continue;
            };
            apply_reverse_chain_subtable(&rc, glyphs);
        }
        return;
    }

    // Pre-parse subtables once so the cursor walk below doesn't
    // re-parse them at every position. ChainContextAny / Context /
    // Ligature parsers each allocate three or four `Vec`s for their
    // coverage / substitution arrays; doing that per cursor on a 80-
    // glyph Devanagari run is what made the bench look like a
    // quadratic explosion.
    let parsed = parse_lookup_subtables(&lookup, raw_lt);
    if parsed.is_empty() {
        return;
    }
    let filter = filter_for_lookup(&lookup, gdef);

    // Build the shadow glyph-id buffer once; the per-subtable
    // matchers read from it and `apply_parsed_lookup_at` keeps it in
    // sync with `glyphs` after each substitution.
    let mut ids = GlyphIds::from_glyphs(glyphs);

    // Run-level "would_apply" precheck. If no glyph in the run can
    // possibly trigger any subtable's primary coverage, the cursor
    // walk has nothing to do — skip it. Saves the per-cursor coverage
    // probe on lookups that target glyph subsets the run never
    // contains (very common: every Indic feature dispatched against
    // a run that doesn't carry that feature's anchor consonants).
    if !lookup_might_apply(&parsed, ids.as_slice()) {
        return;
    }

    // Forward cursor walk: cursor visits only positions whose glyph
    // is in the lookup's primary coverage union — HarfBuzz calls
    // this the "digest" walk. Falls back to visiting every position
    // when at least one subtable's primary coverage isn't a single
    // `Coverage` table (chain-context format 1/2, reverse-chain).
    //
    // At each visited cursor, try every subtable in order; the first
    // one that matches consumes input and the cursor skips past it.
    // A subtable that matches but produces zero substitutions (common
    // in Amiri rlig: a context with `SubstCount=0` is intentionally a
    // "no-op match" that blocks later subtables at this cursor) still
    // advances the cursor by its input length.
    let use_digest = parsed_has_full_digest(&parsed);
    let mut i = 0;
    while i < glyphs.len() {
        if use_digest {
            let id = ids.as_slice()[i];
            if !cursor_in_digest(&parsed, id) {
                i += 1;
                continue;
            }
        }
        let consumed = apply_parsed_lookup_at(
            gsub,
            &parsed,
            &filter,
            glyphs,
            &mut ids,
            gdef,
            i,
            0,
            alternate_index,
        );
        if consumed > 0 {
            i += consumed;
        } else {
            i += 1;
        }
    }
}

/// Applies a nested GSUB lookup at one specific position in the
/// run. Returns the number of glyphs the nested lookup consumed
/// (1 for single substitution, N for ligature, 0 when the lookup
/// did not fire). Called from inside the context/chain-context
/// subtable drivers.
///
/// `depth` is the recursion depth: the caller passes `0` for its
/// first invocation and each recursive edge increments by one; we
/// bail out at [`MAX_NESTED_DEPTH`] so a pathological font loop
/// cannot overflow the stack.
#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
fn apply_gsub_lookup_at(
    gsub: &Gsub<'_>,
    lookup_idx: u16,
    glyphs: &mut Vec<Glyph>,
    ids: &mut GlyphIds,
    gdef: Option<&Gdef<'_>>,
    at: usize,
    depth: u8,
    alternate_index: u16,
) -> usize {
    if depth >= MAX_NESTED_DEPTH {
        return 0;
    }
    if at >= glyphs.len() {
        return 0;
    }
    let lookup_list = gsub.lookup_list();
    let Some(lookup) = lookup_list.get(lookup_idx) else {
        return 0;
    };
    let filter = filter_for_lookup(&lookup, gdef);
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
                if filter.is_skipped(id) {
                    continue;
                }
                if let Some(out) = single.apply(id) {
                    glyphs[at].glyph_id = u32::from(out);
                    ids.set(at, out);
                    return 1;
                }
            }
            gsub_lt::MULTIPLE => {
                let Ok(m) = Multiple::parse(inner_bytes) else {
                    continue;
                };
                let id = glyphs[at].glyph_id as u16;
                if filter.is_skipped(id) {
                    continue;
                }
                if let Some(seq) = m.apply(id) {
                    if let Some(n) = expand_glyph_in_place(glyphs, at, &seq) {
                        ids.resync(glyphs);
                        return n;
                    }
                }
            }
            gsub_lt::ALTERNATE => {
                let Ok(alt) = Alternate::parse(inner_bytes) else {
                    continue;
                };
                let id = glyphs[at].glyph_id as u16;
                if filter.is_skipped(id) {
                    continue;
                }
                if let Some(out) = alt.apply(id, alternate_index) {
                    glyphs[at].glyph_id = u32::from(out);
                    ids.set(at, out);
                    return 1;
                }
            }
            gsub_lt::LIGATURE => {
                let Ok(lig) = Ligature::parse(inner_bytes) else {
                    continue;
                };
                if let Some((out, positions)) = lig.apply_filtered(&ids.as_slice()[at..], &filter) {
                    glyphs[at].glyph_id = u32::from(out);
                    drain_ligature_components(glyphs, at, &positions);
                    let span = positions.last().copied().map_or(0, |p| p + 1);
                    ids.resync(glyphs);
                    return span;
                }
            }
            gsub_lt::CONTEXT => {
                let Ok(ctx) = GsubContext::parse(inner_bytes) else {
                    continue;
                };
                let ran =
                    apply_gsub_context_at(gsub, &ctx, glyphs, ids, gdef, &filter, at, depth + 1);
                if ran > 0 {
                    return ran;
                }
            }
            gsub_lt::CHAINED_CONTEXT => {
                let Ok(chain) = ChainContextAny::parse(inner_bytes) else {
                    continue;
                };
                let ran = apply_gsub_chain_context_at(
                    gsub,
                    &chain,
                    glyphs,
                    ids,
                    gdef,
                    &filter,
                    at,
                    depth + 1,
                );
                if ran > 0 {
                    return ran;
                }
            }
            gsub_lt::REVERSE_CHAINED => {
                let Ok(rc) = ReverseChain::parse(inner_bytes) else {
                    continue;
                };
                if let Some(out) = rc.apply(ids.as_slice(), at) {
                    glyphs[at].glyph_id = u32::from(out);
                    ids.set(at, out);
                    return 1;
                }
            }
            _ => {}
        }
    }
    0
}

/// Collapses a successful ligature match spanning `at..=at+span-1`
/// into a single glyph at `at`, preserving the skipped glyphs
/// (typically marks) that sat between the matched components. The
/// ligature glyph id is assumed to be already written to
/// `glyphs[at]`; this helper only performs the drain.
///
/// `positions` is the relative-offset list returned by
/// [`Ligature::apply_filtered`] — `positions[0] == 0` (the
/// already-consumed first component), and every subsequent entry is
/// the index of a matched component inside `glyphs[at..]`. Any
/// index strictly inside the span that is *not* listed is a skipped
/// glyph and stays in place.
fn drain_ligature_components(glyphs: &mut Vec<Glyph>, at: usize, positions: &[usize]) {
    if positions.len() <= 1 {
        return;
    }
    // Iterate high-to-low so earlier indices stay valid while we
    // remove later ones.
    for rel in positions.iter().skip(1).rev() {
        glyphs.remove(at + rel);
    }
}

/// Nested dispatch for a GSUB contextual subtable at position `at`.
/// Mirrors the chain-context driver but without backtrack/lookahead
/// so the lookup fires on the input window alone.
#[allow(clippy::too_many_arguments)]
fn apply_gsub_context_at(
    gsub: &Gsub<'_>,
    ctx: &GsubContext<'_>,
    glyphs: &mut Vec<Glyph>,
    ids: &mut GlyphIds,
    gdef: Option<&Gdef<'_>>,
    filter: &MatchFilter<'_>,
    at: usize,
    depth: u8,
) -> usize {
    // Match against the shadow `ids` slice — no per-cursor allocation.
    // Records that need to outlive the match call get cloned into a
    // small heap buffer so we can release the borrow on `ids` before
    // dispatching nested lookups (which mutate `ids` via the glyphs
    // it tracks).
    let (input_len, lookups): (usize, Vec<SequenceLookupRecord>) = {
        let id_slice = ids.as_slice();
        match ctx {
            GsubContext::Format1(c) => {
                let Some((n, lks)) = c.matches_filtered(id_slice, at, filter) else {
                    return 0;
                };
                (n, lks.to_vec())
            }
            GsubContext::Format2(c) => {
                let Some((n, lks)) = c.matches_filtered(id_slice, at, filter) else {
                    return 0;
                };
                (n, lks.to_vec())
            }
            GsubContext::Format3(c) => {
                let Some(n) = c.matches_filtered(id_slice, at, filter) else {
                    return 0;
                };
                (n, c.lookups().to_vec())
            }
        }
    };
    apply_nested_gsub_lookups(gsub, glyphs, ids, gdef, filter, at, depth, &lookups);
    input_len.max(1)
}

/// Nested dispatch for a GSUB chained-context subtable at position
/// `at`.
#[allow(clippy::too_many_arguments)]
fn apply_gsub_chain_context_at(
    gsub: &Gsub<'_>,
    chain: &ChainContextAny<'_>,
    glyphs: &mut Vec<Glyph>,
    ids: &mut GlyphIds,
    gdef: Option<&Gdef<'_>>,
    filter: &MatchFilter<'_>,
    at: usize,
    depth: u8,
) -> usize {
    let (input_len, lookups): (usize, Vec<SequenceLookupRecord>) = {
        let id_slice = ids.as_slice();
        match chain {
            ChainContextAny::Format1(c) => {
                let Some((n, lks)) = c.matches_filtered(id_slice, at, filter) else {
                    return 0;
                };
                (n, lks.to_vec())
            }
            ChainContextAny::Format2(c) => {
                let Some((n, lks)) = c.matches_filtered(id_slice, at, filter) else {
                    return 0;
                };
                (n, lks.to_vec())
            }
            ChainContextAny::Format3(c) => {
                let Some(n) = c.matches_filtered(id_slice, at, filter) else {
                    return 0;
                };
                let lks = c
                    .substitutions()
                    .iter()
                    .map(|r| SequenceLookupRecord {
                        sequence_index: r.sequence_index,
                        lookup_list_index: r.lookup_list_index,
                    })
                    .collect();
                (n, lks)
            }
        }
    };
    apply_nested_gsub_lookups(gsub, glyphs, ids, gdef, filter, at, depth, &lookups);
    input_len.max(1)
}

/// Translates a list of sequence-lookup records against the current
/// input window and dispatches each nested lookup at the matching
/// absolute glyph position. `sequence_index` counts *unfiltered*
/// input positions, so we walk the skip-iterator `seq_idx` times
/// from `at` to find the corresponding raw index — marks (or other
/// skipped glyphs) between matched components never appear in the
/// sequence-index space.
#[allow(clippy::too_many_arguments)]
fn apply_nested_gsub_lookups(
    gsub: &Gsub<'_>,
    glyphs: &mut Vec<Glyph>,
    ids: &mut GlyphIds,
    gdef: Option<&Gdef<'_>>,
    filter: &MatchFilter<'_>,
    at: usize,
    depth: u8,
    lookups: &[SequenceLookupRecord],
) {
    for rec in lookups {
        let seq = rec.sequence_index as usize;
        let pos = if seq == 0 {
            at
        } else {
            // Walk `seq` unfiltered steps forward from `at` over the
            // shadow id buffer. Bailing out of the walk has to happen
            // outside the inner loop so we can `return` from the outer
            // function (rather than break out of just the seq walk).
            let id_slice = ids.as_slice();
            let mut cursor = at + 1;
            let mut walked = at;
            let mut found_all = true;
            for _ in 0..seq {
                if let Some(p) = filter.next_unskipped(id_slice, cursor) {
                    walked = p;
                    cursor = p + 1;
                } else {
                    found_all = false;
                    break;
                }
            }
            if !found_all {
                return;
            }
            walked
        };
        // Nested alternate lookups always pick index 0 — feature
        // value-based selection is a top-level concept and does not
        // propagate into a recursed lookup.
        apply_gsub_lookup_at(
            gsub,
            rec.lookup_list_index,
            glyphs,
            ids,
            gdef,
            pos,
            depth,
            0,
        );
    }
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
    // Inherit the source glyph's shaper-internal state so Indic
    // `indic_position` and unicode-property bits survive a
    // multiple-sub split. Rustybuzz does the same via its info mask.
    let source_props = glyphs[at].unicode_props;
    let source_pos = glyphs[at].indic_position;
    glyphs[at].glyph_id = u32::from(seq[0]);
    for (i, &out_gid) in seq.iter().enumerate().skip(1) {
        let mut g = Glyph::new(u32::from(out_gid), source_cluster);
        g.unicode_props = source_props;
        g.indic_position = source_pos;
        glyphs.insert(at + i, g);
    }
    Some(seq.len())
}

/// Reverse chained single substitution (GSUB type 8). Walks the run
/// right-to-left so a match earlier in the run does not see a
/// substituted glyph later in the run (the spec requires this).
fn apply_reverse_chain_subtable(rc: &ReverseChain<'_>, glyphs: &mut [Glyph]) {
    if glyphs.is_empty() {
        return;
    }
    // Snapshot once: type 8 only ever produces one glyph per hit so
    // we can mutate the live stream after computing the substitute
    // against the frozen prefix/suffix. Walking right-to-left means
    // the "input" glyph for position `i` uses the current state of
    // positions < i (untouched so far) and of positions > i (snapshot).
    let mut ids: Vec<u16> = glyphs.iter().map(|g| g.glyph_id as u16).collect();
    for i in (0..glyphs.len()).rev() {
        if let Some(out) = rc.apply(&ids, i) {
            glyphs[i].glyph_id = u32::from(out);
            ids[i] = out;
        }
    }
}

/// Walks the default LangSys (DFLT → first script) and returns the
/// sorted set of lookup indices that the feature `tag` selects. A
/// return value of `None` means no usable script, `Some(empty)`
/// means the LangSys does not carry this feature.
#[allow(dead_code)]
fn lookup_indices_for_feature(gsub: &Gsub<'_>, tag: [u8; 4]) -> Option<Vec<u16>> {
    lookup_indices_for_feature_in_scripts(gsub, tag, &[*b"DFLT"])
}

/// Script-priority variant of [`lookup_indices_for_feature`]. Walks
/// the `script_priority` tags in order and returns lookup indices
/// for the first script that carries the requested feature tag. A
/// script that exists but lacks the feature simply yields an empty
/// lookup list — falls through to the next priority. If none of
/// the priority scripts carry the feature, falls back to DFLT then
/// the first script in the table (matching the generic-path
/// default).
fn lookup_indices_for_feature_in_scripts(
    gsub: &Gsub<'_>,
    tag: [u8; 4],
    script_priority: &[[u8; 4]],
) -> Option<Vec<u16>> {
    let script_list = gsub.script_list();
    let feature_list = gsub.feature_list();

    // Try each requested script in order; the first one that carries
    // the feature wins. Scripts present but lacking this tag still
    // count as "found" and stop the fallback chain — matches how
    // HarfBuzz treats per-script feature overrides.
    for tag_pri in script_priority {
        let Some(script) = script_list.find(*tag_pri) else {
            continue;
        };
        let Some(lang_sys) = script.default_lang_sys() else {
            continue;
        };
        let mut indices: Vec<u16> = Vec::new();
        let mut has_feature = false;
        for feat_idx in lang_sys.feature_indices() {
            let Some((feat_tag, feature)) = feature_list.get(feat_idx) else {
                continue;
            };
            if feat_tag != tag {
                continue;
            }
            has_feature = true;
            for idx in feature.lookup_indices() {
                if !indices.contains(&idx) {
                    indices.push(idx);
                }
            }
        }
        if has_feature {
            indices.sort_unstable();
            return Some(indices);
        }
    }

    // Final fallback: DFLT (if not already tried) then first script.
    let already_tried_dflt = script_priority.iter().any(|t| *t == *b"DFLT");
    let script = if already_tried_dflt {
        script_list.iter().next().map(|(_, s)| s)?
    } else {
        script_list
            .find(*b"DFLT")
            .or_else(|| script_list.iter().next().map(|(_, s)| s))?
    };
    let lang_sys = script.default_lang_sys()?;
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

/// Asks "would feature `tag`'s lookups substitute starting at the
/// head of this glyph sequence?" Used by the Indic shaper's base
/// finder to tag post-halant consonants as below-base (POS_BELOW_C)
/// when the font's `blwf` feature contains a substitution that would
/// consume `virama + consonant` (new-spec) or `consonant + virama`
/// (old-spec) — mirrors HarfBuzz's `consonant_position_from_face`.
///
/// The implementation is a dry-run: copy the candidate glyph slice
/// into a throw-away buffer, run the feature's lookups over it, and
/// report whether any glyph id changed or any glyph was removed.
/// That handles ligature subtables, chaining contexts whose nested
/// lookups ligate, and single subtables uniformly. It is considerably
/// more expensive than poking at individual subtable types, but we
/// only call it per-syllable during Indic initial reordering, so the
/// cost is bounded.
pub(crate) fn feature_would_substitute(
    gsub: &Gsub<'_>,
    gdef: Option<&Gdef<'_>>,
    tag: [u8; 4],
    script_priority: &[[u8; 4]],
    glyph_ids: &[u16],
) -> bool {
    if glyph_ids.is_empty() {
        return false;
    }
    // Build a throw-away glyph slice — cluster values don't matter,
    // only glyph ids survive the dry run. Start clusters at 0 so a
    // ligature merge collapses them to 0 deterministically.
    let mut scratch: Vec<Glyph> = glyph_ids
        .iter()
        .map(|&id| Glyph::new(u32::from(id), 0))
        .collect();
    let before: Vec<u32> = scratch.iter().map(|g| g.glyph_id).collect();
    apply_gsub_feature_in_scripts(gsub, &mut scratch, gdef, tag, 0, script_priority);
    if scratch.len() != before.len() {
        return true;
    }
    for (a, b) in scratch
        .iter()
        .map(|g| g.glyph_id)
        .zip(before.iter().copied())
    {
        if a != b {
            return true;
        }
    }
    false
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
    script_priority: &[[u8; 4]],
    var: &VarCtx<'_>,
) -> bool {
    apply_gpos_feature_in_scripts_with_var(gpos, glyphs, gdef, tag, script_priority, var)
}

/// Script-priority variant of [`apply_gpos_feature`]. Matches the
/// GSUB equivalent: walks `script_priority` in order, stops at the
/// first script that carries the feature tag, and falls back to
/// DFLT / first script when none match.
///
/// Back-compat entry point for callers outside [`shape`] that do
/// not yet thread a variable-font context; equivalent to calling
/// the `_with_var` form with [`VarCtx::none`].
#[allow(dead_code)]
pub(crate) fn apply_gpos_feature_in_scripts(
    gpos: &Gpos<'_>,
    glyphs: &mut [Glyph],
    gdef: Option<&Gdef<'_>>,
    tag: [u8; 4],
    script_priority: &[[u8; 4]],
) -> bool {
    apply_gpos_feature_in_scripts_with_var(
        gpos,
        glyphs,
        gdef,
        tag,
        script_priority,
        &VarCtx::none(),
    )
}

/// Variable-font aware variant of
/// [`apply_gpos_feature_in_scripts`]. Every ValueRecord delta
/// passes through the `var` context so VariationIndex-backed kern
/// pairs scale with the active coords.
fn apply_gpos_feature_in_scripts_with_var(
    gpos: &Gpos<'_>,
    glyphs: &mut [Glyph],
    gdef: Option<&Gdef<'_>>,
    tag: [u8; 4],
    script_priority: &[[u8; 4]],
    var: &VarCtx<'_>,
) -> bool {
    if glyphs.is_empty() {
        return false;
    }

    let Some(lookup_indices) =
        gpos_lookup_indices_for_feature_in_scripts(gpos, tag, script_priority)
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
        let filter = filter_for_lookup(&lookup, gdef);
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
                    apply_single_pos(&sp, glyphs, &filter, inner_bytes, var);
                    ran_any = true;
                }
                gpos_lt::PAIR_ADJUSTMENT => {
                    let Ok(pp) = PairPos::parse(inner_bytes) else {
                        continue;
                    };
                    apply_pair_pos(&pp, glyphs, &filter, inner_bytes, var);
                    ran_any = true;
                }
                gpos_lt::MARK_TO_BASE => {
                    let Ok(mbp) = MarkBasePos::parse(inner_bytes) else {
                        continue;
                    };
                    apply_mark_base(&mbp, glyphs, gdef, &filter);
                    ran_any = true;
                }
                gpos_lt::MARK_TO_LIGATURE => {
                    let Ok(mlp) = MarkLigaPos::parse(inner_bytes) else {
                        continue;
                    };
                    apply_mark_liga(&mlp, glyphs, gdef, &filter);
                    ran_any = true;
                }
                gpos_lt::MARK_TO_MARK => {
                    let Ok(mmp) = MarkMarkPos::parse(inner_bytes) else {
                        continue;
                    };
                    apply_mark_mark(&mmp, glyphs, gdef, &filter);
                    ran_any = true;
                }
                gpos_lt::CONTEXT => {
                    let Ok(ctx) = ContextPos::parse(inner_bytes) else {
                        continue;
                    };
                    apply_gpos_context_subtable(gpos, &ctx, glyphs, gdef, &filter, var);
                    ran_any = true;
                }
                gpos_lt::CHAINED_CONTEXT => {
                    let Ok(chain) = ChainContextPos::parse(inner_bytes) else {
                        continue;
                    };
                    apply_gpos_chain_context_subtable(gpos, &chain, glyphs, gdef, &filter, var);
                    ran_any = true;
                }
                _ => {}
            }
        }
    }
    ran_any
}

/// Applies a nested GPOS lookup at one specific position in the
/// run. Unlike `apply_gsub_lookup_at` the run length is stable —
/// positioning never inserts or deletes glyphs — so there is no
/// return value for "how many glyphs consumed". Context / chained-
/// context dispatch still needs recursion through this function to
/// handle nested positioning-of-positioning.
///
/// `depth` guards against runaway recursion the same way the GSUB
/// side does; bail out silently once we hit [`MAX_NESTED_DEPTH`].
#[allow(clippy::too_many_lines)]
fn apply_gpos_lookup_at(
    gpos: &Gpos<'_>,
    lookup_idx: u16,
    glyphs: &mut [Glyph],
    gdef: Option<&Gdef<'_>>,
    at: usize,
    depth: u8,
    var: &VarCtx<'_>,
) {
    if depth >= MAX_NESTED_DEPTH {
        return;
    }
    if at >= glyphs.len() {
        return;
    }
    let lookup_list = gpos.lookup_list();
    let Some(lookup) = lookup_list.get(lookup_idx) else {
        return;
    };
    let filter = filter_for_lookup(&lookup, gdef);
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
                let id = glyphs[at].glyph_id as u16;
                if filter.is_skipped(id) {
                    continue;
                }
                if let Some(v) = sp.adjustment(id) {
                    apply_value_record(&mut glyphs[at], &v, inner_bytes, var);
                    return;
                }
            }
            gpos_lt::PAIR_ADJUSTMENT => {
                let Ok(pp) = PairPos::parse(inner_bytes) else {
                    continue;
                };
                let first = glyphs[at].glyph_id as u16;
                if filter.is_skipped(first) {
                    continue;
                }
                // Look up the partner via the skip-iterator so that
                // marks (or any other filtered class) between two
                // kerned bases do not defeat the match.
                let ids: Vec<u16> = glyphs.iter().map(|g| g.glyph_id as u16).collect();
                let Some(partner) = filter.next_unskipped(&ids, at + 1) else {
                    continue;
                };
                let second = glyphs[partner].glyph_id as u16;
                if let Some((v1, v2)) = pp.lookup(first, second) {
                    let (left, right) = glyphs.split_at_mut(partner);
                    apply_value_record(&mut left[at], &v1, inner_bytes, var);
                    apply_value_record(&mut right[0], &v2, inner_bytes, var);
                    return;
                }
            }
            gpos_lt::MARK_TO_BASE => {
                let Ok(mbp) = MarkBasePos::parse(inner_bytes) else {
                    continue;
                };
                // Mark-to-base needs the surrounding run; delegate to
                // the subtable driver on a single-position slice.
                // We pass the whole glyph slice so the driver can
                // walk back to the actual base.
                apply_mark_base(&mbp, glyphs, gdef, &filter);
                return;
            }
            gpos_lt::MARK_TO_LIGATURE => {
                let Ok(mlp) = MarkLigaPos::parse(inner_bytes) else {
                    continue;
                };
                apply_mark_liga(&mlp, glyphs, gdef, &filter);
                return;
            }
            gpos_lt::MARK_TO_MARK => {
                let Ok(mmp) = MarkMarkPos::parse(inner_bytes) else {
                    continue;
                };
                apply_mark_mark(&mmp, glyphs, gdef, &filter);
                return;
            }
            gpos_lt::CONTEXT => {
                let Ok(ctx) = ContextPos::parse(inner_bytes) else {
                    continue;
                };
                let ids: Vec<u16> = glyphs.iter().map(|g| g.glyph_id as u16).collect();
                apply_gpos_context_at(gpos, &ctx, glyphs, &ids, gdef, &filter, at, depth + 1, var);
                return;
            }
            gpos_lt::CHAINED_CONTEXT => {
                let Ok(chain) = ChainContextPos::parse(inner_bytes) else {
                    continue;
                };
                let ids: Vec<u16> = glyphs.iter().map(|g| g.glyph_id as u16).collect();
                apply_gpos_chain_context_at(
                    gpos,
                    &chain,
                    glyphs,
                    &ids,
                    gdef,
                    &filter,
                    at,
                    depth + 1,
                    var,
                );
                return;
            }
            _ => {}
        }
    }
}

/// Scans the run and applies a GPOS type-7 contextual subtable on
/// every hit. Glyph-id snapshot is built once and reused across the
/// cursor walk; positioning never changes glyph ids so the snapshot
/// stays valid for the whole pass.
fn apply_gpos_context_subtable(
    gpos: &Gpos<'_>,
    ctx: &ContextPos<'_>,
    glyphs: &mut [Glyph],
    gdef: Option<&Gdef<'_>>,
    filter: &MatchFilter<'_>,
    var: &VarCtx<'_>,
) {
    let ids: Vec<u16> = glyphs.iter().map(|g| g.glyph_id as u16).collect();
    let mut i = 0;
    while i < glyphs.len() {
        let consumed = apply_gpos_context_at(gpos, ctx, glyphs, &ids, gdef, filter, i, 0, var);
        if consumed > 0 {
            i += consumed;
        } else {
            i += 1;
        }
    }
}

/// Scans the run and applies a GPOS type-8 chained-context subtable
/// on every hit.
fn apply_gpos_chain_context_subtable(
    gpos: &Gpos<'_>,
    chain: &ChainContextPos<'_>,
    glyphs: &mut [Glyph],
    gdef: Option<&Gdef<'_>>,
    filter: &MatchFilter<'_>,
    var: &VarCtx<'_>,
) {
    let ids: Vec<u16> = glyphs.iter().map(|g| g.glyph_id as u16).collect();
    let mut i = 0;
    while i < glyphs.len() {
        let consumed =
            apply_gpos_chain_context_at(gpos, chain, glyphs, &ids, gdef, filter, i, 0, var);
        if consumed > 0 {
            i += consumed;
        } else {
            i += 1;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_gpos_context_at(
    gpos: &Gpos<'_>,
    ctx: &ContextPos<'_>,
    glyphs: &mut [Glyph],
    ids: &[u16],
    gdef: Option<&Gdef<'_>>,
    filter: &MatchFilter<'_>,
    at: usize,
    depth: u8,
    var: &VarCtx<'_>,
) -> usize {
    let (input_len, lookups): (usize, Vec<SequenceLookupRecord>) = match ctx {
        ContextPos::Format1(c) => {
            let Some((n, lks)) = c.matches_filtered(ids, at, filter) else {
                return 0;
            };
            (n, lks.to_vec())
        }
        ContextPos::Format2(c) => {
            let Some((n, lks)) = c.matches_filtered(ids, at, filter) else {
                return 0;
            };
            (n, lks.to_vec())
        }
        ContextPos::Format3(c) => {
            let Some(n) = c.matches_filtered(ids, at, filter) else {
                return 0;
            };
            (n, c.lookups().to_vec())
        }
    };
    apply_nested_gpos_lookups(gpos, glyphs, gdef, filter, ids, at, depth, &lookups, var);
    input_len.max(1)
}

#[allow(clippy::too_many_arguments)]
fn apply_gpos_chain_context_at(
    gpos: &Gpos<'_>,
    chain: &ChainContextPos<'_>,
    glyphs: &mut [Glyph],
    ids: &[u16],
    gdef: Option<&Gdef<'_>>,
    filter: &MatchFilter<'_>,
    at: usize,
    depth: u8,
    var: &VarCtx<'_>,
) -> usize {
    let (input_len, lookups): (usize, Vec<SequenceLookupRecord>) = match chain {
        ChainContextPos::Format1(c) => {
            let Some((n, lks)) = c.matches_filtered(ids, at, filter) else {
                return 0;
            };
            (n, lks.to_vec())
        }
        ChainContextPos::Format2(c) => {
            let Some((n, lks)) = c.matches_filtered(ids, at, filter) else {
                return 0;
            };
            (n, lks.to_vec())
        }
        ChainContextPos::Format3(c) => {
            let Some(n) = c.matches_filtered(ids, at, filter) else {
                return 0;
            };
            (n, c.lookups().to_vec())
        }
    };
    apply_nested_gpos_lookups(gpos, glyphs, gdef, filter, ids, at, depth, &lookups, var);
    input_len.max(1)
}

/// See [`apply_nested_gsub_lookups`] — this is the GPOS twin.
#[allow(clippy::too_many_arguments)]
fn apply_nested_gpos_lookups(
    gpos: &Gpos<'_>,
    glyphs: &mut [Glyph],
    gdef: Option<&Gdef<'_>>,
    filter: &MatchFilter<'_>,
    ids: &[u16],
    at: usize,
    depth: u8,
    lookups: &[SequenceLookupRecord],
    var: &VarCtx<'_>,
) {
    for rec in lookups {
        let seq = rec.sequence_index as usize;
        let pos = if seq == 0 {
            at
        } else {
            let mut cursor = at + 1;
            let mut pos = at;
            for _ in 0..seq {
                match filter.next_unskipped(ids, cursor) {
                    Some(p) => {
                        pos = p;
                        cursor = p + 1;
                    }
                    None => return,
                }
            }
            pos
        };
        apply_gpos_lookup_at(gpos, rec.lookup_list_index, glyphs, gdef, pos, depth, var);
    }
}

/// Default-LangSys lookup-index collection for a GPOS feature tag,
/// mirroring the GSUB helper.
#[allow(dead_code)]
fn gpos_lookup_indices_for_feature(gpos: &Gpos<'_>, tag: [u8; 4]) -> Option<Vec<u16>> {
    gpos_lookup_indices_for_feature_in_scripts(gpos, tag, &[*b"DFLT"])
}

/// Script-priority GPOS helper. Twin of
/// [`lookup_indices_for_feature_in_scripts`] for GSUB.
fn gpos_lookup_indices_for_feature_in_scripts(
    gpos: &Gpos<'_>,
    tag: [u8; 4],
    script_priority: &[[u8; 4]],
) -> Option<Vec<u16>> {
    let script_list = gpos.script_list();
    let feature_list = gpos.feature_list();

    for tag_pri in script_priority {
        let Some(script) = script_list.find(*tag_pri) else {
            continue;
        };
        let Some(lang_sys) = script.default_lang_sys() else {
            continue;
        };
        let mut indices: Vec<u16> = Vec::new();
        let mut has_feature = false;
        for feat_idx in lang_sys.feature_indices() {
            let Some((feat_tag, feature)) = feature_list.get(feat_idx) else {
                continue;
            };
            if feat_tag != tag {
                continue;
            }
            has_feature = true;
            for idx in feature.lookup_indices() {
                if !indices.contains(&idx) {
                    indices.push(idx);
                }
            }
        }
        if has_feature {
            indices.sort_unstable();
            return Some(indices);
        }
    }

    let already_tried_dflt = script_priority.iter().any(|t| *t == *b"DFLT");
    let script = if already_tried_dflt {
        script_list.iter().next().map(|(_, s)| s)?
    } else {
        script_list
            .find(*b"DFLT")
            .or_else(|| script_list.iter().next().map(|(_, s)| s))?
    };
    let lang_sys = script.default_lang_sys()?;
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
/// every covered glyph. Glyphs filtered out by the lookup flag
/// (marks / bases / ligatures per the active LookupFlag) are
/// skipped. `subtable` is the SinglePos subtable bytes, from which
/// any Device/VariationIndex offset resolves.
fn apply_single_pos(
    sp: &SinglePos<'_>,
    glyphs: &mut [Glyph],
    filter: &MatchFilter<'_>,
    subtable: &[u8],
    var: &VarCtx<'_>,
) {
    for glyph in glyphs.iter_mut() {
        let id = glyph.glyph_id as u16;
        if filter.is_skipped(id) {
            continue;
        }
        if let Some(v) = sp.adjustment(id) {
            apply_value_record(glyph, &v, subtable, var);
        }
    }
}

/// Walks the run and, for each mark glyph (per GDEF), finds the
/// nearest preceding base and attaches via the subtable's anchor
/// tables. Without GDEF we cannot distinguish marks from bases and
/// the pass is a no-op — that matches HarfBuzz's behaviour.
///
/// The lookup's `LookupFlag` gates which marks participate. A lookup
/// that carries a `UseMarkFilteringSet` or `MarkAttachmentType`
/// restriction skips marks that fall outside the active subset; the
/// shaper walks the same stream, only the "is this a candidate
/// mark?" predicate changes.
fn apply_mark_base(
    mbp: &MarkBasePos<'_>,
    glyphs: &mut [Glyph],
    gdef: Option<&Gdef<'_>>,
    filter: &MatchFilter<'_>,
) {
    let Some(gdef) = gdef else {
        return;
    };

    for i in 0..glyphs.len() {
        let mark_gid = glyphs[i].glyph_id as u16;
        if !gdef.glyph_class(mark_gid).is_mark() {
            continue;
        }
        // LookupFlag gating: skip marks the filter tells us to ignore.
        if filter.is_skipped(mark_gid) {
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
fn apply_mark_liga(
    mlp: &MarkLigaPos<'_>,
    glyphs: &mut [Glyph],
    gdef: Option<&Gdef<'_>>,
    filter: &MatchFilter<'_>,
) {
    let Some(gdef) = gdef else {
        return;
    };

    for i in 0..glyphs.len() {
        let mark_gid = glyphs[i].glyph_id as u16;
        if !gdef.glyph_class(mark_gid).is_mark() {
            continue;
        }
        if filter.is_skipped(mark_gid) {
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
fn apply_mark_mark(
    mmp: &MarkMarkPos<'_>,
    glyphs: &mut [Glyph],
    gdef: Option<&Gdef<'_>>,
    filter: &MatchFilter<'_>,
) {
    let Some(gdef) = gdef else {
        return;
    };

    for i in 1..glyphs.len() {
        let mark1_gid = glyphs[i].glyph_id as u16;
        if !gdef.glyph_class(mark1_gid).is_mark() {
            continue;
        }
        if filter.is_skipped(mark1_gid) {
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

/// AAT `morx` substitution pass — runs only when the font has no
/// GSUB. The morx parser returns a new glyph id stream plus an
/// origin vector; each output index carries the input index it was
/// derived from (or the smallest input index for a ligature). We
/// rebuild the `Glyph` vector by copying metadata from that origin
/// so clusters survive ligation: the surviving glyph inherits the
/// first component's cluster, matching HarfBuzz's "merge clusters
/// to earliest" policy.
fn apply_morx(morx: &Morx<'_>, glyphs: &mut Vec<Glyph>) {
    if glyphs.is_empty() {
        return;
    }
    let input_ids: Vec<u16> = glyphs.iter().map(|g| g.glyph_id as u16).collect();
    let (out_ids, origins) = morx.apply(&input_ids);
    if out_ids.len() == glyphs.len() && out_ids == input_ids {
        return; // no change — avoid needless allocation
    }
    let mut rebuilt: Vec<Glyph> = Vec::with_capacity(out_ids.len());
    for (out_idx, &gid) in out_ids.iter().enumerate() {
        let origin = origins.get(out_idx).copied().unwrap_or(usize::MAX);
        if origin < glyphs.len() {
            let mut g = glyphs[origin];
            g.glyph_id = u32::from(gid);
            rebuilt.push(g);
        } else {
            // Synthesised output with no single origin — rare; fall
            // back to the lowest available cluster so layout does
            // not confuse renderer-side grapheme tracking.
            let cluster = glyphs.first().map_or(0, |g| g.cluster);
            rebuilt.push(Glyph::new(u32::from(gid), cluster));
        }
    }
    *glyphs = rebuilt;
}

/// AAT `kerx` apply pass.
///
/// Two passes run in sequence:
///
/// - Format 0 / 2 (pair-list and compound-class) emit a delta per
///   adjacent pair; we distribute it half-on-left, half-on-right
///   exactly like [`apply_legacy_kern`] so kerx output lines up
///   with the macOS renderer for the same pairs.
/// - Format 1 (state-machine) walks the run through an AAT state
///   table; each value-list pop targets a single stacked glyph and
///   the delta is applied directly to that glyph's `x_advance`. No
///   half-split — the state machine already chose which glyph to
///   land on (typically the left of the pair).
fn apply_kerx(kerx: &Kerx<'_>, glyphs: &mut [Glyph]) {
    if glyphs.is_empty() {
        return;
    }
    if glyphs.len() >= 2 {
        for i in 0..glyphs.len() - 1 {
            let left = glyphs[i].glyph_id as u16;
            let right = glyphs[i + 1].glyph_id as u16;
            let delta = i32::from(kerx.kern(left, right));
            if delta != 0 {
                let half = delta / 2;
                glyphs[i].x_advance += delta - half;
                glyphs[i + 1].x_advance += half;
            }
        }
    }
    if kerx.has_state_machine() {
        let ids: alloc::vec::Vec<u16> = glyphs.iter().map(|g| g.glyph_id as u16).collect();
        kerx.apply_state_machines(&ids, |idx, delta| {
            if let Some(g) = glyphs.get_mut(idx) {
                g.x_advance += i32::from(delta);
            }
        });
    }
    // Format 4 (control-point) apply. Inline-coordinates events
    // resolve fully without consulting glyf / ankr; we land them as
    // x/y offsets on the current glyph. Control-point and anchor-
    // point events need a glyf-point or `ankr` lookup that
    // [`Face::glyph_points`] does not yet expose — those events are
    // dropped silently until the API lands. Dropping is the same
    // behaviour the parse-only path used to give, so no regression.
    if kerx.has_format4() {
        let ids: alloc::vec::Vec<u16> = glyphs.iter().map(|g| g.glyph_id as u16).collect();
        kerx.apply_format4(&ids, |evt| {
            if let crate::tables::kerx::Kerx4Action::Coordinates {
                mark_index: _,
                current_index,
                mark_x,
                mark_y,
                current_x,
                current_y,
            } = evt
            {
                if let Some(g) = glyphs.get_mut(current_index) {
                    let dx = i32::from(mark_x) - i32::from(current_x);
                    let dy = i32::from(mark_y) - i32::from(current_y);
                    g.x_offset += dx;
                    g.y_offset += dy;
                }
            }
        });
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

fn apply_pair_pos(
    pp: &PairPos<'_>,
    glyphs: &mut [Glyph],
    filter: &MatchFilter<'_>,
    subtable: &[u8],
    var: &VarCtx<'_>,
) {
    if glyphs.len() < 2 {
        return;
    }
    // Precompute the id vector once — the filter cost is one GDEF
    // lookup per hop, but walking the whole window fresh per index
    // via next_unskipped already avoids revisiting filtered glyphs.
    let ids: Vec<u16> = glyphs.iter().map(|g| g.glyph_id as u16).collect();
    let mut i = 0;
    while i + 1 < glyphs.len() {
        let first = ids[i];
        if filter.is_skipped(first) {
            i += 1;
            continue;
        }
        let Some(j) = filter.next_unskipped(&ids, i + 1) else {
            break;
        };
        let second = ids[j];
        if let Some((v1, v2)) = pp.lookup(first, second) {
            // PairPos applies v1 to `i` and v2 to `j`. Both records'
            // Device/VariationIndex offsets root at the PairPos
            // subtable bytes; the second-y_advance slot follows the
            // same rule.
            {
                let (left, right) = glyphs.split_at_mut(j);
                apply_value_record(&mut left[i], &v1, subtable, var);
                apply_value_record(&mut right[0], &v2, subtable, var);
            }
        }
        // Advance to the position of the second glyph; HarfBuzz uses
        // a per-pair stride so a single glyph can kern against both
        // its left and right neighbours in one pass.
        i = j;
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

    // ---------------------------------------------------------------
    // End-to-end fixtures for the contextual GSUB lookups. The font
    // below is the same shape as `build_shapeable_font` plus a
    // caller-supplied GSUB table that enables one feature `test` on
    // the default script/DFLT LangSys.
    // ---------------------------------------------------------------

    /// Builds a font identical to `build_shapeable_font` plus a GSUB
    /// table carrying whatever lookup subtables the caller provides.
    /// Each entry of `lookups` is `(lookup_type, subtable_bytes)`;
    /// only the lookup indices in `feature_indices` fire from the
    /// top-level `test` feature — the rest are still in the
    /// LookupList so nested-lookup dispatch can reach them.
    fn build_shapeable_font_with_gsub(
        lookups: &[(u16, Vec<u8>)],
        feature_indices: &[u16],
    ) -> Vec<u8> {
        let gsub_bytes = build_single_feature_gsub_with_filter(*b"test", lookups, feature_indices);

        // Reuse the build_shapeable_font bodies by re-assembling with
        // GSUB appended.
        let mut head = Vec::new();
        head.extend_from_slice(&1u16.to_be_bytes());
        head.extend_from_slice(&0u16.to_be_bytes());
        head.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        head.extend_from_slice(&0u32.to_be_bytes());
        head.extend_from_slice(&0x5F0F_3CF5u32.to_be_bytes());
        head.extend_from_slice(&0u16.to_be_bytes());
        head.extend_from_slice(&1000u16.to_be_bytes());
        head.extend_from_slice(&[0; 8 + 8 + 8 + 2 + 2 + 2]);
        head.extend_from_slice(&0i16.to_be_bytes());
        head.extend_from_slice(&0i16.to_be_bytes());

        let mut maxp = Vec::new();
        maxp.extend_from_slice(&0x0000_5000u32.to_be_bytes());
        maxp.extend_from_slice(&10u16.to_be_bytes()); // glyph count

        let mut hhea = Vec::new();
        hhea.extend_from_slice(&1u16.to_be_bytes());
        hhea.extend_from_slice(&0u16.to_be_bytes());
        hhea.extend_from_slice(&800i16.to_be_bytes());
        hhea.extend_from_slice(&(-200i16).to_be_bytes());
        hhea.extend_from_slice(&0i16.to_be_bytes());
        hhea.extend_from_slice(&[0; 14]);
        hhea.extend_from_slice(&[0; 8]);
        hhea.extend_from_slice(&0i16.to_be_bytes());
        hhea.extend_from_slice(&10u16.to_be_bytes()); // numberOfHMetrics

        let mut hmtx = Vec::new();
        for adv in [0u16, 500, 600, 700, 500, 500, 500, 500, 500, 500] {
            hmtx.extend_from_slice(&adv.to_be_bytes());
            hmtx.extend_from_slice(&0i16.to_be_bytes());
        }

        // cmap format 4 mapping: A..=C -> 1..=3 (delta -64), D..=F ->
        // 4..=6 (delta -64 too, same range works because we chain
        // another segment). Simplest: map A..=F with delta -64.
        let cmap_sub = build_format4(&[(b'A' as u16, b'F' as u16, -64)]);
        let cmap = build_cmap_wrapper(&[(3, 1, cmap_sub)]);

        let tables: Vec<([u8; 4], Vec<u8>)> = alloc::vec![
            (*b"GSUB", gsub_bytes),
            (*b"cmap", cmap),
            (*b"head", head),
            (*b"hhea", hhea),
            (*b"hmtx", hmtx),
            (*b"maxp", maxp),
        ];
        assemble_sfnt(&tables)
    }

    /// Builds a GSUB table wiring a single feature tag to a subset
    /// of the given lookup list. `feature_lookup_indices` selects
    /// which entries fire from the feature; lookups outside the set
    /// are still reachable via nested dispatch but do not run as the
    /// top-level feature walk.
    fn build_single_feature_gsub_with_filter(
        tag: [u8; 4],
        lookups: &[(u16, Vec<u8>)],
        feature_lookup_indices: &[u16],
    ) -> Vec<u8> {
        // ---- Build LookupList bytes ----
        let mut lookup_list = Vec::new();
        lookup_list.extend_from_slice(&(lookups.len() as u16).to_be_bytes());
        let offsets_start = lookup_list.len();
        for _ in 0..lookups.len() {
            lookup_list.extend_from_slice(&[0u8; 2]);
        }
        // Each lookup header: u16 type, u16 flag, u16 subtableCount,
        // u16 subtableOffset[]. Subtable bodies are appended after
        // the header.
        for (i, (lt, body)) in lookups.iter().enumerate() {
            let lookup_off = lookup_list.len();
            let slot = offsets_start + i * 2;
            lookup_list[slot..slot + 2].copy_from_slice(&(lookup_off as u16).to_be_bytes());
            lookup_list.extend_from_slice(&lt.to_be_bytes());
            lookup_list.extend_from_slice(&0u16.to_be_bytes()); // flag
            lookup_list.extend_from_slice(&1u16.to_be_bytes()); // subtable count
            let sub_slot = lookup_list.len();
            lookup_list.extend_from_slice(&[0u8; 2]); // subtable offset slot
            let sub_off_rel = lookup_list.len() - lookup_off;
            lookup_list[sub_slot..sub_slot + 2]
                .copy_from_slice(&(sub_off_rel as u16).to_be_bytes());
            lookup_list.extend_from_slice(body);
        }

        // ---- FeatureList ----
        // One feature referencing the filtered lookup indices.
        let mut feature_list = Vec::new();
        feature_list.extend_from_slice(&1u16.to_be_bytes()); // featureCount
        let feat_rec_off = feature_list.len();
        feature_list.extend_from_slice(&tag); // featureTag
        feature_list.extend_from_slice(&0u16.to_be_bytes()); // feature offset slot
        let feat_off_rel = feature_list.len();
        // Feature table: featureParamsOffset=0, lookupIndexCount, lookupIndexArray.
        feature_list.extend_from_slice(&0u16.to_be_bytes()); // params offset
        feature_list.extend_from_slice(&(feature_lookup_indices.len() as u16).to_be_bytes());
        for &i in feature_lookup_indices {
            feature_list.extend_from_slice(&i.to_be_bytes());
        }
        feature_list[feat_rec_off + 4..feat_rec_off + 6]
            .copy_from_slice(&(feat_off_rel as u16).to_be_bytes());

        // ---- ScriptList: one DFLT script with default LangSys and feature 0 ----
        let mut script_list = Vec::new();
        script_list.extend_from_slice(&1u16.to_be_bytes()); // scriptCount
        let script_rec_off = script_list.len();
        script_list.extend_from_slice(b"DFLT");
        script_list.extend_from_slice(&0u16.to_be_bytes()); // script offset slot
        let script_off_rel = script_list.len();
        // Script table: defaultLangSysOffset, langSysCount=0.
        script_list.extend_from_slice(&0u16.to_be_bytes()); // default langSys slot
        script_list.extend_from_slice(&0u16.to_be_bytes()); // langSysCount
        let default_langsys_slot = script_off_rel;
        let default_langsys_off_rel = script_list.len() - script_off_rel;
        // LangSys: lookupOrderOffset=0, requiredFeatureIndex=0xFFFF,
        //          featureIndexCount, featureIndexArray.
        script_list.extend_from_slice(&0u16.to_be_bytes()); // lookupOrder
        script_list.extend_from_slice(&0xFFFFu16.to_be_bytes()); // required (none)
        script_list.extend_from_slice(&1u16.to_be_bytes()); // feature count
        script_list.extend_from_slice(&0u16.to_be_bytes()); // feature index 0
        script_list[default_langsys_slot..default_langsys_slot + 2]
            .copy_from_slice(&(default_langsys_off_rel as u16).to_be_bytes());
        script_list[script_rec_off + 4..script_rec_off + 6]
            .copy_from_slice(&(script_off_rel as u16).to_be_bytes());

        // ---- Assemble top-level GSUB header ----
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // major
        out.extend_from_slice(&0u16.to_be_bytes()); // minor
        let sl_slot = out.len();
        out.extend_from_slice(&0u16.to_be_bytes());
        let fl_slot = out.len();
        out.extend_from_slice(&0u16.to_be_bytes());
        let ll_slot = out.len();
        out.extend_from_slice(&0u16.to_be_bytes());

        let sl_off = out.len();
        out.extend_from_slice(&script_list);
        let fl_off = out.len();
        out.extend_from_slice(&feature_list);
        let ll_off = out.len();
        out.extend_from_slice(&lookup_list);

        out[sl_slot..sl_slot + 2].copy_from_slice(&(sl_off as u16).to_be_bytes());
        out[fl_slot..fl_slot + 2].copy_from_slice(&(fl_off as u16).to_be_bytes());
        out[ll_slot..ll_slot + 2].copy_from_slice(&(ll_off as u16).to_be_bytes());
        out
    }

    // Helpers for building individual subtable bodies.
    fn build_cov_fmt1(glyphs: &[u16]) -> Vec<u8> {
        let mut o = Vec::new();
        o.extend_from_slice(&1u16.to_be_bytes());
        o.extend_from_slice(&(glyphs.len() as u16).to_be_bytes());
        for g in glyphs {
            o.extend_from_slice(&g.to_be_bytes());
        }
        o
    }

    fn build_classdef_fmt2(ranges: &[(u16, u16, u16)]) -> Vec<u8> {
        let mut o = Vec::new();
        o.extend_from_slice(&2u16.to_be_bytes());
        o.extend_from_slice(&(ranges.len() as u16).to_be_bytes());
        for (s, e, c) in ranges {
            o.extend_from_slice(&s.to_be_bytes());
            o.extend_from_slice(&e.to_be_bytes());
            o.extend_from_slice(&c.to_be_bytes());
        }
        o
    }

    /// Builds a GSUB type-1 format-2 (explicit) single-sub subtable
    /// that maps each glyph in `coverage` to the corresponding entry
    /// in `substitutes`.
    fn build_single_fmt2_subst(coverage_glyphs: &[u16], substitutes: &[u16]) -> Vec<u8> {
        assert_eq!(coverage_glyphs.len(), substitutes.len());
        let mut o = Vec::new();
        o.extend_from_slice(&2u16.to_be_bytes()); // format
        o.extend_from_slice(&0u16.to_be_bytes()); // cov offset slot
        o.extend_from_slice(&(substitutes.len() as u16).to_be_bytes());
        for s in substitutes {
            o.extend_from_slice(&s.to_be_bytes());
        }
        let cov_off = o.len();
        o.extend_from_slice(&build_cov_fmt1(coverage_glyphs));
        o[2..4].copy_from_slice(&(cov_off as u16).to_be_bytes());
        o
    }

    #[test]
    fn gsub_context_fmt3_runs_nested_single_substitution() {
        // Setup: shape "ABC" where glyph A=1, B=2, C=3.
        // Lookup 0: single-sub that rewrites glyph 2 → 9.
        // Lookup 1: context fmt 3 — input = cov{1}, cov{2}, cov{3};
        //           nested lookup (seq=1, lk=0) i.e. fire lookup 0 on
        //           the B position.
        let single = build_single_fmt2_subst(&[2], &[9]);

        // Context fmt 3 subtable:
        //   u16 format = 3
        //   u16 glyphCount = 3
        //   u16 lookupCount = 1
        //   u16 covOffset[3]
        //   (seq, lookup) * 1
        let mut ctx = Vec::new();
        ctx.extend_from_slice(&3u16.to_be_bytes());
        ctx.extend_from_slice(&3u16.to_be_bytes());
        ctx.extend_from_slice(&1u16.to_be_bytes());
        ctx.extend_from_slice(&[0u8; 6]); // three cov slots
        ctx.extend_from_slice(&1u16.to_be_bytes()); // seq
        ctx.extend_from_slice(&0u16.to_be_bytes()); // lookup index
        let cov_slots_start = 6;
        for (j, gs) in [&[1u16][..], &[2u16][..], &[3u16][..]].iter().enumerate() {
            let off = ctx.len();
            ctx.extend_from_slice(&build_cov_fmt1(gs));
            let slot = cov_slots_start + j * 2;
            ctx[slot..slot + 2].copy_from_slice(&(off as u16).to_be_bytes());
        }

        // Feature fires only lookup index 1 (the context lookup);
        // lookup 0 (single-sub) is reachable via nested dispatch.
        let data = build_shapeable_font_with_gsub(&[(1, single), (5, ctx)], &[1]);
        let blob = Blob::new(&data);
        let face = Face::parse(&blob, 0).unwrap();
        let font = Font::new(face, 16.0);
        let mut buffer = Buffer::new();
        buffer.push_str("ABC");

        // Enable the custom feature tag with value 1 so the
        // override walker dispatches through apply_gsub_feature.
        let features = [Feature {
            tag: *b"test",
            value: 1,
        }];
        let shaped = shape(&font, &buffer, &features).unwrap();
        assert_eq!(shaped.len(), 3);
        assert_eq!(shaped.glyphs[0].glyph_id, 1); // A unchanged
        assert_eq!(shaped.glyphs[1].glyph_id, 9); // B -> 9 via nested single-sub
        assert_eq!(shaped.glyphs[2].glyph_id, 3); // C unchanged

        // Buffer of length 2 does not match the 3-wide context, so
        // no substitution fires.
        let mut buffer2 = Buffer::new();
        buffer2.push_str("AB");
        let shaped2 = shape(&font, &buffer2, &features).unwrap();
        assert_eq!(shaped2.glyphs[1].glyph_id, 2);
    }

    #[test]
    fn gsub_chain_context_fmt2_class_based_runs_real_font() {
        // Fixture: glyphs 1=A, 2=B, 3=C, 4=D, 5=E, 6=F.
        //
        // Shared class def (format 2):
        //   1 -> class 1 (A, "consonant")
        //   2 -> class 2 (B, "vowel")
        //   3 -> class 3 (C, "punct")
        //   4 -> class 1
        //   5 -> class 2
        //
        // Rule: for first glyph of class 2 (vowels B, E), if the
        // preceding glyph is class 1 (A/D) and the following glyph
        // is class 3 (C), run nested single-sub at seq 0 that maps
        // B→7, E→8 (glyph in slot 7/8).
        //
        // Lookup layout:
        //   Lookup 0: single-sub explicit, coverage={2,5}, subs={7,8}.
        //   Lookup 1: chain-context fmt 2, fires lookup 0 at seq 0.
        let single = build_single_fmt2_subst(&[2, 5], &[7, 8]);

        // Build chain-context fmt 2.
        //   u16 format=2
        //   u16 coverageOffset
        //   u16 backtrackClassDefOffset
        //   u16 inputClassDefOffset
        //   u16 lookaheadClassDefOffset
        //   u16 classSetCount (4 — classes 0..=3, nulls for 0, 1, 3)
        //   u16 classSetOffsets[]
        let mut cc = Vec::new();
        cc.extend_from_slice(&2u16.to_be_bytes()); // format
        cc.extend_from_slice(&0u16.to_be_bytes()); // cov slot
        cc.extend_from_slice(&0u16.to_be_bytes()); // bt cd
        cc.extend_from_slice(&0u16.to_be_bytes()); // in cd
        cc.extend_from_slice(&0u16.to_be_bytes()); // la cd
        cc.extend_from_slice(&4u16.to_be_bytes()); // class set count
        cc.extend_from_slice(&0u16.to_be_bytes()); // set[0] NULL
        cc.extend_from_slice(&0u16.to_be_bytes()); // set[1] NULL
        cc.extend_from_slice(&0u16.to_be_bytes()); // set[2] slot
        cc.extend_from_slice(&0u16.to_be_bytes()); // set[3] NULL
        let cov_slot = 2;
        let bt_cd_slot = 4;
        let in_cd_slot = 6;
        let la_cd_slot = 8;
        let set2_slot = 16;

        // ClassSet 2 with one rule.
        let set_off = cc.len();
        cc.extend_from_slice(&1u16.to_be_bytes()); // rule count
        cc.extend_from_slice(&0u16.to_be_bytes()); // rule slot
        let rule_off_rel = cc.len() - set_off;
        // Rule body: bt_count=1, bt_classes=[1]; in_count=1, (no tail);
        // la_count=1, la_classes=[3]; lookup_count=1, (seq, lk)=(0, 0).
        cc.extend_from_slice(&1u16.to_be_bytes()); // bt count
        cc.extend_from_slice(&1u16.to_be_bytes()); // bt class
        cc.extend_from_slice(&1u16.to_be_bytes()); // input count (1, so tail is 0)
        cc.extend_from_slice(&1u16.to_be_bytes()); // la count
        cc.extend_from_slice(&3u16.to_be_bytes()); // la class
        cc.extend_from_slice(&1u16.to_be_bytes()); // lookup count
        cc.extend_from_slice(&0u16.to_be_bytes()); // seq index
        cc.extend_from_slice(&0u16.to_be_bytes()); // lookup list index
        cc[set_off + 2..set_off + 4].copy_from_slice(&(rule_off_rel as u16).to_be_bytes());
        cc[set2_slot..set2_slot + 2].copy_from_slice(&(set_off as u16).to_be_bytes());

        // Coverage: {2, 5}.
        let cov_off = cc.len();
        cc.extend_from_slice(&build_cov_fmt1(&[2, 5]));
        cc[cov_slot..cov_slot + 2].copy_from_slice(&(cov_off as u16).to_be_bytes());

        // Shared ClassDef format 2.
        let cd = build_classdef_fmt2(&[(1, 1, 1), (2, 2, 2), (3, 3, 3), (4, 4, 1), (5, 5, 2)]);
        let cd_off = cc.len();
        cc.extend_from_slice(&cd);
        for slot in [bt_cd_slot, in_cd_slot, la_cd_slot] {
            cc[slot..slot + 2].copy_from_slice(&(cd_off as u16).to_be_bytes());
        }

        let data = build_shapeable_font_with_gsub(&[(1, single), (6, cc)], &[1]);
        let blob = Blob::new(&data);
        let face = Face::parse(&blob, 0).unwrap();
        let font = Font::new(face, 16.0);
        let features = [Feature {
            tag: *b"test",
            value: 1,
        }];

        // "ABC": A=class1, B=class2, C=class3 -> fires, B→7.
        let mut buf = Buffer::new();
        buf.push_str("ABC");
        let out = shape(&font, &buf, &features).unwrap();
        assert_eq!(out.glyphs[1].glyph_id, 7);
        assert_eq!(out.glyphs[0].glyph_id, 1); // A unchanged
        assert_eq!(out.glyphs[2].glyph_id, 3); // C unchanged

        // "DEC": D=class1, E=class2, C=class3 -> fires, E→8.
        let mut buf = Buffer::new();
        buf.push_str("DEC");
        let out = shape(&font, &buf, &features).unwrap();
        assert_eq!(out.glyphs[1].glyph_id, 8);

        // "ABF": B is class2 but F is class0 (not class3) -> no fire.
        let mut buf = Buffer::new();
        buf.push_str("ABF");
        let out = shape(&font, &buf, &features).unwrap();
        assert_eq!(out.glyphs[1].glyph_id, 2);

        // "BBC": first B has no preceding class-1 backtrack -> no fire.
        let mut buf = Buffer::new();
        buf.push_str("BBC");
        let out = shape(&font, &buf, &features).unwrap();
        assert_eq!(out.glyphs[0].glyph_id, 2);
        // Second B has preceding class-2, not class-1, so also no fire.
        assert_eq!(out.glyphs[1].glyph_id, 2);
    }

    #[test]
    fn gsub_reverse_chain_iterates_right_to_left() {
        // Fixture: glyphs 1=A, 2=B, 3=C, 4=D, 5=E, 6=F.
        //
        // Reverse-chain rule: cover {2,5} (B, E). When lookahead is
        // {3,6} (C, F) substitute B→7, E→8. No backtrack.
        //
        // "BECF": walked right-to-left:
        //   pos 3 (F): not in coverage, skip.
        //   pos 2 (C): not in coverage, skip.
        //   pos 1 (E): cov idx = 1 -> sub 8; lookahead glyph is
        //              currently C which is in {3,6}; substitute E→8.
        //   pos 0 (B): cov idx = 0 -> sub 7; lookahead glyph is
        //              (post-sub) 8 which is NOT in {3,6}, so reject.
        // Expected out: [B=2, E->8, C=3, F=6].
        let mut rc = Vec::new();
        rc.extend_from_slice(&1u16.to_be_bytes()); // format
        rc.extend_from_slice(&0u16.to_be_bytes()); // cov slot
        rc.extend_from_slice(&0u16.to_be_bytes()); // backtrack count
        rc.extend_from_slice(&1u16.to_be_bytes()); // lookahead count
        rc.extend_from_slice(&0u16.to_be_bytes()); // la cov slot
        rc.extend_from_slice(&2u16.to_be_bytes()); // glyphCount
        rc.extend_from_slice(&7u16.to_be_bytes());
        rc.extend_from_slice(&8u16.to_be_bytes());
        // Offsets: format(2) + cov(2) = cov slot at 2; bt_count(2) +
        // la_count(2) = lookahead cov slot at 8.
        let cov_slot = 2;
        let la_slot = 8;
        let cov_off = rc.len();
        rc.extend_from_slice(&build_cov_fmt1(&[2, 5]));
        rc[cov_slot..cov_slot + 2].copy_from_slice(&(cov_off as u16).to_be_bytes());
        let la_off = rc.len();
        rc.extend_from_slice(&build_cov_fmt1(&[3, 6]));
        rc[la_slot..la_slot + 2].copy_from_slice(&(la_off as u16).to_be_bytes());

        let data = build_shapeable_font_with_gsub(&[(8, rc)], &[0]);
        let blob = Blob::new(&data);
        let face = Face::parse(&blob, 0).unwrap();
        let font = Font::new(face, 16.0);
        let features = [Feature {
            tag: *b"test",
            value: 1,
        }];

        let mut buf = Buffer::new();
        buf.push_str("BECF");
        let out = shape(&font, &buf, &features).unwrap();
        assert_eq!(
            out.glyphs[0].glyph_id, 2,
            "B should survive: its post-sub lookahead no longer matches"
        );
        assert_eq!(
            out.glyphs[1].glyph_id, 8,
            "E should become 8 via reverse-chain"
        );
        assert_eq!(out.glyphs[2].glyph_id, 3);
        assert_eq!(out.glyphs[3].glyph_id, 6);
    }

    #[test]
    fn gsub_chain_context_depth_guard_bottoms_out() {
        // Self-referential chained-context: lookup 0 is a chain-
        // context fmt 3 whose nested lookup is 0 itself. Without a
        // depth guard this would infinitely recurse and overflow the
        // stack; with the guard, it must bottom out harmlessly.
        //
        // The subtable matches any single glyph at pos i (input
        // coverage = {all glyphs 1..=6}), no backtrack/lookahead,
        // and fires lookup 0 at seq 0 — the lookup itself.
        let mut chain = Vec::new();
        chain.extend_from_slice(&3u16.to_be_bytes()); // format
        chain.extend_from_slice(&0u16.to_be_bytes()); // backtrack count
        chain.extend_from_slice(&1u16.to_be_bytes()); // input count
        chain.extend_from_slice(&0u16.to_be_bytes()); // input cov slot
        chain.extend_from_slice(&0u16.to_be_bytes()); // lookahead count
        chain.extend_from_slice(&1u16.to_be_bytes()); // lookup count
        chain.extend_from_slice(&0u16.to_be_bytes()); // seq
        chain.extend_from_slice(&0u16.to_be_bytes()); // lookup list index = self
        let in_slot = 4;
        let cov_off = chain.len();
        chain.extend_from_slice(&build_cov_fmt1(&[1, 2, 3, 4, 5, 6]));
        chain[in_slot..in_slot + 2].copy_from_slice(&(cov_off as u16).to_be_bytes());

        let data = build_shapeable_font_with_gsub(&[(6, chain)], &[0]);
        let blob = Blob::new(&data);
        let face = Face::parse(&blob, 0).unwrap();
        let font = Font::new(face, 16.0);
        let features = [Feature {
            tag: *b"test",
            value: 1,
        }];

        let mut buf = Buffer::new();
        buf.push_str("AB");
        // If the depth guard works, this returns without stack
        // overflow and leaves the glyphs untouched (every nested
        // dispatch is itself a chain-context that does not ultimately
        // perform any concrete substitution).
        let out = shape(&font, &buf, &features).unwrap();
        assert_eq!(out.glyphs[0].glyph_id, 1);
        assert_eq!(out.glyphs[1].glyph_id, 2);
    }
}
