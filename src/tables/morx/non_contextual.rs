//! `morx` type 4: non-contextual glyph substitution subtables.

use super::contextual::lookup_value;

// --- Type 4: Non-Contextual Substitution ---

/// Walks every glyph in the run and replaces it with whatever the
/// subtable's AAT lookup yields. A glyph the lookup does not cover, or
/// a lookup that errors on a malformed slice, keeps the original glyph,
/// so a partly-broken subtable can't blank out the run.
pub(super) fn apply_non_contextual(lookup: &[u8], glyphs: &mut [u16]) {
    for slot in glyphs.iter_mut() {
        if let Ok(Some(replacement)) = lookup_value(lookup, *slot) {
            *slot = replacement;
        }
    }
}
