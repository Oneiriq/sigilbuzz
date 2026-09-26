//! Rewriter for GPOS type 8 (chained context positioning).

use crate::layout::{RewriterCtx, RewrittenSubtable};

/// Rewrites a GPOS type 8 (Chained Context Positioning) subtable,
/// formats 1 / 2 / 3. Shares the GSUB type 6 rewriter.
pub(super) fn rewrite_chain_context_pos(
    ctx: &RewriterCtx,
    sub: &[u8],
) -> Option<RewrittenSubtable> {
    crate::gsub::rewrite_type6(ctx, sub)
}
