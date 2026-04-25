//! `sigilbuzz-text-layout` — line-breaking and word-wrap for sigilbuzz.
//!
//! This companion crate implements a curated subset of
//! [UAX #14 *Unicode Line Breaking Algorithm*][uax14] sufficient to
//! wrap English, other European scripts, and CJK text correctly. This
//! initial scaffold ships only the line-break-class classifier
//! ([`LineBreakClass`] + `line_break_class`); subsequent commits land
//! the UAX 14 iterator, width-budget wrapper, and a UAX 29
//! word-segmentation iterator.
//!
//! [uax14]: https://www.unicode.org/reports/tr14/

#![cfg_attr(not(feature = "std"), no_std)]
#![warn(missing_docs)]
#![forbid(unsafe_op_in_unsafe_fn)]

extern crate alloc;

mod class;

pub use class::{line_break_class, LineBreakClass};
