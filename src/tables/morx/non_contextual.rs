//! `morx` type 4: non-contextual glyph substitution subtables.

use super::contextual::lookup_value;
use crate::tables::layout::state_table::CLASS_OUT_OF_BOUNDS;

// --- Type 4: Non-Contextual Substitution ---

/// Walks every glyph in the run and replaces it with whatever the
/// subtable's AAT lookup yields. A lookup that returns
/// [`CLASS_OUT_OF_BOUNDS`] (the AAT "glyph not covered" sentinel) or
/// errors on a malformed slice falls through to "keep the original
/// glyph", so a partly-broken subtable can't blank out the run.
pub(super) fn apply_non_contextual(lookup: &[u8], glyphs: &mut [u16]) {
    for slot in glyphs.iter_mut() {
        if let Ok(replacement) = lookup_value(lookup, *slot) {
            if replacement != CLASS_OUT_OF_BOUNDS {
                *slot = replacement;
            }
        }
    }
}
