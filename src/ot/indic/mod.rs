//! Indic2 reordering shaper.
//!
//! Indic scripts (Devanagari, Bengali, Gurmukhi, ...) are written
//! phonetically but laid out with a richer set of positional rules
//! than Latin. A pre-base vowel sign is typed after its consonant
//! but must render before it; `ra + halant` at the start of a
//! syllable becomes a "reph" that renders above the last consonant;
//! conjunct consonants — half-forms, below-base forms, post-base
//! forms — are selected by font-declared GSUB features that run in
//! a specific order.
//!
//! The HarfBuzz implementation of the Indic2 shaper is the
//! reference; sigilbuzz follows the same phase structure:
//!
//! ```text
//!   1. Segment the buffer into syllables.
//!   2. For each syllable: initial reordering.
//!      - Matra decomposition.
//!      - Classify consonants (half-form, below-base, post-base, ...).
//!      - Reorder pre-base matras to before the base consonant.
//!      - Mark `ra + halant` as reph and move to the reordering slot.
//!   3. Apply basic features in order: `nukt`, `akhn`, `rphf`,
//!      `blwf`, `half`, `pstf`, `vatu`, `cjct`.
//!   4. Final reordering.
//!      - Reph to its font-declared position.
//!      - Pre-base matras to their visual slot.
//!   5. Apply presentation features: `init`, `pres`, `abvs`,
//!      `blws`, `psts`, `haln`. (These run through the generic
//!      GSUB pass after the Indic pipeline returns.)
//! ```
//!
//! Only Devanagari ships at M4. Other Indic scripts reuse the state
//! machine; they just need their ISC/IPC tables populated and the
//! script hook added.

pub mod devanagari;

pub use devanagari::shape_devanagari;
