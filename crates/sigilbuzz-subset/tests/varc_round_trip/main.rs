//! VARC subset round-trip integration tests.
//!
//! Builds the same synthetic 3-glyph VARC font that `tests/varc_synthetic.rs`
//! exercises in the parser crate (gid 0 .notdef, gid 1 VARC composite of
//! gid 2, gid 2 a 100x100 square), then runs the subsetter and asserts:
//!
//! - Closure expansion pulls gid 2 into the kept set when the caller asks
//!   only for gid 1.
//! - The output VARC has one coverage entry pointing at the renumbered
//!   gid 1.
//! - Each component gid in the output VARC matches the new gid map.
//! - The output font's gid 1 outline at default coords matches the
//!   source's gid 1 outline.
//! - When the kept set has only gid 2 (a base glyph not VARC-covered),
//!   the VARC table is omitted from the output entirely.

mod basic;
mod fixtures;
mod mvs;
mod region_list;
