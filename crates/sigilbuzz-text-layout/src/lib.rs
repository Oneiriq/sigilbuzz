//! `sigilbuzz-text-layout` — line-breaking and word-wrap for sigilbuzz.
//!
//! This companion crate implements a curated subset of
//! [UAX #14 *Unicode Line Breaking Algorithm*][uax14] sufficient to
//! wrap English, other European scripts, and CJK text correctly.
//!
//! - [`line_break_opportunities`] — UAX 14 break iterator over a `&str`.
//! - [`wrap_lines`] — walks a slice of shaped [`sigilbuzz::Glyph`]s
//!   and a width budget to produce [`LineRange`]s.
//!
//! [uax14]: https://www.unicode.org/reports/tr14/

#![cfg_attr(not(feature = "std"), no_std)]
#![warn(missing_docs)]
#![forbid(unsafe_op_in_unsafe_fn)]

extern crate alloc;

mod class;
mod linebreak;
mod wrap;

pub use class::{line_break_class, LineBreakClass};
pub use linebreak::{line_break_opportunities, BreakOpportunity, LineBreakIter};
pub use wrap::{wrap_lines, LineRange, WrapOptions};
