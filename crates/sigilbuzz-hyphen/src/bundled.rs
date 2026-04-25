//! Bundled language pattern sets.
//!
//! Each pattern bundle is gated behind a cargo feature so that
//! consumers who only need one language do not pay the binary cost of
//! the others. Pattern files live under `crates/sigilbuzz-hyphen/
//! patterns/` and are baked in via [`include_str!`].
//!
//! Patterns parse on first use and are cached for the lifetime of the
//! process — see [`Patterns::for_language`].
//!
//! [`Patterns::for_language`]: super::Patterns::for_language

use crate::pattern::Patterns;

/// Selector for a bundled language pattern set.
///
/// Each variant is gated on the corresponding cargo feature; turning a
/// feature off both removes the pattern bytes from the binary *and*
/// removes the variant from this enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
    /// American English (`en-us`). Source: hyph-en-us.tex by Gerard
    /// D.C. Kuiken.
    #[cfg(feature = "patterns-en-us")]
    EnglishUs,
    /// German (`de`). Reserved — no patterns vendored yet; the variant
    /// exists so callers can future-proof their `match` arms.
    #[cfg(feature = "patterns-de")]
    German,
    /// French (`fr`). Reserved — no patterns vendored yet.
    #[cfg(feature = "patterns-fr")]
    French,
    /// Spanish (`es`). Reserved — no patterns vendored yet.
    #[cfg(feature = "patterns-es")]
    Spanish,
}

/// The raw en-us pattern bytes, baked into the binary at build time.
#[cfg(feature = "patterns-en-us")]
pub const PATTERNS_EN_US: &str = include_str!("../patterns/en-us.txt");

#[cfg(feature = "patterns-en-us")]
fn en_us_patterns() -> &'static Patterns {
    use std::sync::OnceLock;
    static CELL: OnceLock<Patterns> = OnceLock::new();
    CELL.get_or_init(|| {
        let mut p = Patterns::parse(PATTERNS_EN_US)
            .expect("bundled en-us patterns must parse");
        // hyph-en-us.tex declares hyphenmins typesetting left=2, right=3.
        p.left_min = 2;
        p.right_min = 3;
        p
    })
}

impl Patterns {
    /// Load a pre-bundled language pattern set.
    ///
    /// Returns `None` if the requested language has no patterns
    /// vendored in this build (e.g. `Language::German` without the
    /// `patterns-de` feature would not even compile, but reserved
    /// variants without bundled data still return `None`).
    ///
    /// The first call for a given language parses the pattern bytes
    /// and caches the compiled [`Patterns`]; subsequent calls return
    /// the cached reference.
    #[must_use]
    pub fn for_language(lang: Language) -> Option<&'static Self> {
        match lang {
            #[cfg(feature = "patterns-en-us")]
            Language::EnglishUs => Some(en_us_patterns()),
            // Reserved variants without vendored data fall through to
            // `None`. The match is exhaustive; the catchall is needed
            // only when at least one reserved variant is enabled.
            #[cfg(any(
                feature = "patterns-de",
                feature = "patterns-fr",
                feature = "patterns-es"
            ))]
            _ => None,
        }
    }
}

#[cfg(all(test, feature = "patterns-en-us"))]
mod tests {
    use super::*;
    use crate::algorithm::hyphenate;

    #[test]
    fn en_us_loads() {
        let p = Patterns::for_language(Language::EnglishUs).unwrap();
        assert!(p.len() > 4000, "expected ~4938 patterns, got {}", p.len());
        assert_eq!(p.left_min, 2);
        assert_eq!(p.right_min, 3);
    }

    #[test]
    fn en_us_cached() {
        let a = Patterns::for_language(Language::EnglishUs).unwrap();
        let b = Patterns::for_language(Language::EnglishUs).unwrap();
        assert!(core::ptr::eq(a, b));
    }

    /// Liang's canonical demonstration: "hyphenation" => "hy-phen-ation",
    /// breaks at byte offsets 2, 6.
    ///
    /// Matches the reference output produced by pyphen / libhyphen
    /// against the same `hyph-en-us.tex` pattern set; the `hena4`
    /// rule blocks the otherwise-tempting `a-tion` cut.
    #[test]
    fn hyphenation_canonical() {
        let p = Patterns::for_language(Language::EnglishUs).unwrap();
        assert_eq!(hyphenate("hyphenation", p), vec![2, 6]);
    }
}
