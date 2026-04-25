//! GPOS byte-level rewriter.
//!
//! As of this commit the GPOS rewriter ships no per-lookup-type
//! rewriters — every lookup drops. The drop cascade in
//! [`crate::layout`] then removes empty subtables, lookups, features,
//! and scripts. GPOS as a whole drops when no lookup survives.
//!
//! The rewriter is wired through the same [`super::layout`]
//! infrastructure GSUB uses, so the per-type rewriters slot in here
//! one at a time without needing to re-thread the call site.
//!
//! Issue tracking the GPOS lookup-type rewriters: see the sibling
//! issue filed alongside this module.

use crate::layout::{RewrittenLookup, RewriterCtx};

/// Rewrites a single GPOS lookup. Returns `None` while no lookup type
/// has a byte-level rewriter — the drop cascade handles the rest.
pub(crate) fn rewrite_lookup(
    _ctx: &RewriterCtx,
    _lookup_type: u16,
    _lookup_flag: u16,
    _mark_filtering_set: Option<u16>,
    _subtable_bodies: &[&[u8]],
) -> Option<RewrittenLookup> {
    // Stub. Drop everything until per-type rewriters land.
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::GidMap;
    use alloc::{vec, vec::Vec};

    #[test]
    fn rewrite_lookup_drops_everything() {
        let map = GidMap::from_table(vec![Some(0)]);
        let ctx = RewriterCtx { gid_map: &map };
        let dummy: Vec<&[u8]> = vec![&[0u8; 4]];
        assert!(rewrite_lookup(&ctx, 1, 0, None, &dummy).is_none());
    }
}
