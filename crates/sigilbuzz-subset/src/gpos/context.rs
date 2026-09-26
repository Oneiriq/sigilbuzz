//! Rewriter for GPOS type 7 (context positioning).

use crate::layout::{RewriterCtx, RewrittenSubtable};

// ===== GPOS types 7 / 8: contextual / chained-contextual positioning =====
//
// Structurally identical to GSUB types 5 / 6: the same three formats
// (glyph rule sets, class rule sets, coverage arrays) drive a list of
// `PosLookupRecord` entries that re-enter the GPOS dispatcher on a
// match. Each record is `u16 sequenceIndex, u16 lookupListIndex`; the
// second field is patched on the second pass through
// [`crate::layout::build_gpos`] once the GPOS lookup-list renumber is
// known. See [`context_lookup_type`] for the driver hook. Only the
// dispatcher target of the nested records differs, so the GSUB
// rewriters handle both tables.

/// Rewrites a GPOS type 7 (Context Positioning) subtable, formats 1 /
/// 2 / 3. Shares the GSUB type 5 rewriter.
pub(super) fn rewrite_context_pos(ctx: &RewriterCtx, sub: &[u8]) -> Option<RewrittenSubtable> {
    crate::gsub::rewrite_type5(ctx, sub)
}
