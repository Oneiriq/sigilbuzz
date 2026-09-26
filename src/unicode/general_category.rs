//! The parts of `General_Category` and `Extended_Pictographic` that
//! HarfBuzz's grapheme and native-direction rules and its synthesized
//! glyph classes read.
//!
//! The tables in `general_category_table.rs` are generated from Unicode
//! 17.0.0 `DerivedGeneralCategory.txt` and `emoji-data.txt` (snapshots
//! in `tests/tools/ucd/`; regenerate with
//! `cargo test --test unicode_table_gen -- --ignored`).

use super::general_category_table::{CLASSES, EXTENDED_PICTOGRAPHIC, NONSPACING_MARKS};

/// A coarse General_Category class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeneralCategoryClass {
    /// Lu, Ll, Lt, Lm, or Lo.
    Letter,
    /// Mn, Mc, or Me.
    Mark,
    /// Nd.
    DecimalNumber,
}

/// Index of the inclusive range in `ranges` that holds `cp`.
fn find<T>(ranges: &[(u32, u32, T)], cp: u32) -> Option<usize> {
    ranges
        .binary_search_by(|&(start, end, _)| {
            if end < cp {
                core::cmp::Ordering::Less
            } else if start > cp {
                core::cmp::Ordering::Greater
            } else {
                core::cmp::Ordering::Equal
            }
        })
        .ok()
}

/// The [`GeneralCategoryClass`] of `ch`, or `None` for every other
/// category (punctuation, symbols, separators, format characters, ...).
///
/// # Examples
///
/// ```
/// use sigilbuzz::unicode::general_category::{general_category_class, GeneralCategoryClass};
///
/// assert_eq!(general_category_class('a'), Some(GeneralCategoryClass::Letter));
/// assert_eq!(general_category_class('\u{0301}'), Some(GeneralCategoryClass::Mark));
/// assert_eq!(general_category_class('\u{0663}'), Some(GeneralCategoryClass::DecimalNumber));
/// assert_eq!(general_category_class(','), None);
/// ```
#[must_use]
pub fn general_category_class(ch: char) -> Option<GeneralCategoryClass> {
    find(CLASSES, u32::from(ch)).map(|i| CLASSES[i].2)
}

/// True for `Extended_Pictographic` characters (most emoji).
///
/// # Examples
///
/// ```
/// use sigilbuzz::unicode::general_category::is_extended_pictographic;
///
/// assert!(is_extended_pictographic('\u{1F600}'));
/// assert!(!is_extended_pictographic('a'));
/// ```
#[must_use]
pub fn is_extended_pictographic(ch: char) -> bool {
    in_ranges(EXTENDED_PICTOGRAPHIC, ch)
}

/// True for nonspacing marks (General_Category Mn), the characters
/// HarfBuzz's synthesized glyph classes treat as marks.
///
/// # Examples
///
/// ```
/// use sigilbuzz::unicode::general_category::is_nonspacing_mark;
///
/// assert!(is_nonspacing_mark('\u{0301}')); // combining acute accent
/// assert!(!is_nonspacing_mark('\u{0903}')); // Devanagari visarga, Mc
/// assert!(!is_nonspacing_mark('a'));
/// ```
#[must_use]
pub fn is_nonspacing_mark(ch: char) -> bool {
    in_ranges(NONSPACING_MARKS, ch)
}

/// True when `ch` falls in one of the sorted inclusive `ranges`.
fn in_ranges(ranges: &[(u32, u32)], ch: char) -> bool {
    let cp = u32::from(ch);
    ranges
        .binary_search_by(|&(start, end)| {
            if end < cp {
                core::cmp::Ordering::Less
            } else if start > cp {
                core::cmp::Ordering::Greater
            } else {
                core::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classes_cover_letters_marks_and_digits() {
        use GeneralCategoryClass::{DecimalNumber, Letter, Mark};
        assert_eq!(general_category_class('Z'), Some(Letter));
        assert_eq!(general_category_class('\u{0628}'), Some(Letter));
        assert_eq!(general_category_class('\u{02B0}'), Some(Letter)); // Lm
        assert_eq!(general_category_class('\u{0903}'), Some(Mark)); // Mc
        assert_eq!(general_category_class('\u{20DD}'), Some(Mark)); // Me
        assert_eq!(general_category_class('7'), Some(DecimalNumber));
        assert_eq!(general_category_class(' '), None);
        assert_eq!(general_category_class('\u{200D}'), None); // Cf
        assert_eq!(general_category_class('\u{0378}'), None); // unassigned
    }

    #[test]
    fn tables_are_sorted() {
        assert!(CLASSES.windows(2).all(|w| w[0].1 < w[1].0));
        assert!(EXTENDED_PICTOGRAPHIC.windows(2).all(|w| w[0].1 < w[1].0));
        assert!(NONSPACING_MARKS.windows(2).all(|w| w[0].1 < w[1].0));
    }
}
