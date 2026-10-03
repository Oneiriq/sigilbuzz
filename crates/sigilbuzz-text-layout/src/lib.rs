//! `sigilbuzz-text-layout`: line breaking, word wrap, and word
//! boundaries for sigilbuzz.
//!
//! This companion crate implements the
//! [UAX #14 *Unicode Line Breaking Algorithm*][uax14] (revision 55,
//! Unicode 17.0.0) and the word boundary rules of
//! [UAX #29 *Unicode Text Segmentation*][uax29] (revision 47), from
//! tables generated out of the Unicode Character Database. Both pass
//! every case of the Unicode conformance files `LineBreakTest.txt` and
//! `WordBreakTest.txt`.
//!
//! The headline entry points are:
//!
//! - [`line_break_opportunities`]: the UAX 14 break iterator over a
//!   `&str`, and [`line_break_opportunities_with`] for the CSS
//!   `word-break` tailorings in [`WordBreak`] (`keep-all` breaks Korean
//!   between words instead of syllables, `break-all` breaks inside any
//!   word).
//! - [`wrap_lines`]: walks a slice of shaped [`sigilbuzz::Glyph`]s
//!   and a width budget to produce [`LineRange`]s.
//! - [`word_breaks`]: the UAX 29 word boundary iterator, for cursor
//!   movement and double-click selection.
//! - [`line_break_class`]: the `Line_Break` property of a character.
//!
//! `wrap_lines` and the iterators need only `alloc`. The crate still
//! depends on `sigilbuzz` with its default `std` feature, so even with
//! the `std` feature off it needs a target with `std`.
//!
//! # Coverage
//!
//! Every UAX 14 rule is implemented, LB1 through LB31, including the
//! Korean syllable blocks of conjoining jamo (LB26, LB27), the Brahmic
//! orthographic syllables (LB28a), and regional indicator pairs
//! (LB30a). Southeast Asian scripts (class SA: Thai, Lao, Khmer,
//! Myanmar, and others) need a dictionary to find word boundaries,
//! which this crate does not have. They resolve to AL as LB1 directs,
//! so a run of them breaks only at spaces and punctuation. For the
//! same reason [`word_breaks`] finds a boundary after every character
//! of them, apart from combining marks.
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
#[rustfmt::skip]
mod word_break_table;
mod wrap;

pub use class::{line_break_class, LineBreakClass};
pub use linebreak::{
    line_break_opportunities, line_break_opportunities_with, BreakOpportunity, LineBreakIter,
    WordBreak,
};
pub use word::{word_breaks, WordBreakIter};
pub use wrap::{wrap_lines, LineRange, WrapOptions};
