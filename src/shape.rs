//! The public shaping entry point.
//!
//! # Pipeline
//!
//! ```text
//!   buffer.text  ->  split into chars (cluster = UTF-8 byte offset)
//!                ->  normalize against the font's cmap (decompose, reorder
//!                    marks, recompose), mapping each char to its glyph id
//!                    (.notdef when missing)
//!                ->  hmtx.advance(gid)  (advance in font design units)
//!                ->  Glyph { glyph_id, cluster, x_advance, ... }
//! ```
//!
//! Advances are emitted in the font's design units (i.e. the grid
//! defined by `head.unitsPerEm`). Callers that want pixels can scale
//! by `font.size() / font.units_per_em()` at render time. Keeping the
//! shaper output in design units matches `rustybuzz`'s default and
//! preserves determinism: every intermediate value is an integer.
//!
//! # What is here
//!
//! - HarfBuzz's font-aware normalization, which also maps characters
//!   to glyphs (the `normalize` submodule): clusters decompose into
//!   what the font supports, marks sort by combining class, and base
//!   and mark pairs recompose when the font has the composite, with
//!   the mode and hooks of the shaper HarfBuzz picks for the script
//!   (the `shaper` submodule). Then the full shaping pipeline in spec
//!   order.
//! - GSUB lookup types 1 through 8, with Extension (type 7)
//!   unwrapped: `ccmp`, `rlig`, `liga`, `clig`, `calt` run by
//!   default; any user-enabled tag with non-zero value flows
//!   through the same dispatcher. Context and chained-context
//!   lookups invoke nested lookups at positions inside the match
//!   window, up to `MAX_NESTED_DEPTH` levels deep.
//! - hmtx advance lookup, post-substitution so ligature glyphs get
//!   their own advance rather than the sum of their components.
//! - GPOS (lookup types 1 through 8, plus Extension type 9
//!   unwrapping) as one HarfBuzz-style stage: the lookups of `abvm`,
//!   `blwm`, `mark`, `mkmk` (every run), `curs`, `dist`, `kern`
//!   (horizontal runs) and any user-enabled tag run once each, in
//!   lookup-list order (see the `gpos` submodule). Mark attachment
//!   finds its base, ligature component, or previous mark with the
//!   ligature component ids GSUB records (the `lig` submodule).
//!   Attachments are resolved into final offsets in one
//!   direction-aware pass after all positioning (the `attach`
//!   submodule).
//! - Legacy `kern` and AAT `kerx` pair kerning when GPOS has no `kern`
//!   feature for the run (Open Sans is the canonical example), split
//!   across each pair the way HarfBuzz does (the `kern` submodule),
//!   and HarfBuzz's mark-width zeroing per script (the `position`
//!   submodule).
//! - HarfBuzz's fallback positioning (the `fallback` submodule): the
//!   widths of space characters drawn with the space glyph, and, when
//!   no GPOS, `kerx`, or cross-stream `kern` table positions the run,
//!   marks placed from their combining classes and glyph extents.
//! - AAT `morx` substitution for fonts without GSUB.
//!
//! Any default-on feature can be suppressed by a `Feature { tag,
//! value: 0 }` entry.
//!
//! # Direction and output order
//!
//! The contract matches HarfBuzz's `hb_shape`. Every pass (GSUB,
//! GPOS, kerning) runs over the glyphs in logical order. For the
//! backward directions ([`crate::Direction::Rtl`] and
//! [`crate::Direction::Btt`]) the glyph vector is reversed as the very
//! last step, so the returned `ShapedRun` always holds visual order and
//! the offsets are relative to that order: an RTL run comes out
//! leftmost glyph first, byte-for-byte what HarfBuzz and rustybuzz
//! return. Forward directions ([`crate::Direction::Ltr`],
//! [`crate::Direction::Ttb`]) come out in logical order. Vertical runs
//! report negative `y_advance` values in both TTB and BTT, with every
//! glyph moved from its vertical origin to its horizontal one before
//! GPOS, as HarfBuzz does.
//!
//! An explicit direction that is not the script's native one (Arabic
//! or Hebrew in an LTR buffer, Latin in an RTL one, any BTT buffer)
//! means, as in HarfBuzz, that the text is already in that visual
//! order: the graphemes are reversed and shaped in the native
//! direction (see the `native_direction` submodule).
//!
//! When the caller never set a direction
//! ([`crate::Buffer::has_explicit_direction`] is false) the buffer
//! shapes as LTR, except that a Mongolian-dominant run switches to
//! vertical top-to-bottom layout. An explicit
//! [`crate::Direction::Ltr`] keeps Mongolian horizontal.
//!
//! # Clusters and buffer flags
//!
//! Every glyph starts with the UTF-8 offset of its character as its
//! cluster. The buffer's [`crate::ClusterLevel`] then decides, at each
//! place HarfBuzz forms or merges clusters, whether that happens:
//! grapheme forming before shaping, the merge of each reversed
//! grapheme in a non-native direction, ligatures, the Indic, Khmer,
//! Myanmar, and USE reorderings, Thai and Lao SARA AM, Old Hangul jamo
//! sequences, and deleted default ignorables (see the `cluster`
//! submodule). The [`crate::BufferFlags`] add HarfBuzz's dotted circle
//! at the start of a paragraph (`BOT`), turn dotted circles off, and
//! keep or remove default-ignorable glyphs instead of hiding them.
//!
//! # Limits
//!
//! Hostile fonts can nest lookups or chain multiple substitutions
//! so that the work grows exponentially. Every lookup one `shape()`
//! call applies therefore shares one `LookupBudget`: a cap on nested
//! lookup calls and on how far multiple substitution may grow the
//! run, sized like HarfBuzz's `max_ops` and `max_len`. Features the
//! `ot` pre-shapers apply get a smaller budget each. Well-formed
//! fonts stay far below the caps.
//!
//! # What is not here
//!
//! - Automatic direction detection: an unset direction shapes as LTR
//!   even for Arabic or Hebrew text. Set [`crate::Direction::Rtl`]
//!   explicitly to get HarfBuzz's RTL behavior and visual order.

mod aat;
mod attach;
mod cluster;
mod dotted_circle;
mod fallback;
mod features;
mod glyph_props;
mod gpos;
mod gsub;
mod gsub_parsed;
mod hangul;
mod ignorables;
mod joiners;
mod kern;
mod lig;
mod native_direction;
mod normalize;
mod pipeline;
mod position;
mod required;
mod rotate;
mod segment;
mod shaper;
mod syllabic;
mod thai;

use aat::apply_kerx_format4;
pub(crate) use cluster::{merge_clusters, merge_grapheme_clusters};
pub(crate) use features::{
    apply_gsub_feature_in_scripts, apply_gsub_feature_masked, apply_gsub_features_merged,
    apply_locl_ccmp_if_length_preserving, feature_would_substitute,
};
use gsub::apply_gsub_lookup;
use gsub_parsed::filter_for_lookup;
pub(crate) use joiners::JoinerTable;
pub use pipeline::shape;
use segment::ProcessedSegment;
pub(crate) use syllabic::SyllabicGsub;

use crate::buffer::Glyph;
use crate::tables::gpos::resolve_variation_delta;
use crate::tables::variation_store::ItemVariationStore;

/// Variable-font context threaded through every GPOS apply site.
///
/// Decoupling this from the GPOS tables themselves means every
/// apply function keeps the same shape for variable and static
/// fonts. With empty coords or no store every resolver
/// short-circuits to zero without reading the store.
#[derive(Debug, Clone, Copy)]
struct VarCtx<'a> {
    /// Normalized axis coords. Empty for the default instance.
    coords: &'a [f32],
    /// GDEF's shared `ItemVariationStore`. Required for every
    /// `VariationIndex` the ValueRecord's device slots point at.
    store: Option<&'a ItemVariationStore<'a>>,
}

impl VarCtx<'_> {
    /// A static-instance context: no coords, no store. Every resolver
    /// produces a zero delta. Unit tests use it to call a GPOS apply
    /// site without a font.
    #[cfg(test)]
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

/// Maximum recursion depth for nested-lookup dispatch. Matches the
/// limit HarfBuzz uses (`HB_MAX_NESTING_LEVEL = 16`); any deeper and
/// we assume the font is pathological (a cycle in the LookupList)
/// and stop rather than overflow the stack.
const MAX_NESTED_DEPTH: u8 = 16;

/// Nested lookup calls allowed per input glyph. Mirrors HarfBuzz's
/// `HB_BUFFER_MAX_OPS_FACTOR`. Real fonts nest a handful of lookups
/// per glyph at most.
const NESTED_OPS_PER_GLYPH: usize = 64;

/// Floor for the nested-call budget of one [`shape`] call, so short
/// runs still get room. Mirrors HarfBuzz's `HB_BUFFER_MAX_OPS_MIN`.
const NESTED_OPS_MIN: usize = 16_384;

/// Floor for the nested-call budget of one standalone feature
/// application (the `ot` pre-shapers call those once per feature and
/// sometimes once per syllable, so the floor stays small).
const NESTED_OPS_MIN_STANDALONE: usize = 1024;

/// How far multiple substitution may grow the run: this many glyphs
/// per input glyph. Mirrors HarfBuzz's `HB_BUFFER_MAX_LEN_FACTOR`.
const MAX_LEN_FACTOR: usize = 64;

/// Floor for the run length one [`shape`] call may grow to. Mirrors
/// HarfBuzz's `HB_BUFFER_MAX_LEN_MIN`.
const MAX_LEN_MIN: usize = 16_384;

/// Work limits for lookup application.
///
/// Depth alone does not stop a hostile font: a context rule with k
/// nested records that point back at its own lookup makes k^16 calls
/// before the depth cap bites, and a feature of lookups that each
/// double the run grows it exponentially. The budget bounds both.
/// When it runs out, further nested calls are skipped and further
/// multiple substitutions are refused, as if their subtables had
/// not matched. Top-level lookups keep running, each a single pass
/// over the run.
#[derive(Debug)]
struct LookupBudget {
    /// Nested lookup calls left.
    nested_ops_left: usize,
    /// Glyphs multiple substitution may still add to the run.
    growth_left: usize,
}

impl LookupBudget {
    /// Budget shared by every lookup one [`shape`] call applies over
    /// a run of `input_len` glyphs.
    fn for_shape(input_len: usize) -> Self {
        let max_len = input_len.saturating_mul(MAX_LEN_FACTOR).max(MAX_LEN_MIN);
        Self {
            nested_ops_left: input_len
                .saturating_mul(NESTED_OPS_PER_GLYPH)
                .max(NESTED_OPS_MIN),
            growth_left: max_len.saturating_sub(input_len),
        }
    }

    /// Budget for one standalone feature application over `glyphs`,
    /// used by the `pub(crate)` entry points the `ot` pre-shapers call.
    ///
    /// Those callers apply many features in a row, each with a fresh
    /// budget, so the growth cap must not compound. It is tied to the
    /// span of cluster values (source byte offsets), which multiple
    /// substitution copies and never widens, rather than to the
    /// current length.
    fn for_run(glyphs: &[Glyph]) -> Self {
        let span = match (
            glyphs.iter().map(|g| g.cluster).min(),
            glyphs.iter().map(|g| g.cluster).max(),
        ) {
            (Some(lo), Some(hi)) => ((hi - lo) as usize).saturating_add(1),
            _ => 1,
        };
        Self {
            nested_ops_left: glyphs
                .len()
                .saturating_mul(NESTED_OPS_PER_GLYPH)
                .max(NESTED_OPS_MIN_STANDALONE),
            growth_left: span
                .saturating_mul(MAX_LEN_FACTOR)
                .saturating_sub(glyphs.len()),
        }
    }

    /// Spends one nested call. False once the budget is gone.
    fn take_nested_op(&mut self) -> bool {
        if self.nested_ops_left == 0 {
            return false;
        }
        self.nested_ops_left -= 1;
        true
    }

    /// True once no nested call is left.
    fn exhausted(&self) -> bool {
        self.nested_ops_left == 0
    }

    /// Spends room for `extra` new glyphs. False, spending nothing,
    /// when the run may not grow that much.
    fn take_growth(&mut self, extra: usize) -> bool {
        match self.growth_left.checked_sub(extra) {
            Some(left) => {
                self.growth_left = left;
                true
            }
            None => false,
        }
    }
}

/// One entry in a feature list passed to [`shape`]. The tag is a
/// four-byte OpenType feature tag (e.g. `b"liga"`, `b"kern"`, `b"smcp"`);
/// the value is interpreted per-feature: typically `0` disables and
/// any non-zero value enables.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Feature {
    /// Four-byte feature tag.
    pub tag: [u8; 4],
    /// Feature value. Zero means off; non-zero means on (or a
    /// feature-specific selector for alternates).
    pub value: u32,
}

/// Returns `true` when the feature is explicitly disabled via
/// `Feature { tag, value: 0 }` in the override list.
fn feature_disabled(features: &[Feature], tag: [u8; 4]) -> bool {
    features.iter().any(|f| f.tag == tag && f.value == 0)
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
/// The inner offset is u32. That is why Extension exists, to reach
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

#[cfg(test)]
mod tests;
