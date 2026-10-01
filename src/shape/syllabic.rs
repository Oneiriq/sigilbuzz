//! GSUB lookups for the syllable-based shapers
//! ([`crate::ot::syllabic`]).
//!
//! HarfBuzz applies a lookup at a glyph only when the glyph's mask has
//! one of the bits of the features the lookup belongs to
//! (`apply_forward` and `apply_backward` in `hb-ot-layout-gsubgpos.hh`),
//! and a feature flagged `F_PER_SYLLABLE` only matches glyphs of the
//! cursor's syllable. The shapers keep masks beside the glyphs and
//! syllables in [`Glyph::syllable`]. [`SyllabicGsub::apply_lookup`]
//! turns the mask predicate into the per-glyph mask of the GSUB
//! buffer and runs one lookup with it, so the cursor stops only at
//! glyphs the feature is on at, the other input glyphs a rule matches
//! must have it on too, and a per-syllable lookup never reaches into a
//! neighboring syllable.

use alloc::vec::Vec;

use super::gsub::{apply_gsub_lookup, apply_gsub_lookups_masked};
use super::joiners::FeatureFlags;
use super::LookupBudget;
use crate::buffer::Glyph;
use crate::tables::gdef::Gdef;
use crate::tables::layout::Joiners;
use crate::tables::Gsub;

/// Applies GSUB lookups for one run of a syllable-based shaper, under
/// one [`LookupBudget`] for the whole run.
pub(crate) struct SyllabicGsub<'a> {
    gsub: &'a Gsub<'a>,
    gdef: Option<&'a Gdef<'a>>,
    budget: LookupBudget,
}

impl<'a> SyllabicGsub<'a> {
    /// A runner for the run `glyphs`, whose budget the length and
    /// clusters of `glyphs` size.
    pub(crate) fn new(gsub: &'a Gsub<'a>, gdef: Option<&'a Gdef<'a>>, glyphs: &[Glyph]) -> Self {
        Self {
            gsub,
            gdef,
            budget: LookupBudget::for_run(glyphs),
        }
    }

    /// The table the runner applies.
    pub(crate) fn gsub(&self) -> &'a Gsub<'a> {
        self.gsub
    }

    /// Applies lookup `index` to every glyph of `glyphs`, picking
    /// alternate `alternate` (0-based) in alternate substitutions.
    pub(crate) fn apply_lookup_alternate(
        &mut self,
        index: u16,
        alternate: u16,
        glyphs: &mut Vec<Glyph>,
    ) {
        apply_gsub_lookup(
            self.gsub,
            index,
            glyphs,
            self.gdef,
            alternate,
            FeatureFlags::AUTO,
            &mut self.budget,
        );
    }

    /// Applies lookup `index` to `glyphs`, with the feature's joiner
    /// handling. The lookup applies at a glyph only when `applies`
    /// accepts it. With `per_syllable`, it matches only glyphs whose
    /// [`Glyph::syllable`] is the cursor glyph's.
    pub(crate) fn apply_lookup(
        &mut self,
        index: u16,
        joiners: Joiners,
        per_syllable: bool,
        glyphs: &mut Vec<Glyph>,
        applies: &dyn Fn(&Glyph) -> bool,
    ) {
        if glyphs.is_empty() {
            return;
        }
        let mask: Vec<bool> = glyphs.iter().map(applies).collect();
        let flags = FeatureFlags {
            joiners,
            per_syllable,
        };
        apply_gsub_lookups_masked(
            self.gsub,
            &[index],
            glyphs,
            self.gdef,
            &mask,
            flags,
            &mut self.budget,
        );
    }
}
