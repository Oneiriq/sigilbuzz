//! `sigilbuzz-text-layout` — line-breaking and word-wrap for sigilbuzz.
//!
//! This companion crate implements a curated subset of
//! [UAX #14 *Unicode Line Breaking Algorithm*][uax14] sufficient to
//! wrap English, other European scripts, and CJK text correctly. This
//! commit adds the [`LineBreakIter`] state machine on top of the
//! line-break-class classifier; the width-budget wrapper and the UAX
//! 29 word-segmentation iterator land in subsequent commits.
//!
//! [uax14]: https://www.unicode.org/reports/tr14/

#![cfg_attr(not(feature = "std"), no_std)]
#![warn(missing_docs)]
#![forbid(unsafe_op_in_unsafe_fn)]

extern crate alloc;

mod class;
mod linebreak;

pub use class::{line_break_class, LineBreakClass};
pub use linebreak::{line_break_opportunities, BreakOpportunity, LineBreakIter};
