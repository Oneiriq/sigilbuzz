//! The public shaping entry point.
//!
//! # Pipeline
//!
//! ```text
//!   buffer.text  ->  split into chars (cluster = UTF-8 byte offset)
//!                ->  cmap.glyph_id(ch)  (falls back to .notdef when missing)
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
//! - cmap -> glyph id, then the full shaping pipeline in spec order.
//! - GSUB (lookup types 1, 4, 6 format 3, plus Extension type 7
//!   unwrapping): `ccmp`, `rlig`, `liga`, `clig`, `calt` run by
//!   default; any user-enabled tag with non-zero value flows
//!   through the same dispatcher. Chained-context lookups can
//!   invoke other lookups at specific positions inside the match
//!   window (the first recursive layer sigilbuzz supports).
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
//! # What is not here yet
//!
//! - Full Unicode NFC normalization. sigilbuzz ships the
//!   composition half of NFC (opt-in via
//!   [`crate::Buffer::set_normalize_nfc`]); canonical
//!   decomposition and combining-class reordering do not run yet,
//!   so pathological inputs that need reordering fall through
//!   unchanged.
//! - GSUB contextual non-chained (type 5), multiple substitution
//!   (type 2), alternate (type 3), reverse chained (type 8),
//!   and the format 1/2 variants of type 6.
//! - Automatic direction detection: an unset direction shapes as LTR
//!   even for Arabic or Hebrew text. Set [`crate::Direction::Rtl`]
//!   explicitly to get HarfBuzz's RTL behavior and visual order.
//! - The fallback mark positioner HarfBuzz uses for fonts without
//!   GPOS.

mod aat;
mod attach;
mod cluster;
mod dotted_circle;
mod features;
mod gpos;
mod gsub;
mod gsub_parsed;
mod hangul;
mod ignorables;
mod kern;
mod lig;
mod native_direction;
mod pipeline;
mod position;
mod required;
mod rotate;
mod segment;
mod thai;

use aat::apply_kerx_format4;
pub(crate) use cluster::{merge_clusters, merge_grapheme_clusters};
pub(crate) use features::{
    apply_gsub_feature_in_scripts, apply_gsub_feature_masked, apply_gsub_features_merged,
    apply_locl_ccmp_if_length_preserving, feature_would_substitute,
};
use gsub::apply_gsub_lookup;
use gsub_parsed::filter_for_lookup;
pub use pipeline::shape;
use segment::ProcessedSegment;

use crate::tables::gpos::resolve_variation_delta;
use crate::tables::variation_store::ItemVariationStore;

/// Variable-font context threaded through every GPOS apply site.
///
/// Decoupling this from the GPOS tables themselves means every
/// apply function keeps the same shape for variable and static
/// fonts; static callers pass [`VarCtx::none`] and every resolver
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
    /// Builds a static-instance context: no coords, no store.
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

/// Maximum recursion depth for nested-lookup dispatch. Matches the
/// limit HarfBuzz uses (`HB_MAX_NESTING_LEVEL = 16`); any deeper and
/// we assume the font is pathological (a cycle in the LookupList)
/// and stop rather than overflow the stack.
const MAX_NESTED_DEPTH: u8 = 16;

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
