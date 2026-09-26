//! `sigilbuzz-hyphen`: Liang/Knuth pattern-driven hyphenation.
//!
//! This companion crate implements [Liang's hyphenation algorithm][liang]
//! (PhD thesis, Stanford 1983; widely deployed in TeX, OpenOffice, web
//! browsers). The algorithm walks every contiguous *pattern* that
//! matches in a word, keeps the highest priority number at each
//! position, and treats odd numbers as valid break points.
//!
//! Patterns look like `hy3ph` ("after `hy`, before `ph`, strongly
//! encourage breaking") or `2tion` ("slightly discourage breaking
//! before `tion`"). Each language ships its own pattern set; the
//! classic en-us set (Liang's original, refined by Kuiken) is bundled
//! by default and parses on first use.
//!
//! # Headline entry points
//!
//! - [`hyphenate`]: the algorithm. Returns the byte offsets within a
//!   word at which a soft-hyphen break is permitted.
//! - [`Patterns::for_language`]: fetch a pre-bundled pattern set.
//! - [`Patterns::parse`]: parse a custom newline-separated pattern
//!   list (e.g. for a language not bundled in this crate).
//!
//! # Quick start
//!
//! ```
//! # #[cfg(feature = "patterns-en-us")] {
//! use sigilbuzz_hyphen::{hyphenate, Language, Patterns};
//!
//! let patterns = Patterns::for_language(Language::EnglishUs).unwrap();
//! let breaks = hyphenate("hyphenation", patterns);
//! assert_eq!(breaks, vec![2, 6]); // "hy-phen-ation"
//! # }
//! ```
//!
//! # Pattern licensing
//!
//! The bundled `en-us` pattern set is derived from Gerard D.C. Kuiken's
//! `hyph-en-us.tex` (TeX hyph-utf8 package). Its upstream notice
//! ("copying and distribution permitted, copyright notice preserved")
//! is compatible with this crate's Apache-2.0 grant. See
//! `patterns/LICENSE-en-us` in the source tree.
//!
//! [liang]: https://www.tug.org/docs/liang/

#![cfg_attr(not(feature = "std"), no_std)]
#![warn(missing_docs)]
#![forbid(unsafe_op_in_unsafe_fn)]

extern crate alloc;

mod algorithm;
mod bundled;
mod pattern;

#[cfg(feature = "text-layout-integration")]
mod text_layout;

pub use algorithm::hyphenate;
pub use bundled::Language;
pub use pattern::{ParseError, Patterns};

#[cfg(feature = "text-layout-integration")]
pub use text_layout::{break_opportunities_with_hyphens, HyphenatedBreak};
