//! `sigilbuzz-text-layout`: line-breaking and word-wrap for sigilbuzz.
//!
//! This companion crate implements a curated subset of
//! [UAX #14 *Unicode Line Breaking Algorithm*][uax14] sufficient to
//! wrap English, other European scripts, and CJK text correctly. It
//! also offers a simplified [UAX #29][uax29] word-segmentation iterator
//! for callers that need word boundaries (cursor movement, double-click
//! selection) without pulling in a full Unicode segmentation crate.
//!
//! The headline entry points are:
//!
//! - [`line_break_opportunities`]: UAX 14 break iterator over a `&str`.
//! - [`wrap_lines`]: walks a slice of shaped [`sigilbuzz::Glyph`]s
//!   and a width budget to produce [`LineRange`]s.
//! - [`word_breaks`]: simplified UAX 29 word-segmentation iterator.
//!
//! `wrap_lines` and the iterators need only `alloc`. The crate still
//! depends on `sigilbuzz` with its default `std` feature, so even with
//! the `std` feature off it needs a target with `std`.
//!
//! # Coverage
//!
//! The line-break classifier covers the high-impact UAX 14 classes:
//! `BK`, `CR`, `LF`, `NL`, `WJ`, `CL`, `CP`, `OP`, `QU`, `GL`, `NS`,
//! `CM`, `SP`, `BA`, `BB`, `HY`, `AL`, `NU`, `PR`, `PO`, `ID`, `EX`,
//! `ZW`, `EB`, and `EM`. Brahmic combining marks, Korean Jamo
//! clustering, complex line-breaking for Southeast-Asian scripts, and
//! the UAX 14 LB30a regional-indicator pair logic are deferred to a
//! future release.
//!
//! [uax14]: https://www.unicode.org/reports/tr14/
//! [uax29]: https://www.unicode.org/reports/tr29/

#![cfg_attr(not(feature = "std"), no_std)]
#![warn(missing_docs)]
#![forbid(unsafe_op_in_unsafe_fn)]

extern crate alloc;

mod class;
#[rustfmt::skip]
mod line_break_table;
mod linebreak;
mod word;
mod wrap;

pub use class::{line_break_class, LineBreakClass};
pub use linebreak::{
    line_break_opportunities, line_break_opportunities_with, BreakOpportunity, LineBreakIter,
    WordBreak,
};
pub use word::word_breaks;
pub use wrap::{wrap_lines, LineRange, WrapOptions};
