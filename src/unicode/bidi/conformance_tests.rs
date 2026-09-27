//! Lines of the Unicode 17.0 bidi conformance files (BidiTest.txt and
//! BidiCharacterTest.txt) that pin rules X5a to X5c, X6, X8, N0, and
//! BD16.
//!
//! The full files pass except four BidiCharacterTest lines whose
//! characters the curated `bidi_class` table classifies differently
//! from the UCD (U+061C, U+002A, U+06F1).

use alloc::string::String;
use alloc::vec::Vec;

use super::reorder::is_x9_removed;
use super::*;

/// The levels [`BidiInfo`] resolves for `text`, `None` for the
/// characters rule X9 removes, and the visual order of the others.
fn resolve(text: &str, direction: Option<Direction>) -> (Vec<Option<u8>>, Vec<usize>) {
    let chars: Vec<char> = text.chars().collect();
    let info = BidiInfo::new(text, direction);
    let levels = chars
        .iter()
        .zip(info.levels())
        .map(|(&c, &l)| (!is_x9_removed(bidi_class(c))).then_some(l))
        .collect();
    let order = info
        .reorder()
        .into_iter()
        .filter(|&i| !is_x9_removed(bidi_class(chars[i])))
        .collect();
    (levels, order)
}

/// Parses a levels field (`x` for a removed character).
fn levels(field: &str) -> Vec<Option<u8>> {
    field.split_whitespace().map(|t| t.parse().ok()).collect()
}

/// Parses an order field.
fn order(field: &str) -> Vec<usize> {
    field
        .split_whitespace()
        .filter_map(|t| t.parse().ok())
        .collect()
}

/// A character of each class, for BidiTest.txt lines.
fn class_char(name: &str) -> char {
    match name {
        "L" => 'a',
        "R" => '\u{05D0}',
        "AL" => '\u{0627}',
        "EN" => '1',
        "ES" => '+',
        "ET" => '$',
        "AN" => '\u{0660}',
        "CS" => ',',
        "NSM" => '\u{0300}',
        "BN" => '\u{00AD}',
        "B" => '\u{2029}',
        "S" => '\t',
        "WS" => ' ',
        "LRE" => '\u{202A}',
        "RLE" => '\u{202B}',
        "PDF" => '\u{202C}',
        "LRO" => '\u{202D}',
        "RLO" => '\u{202E}',
        "LRI" => '\u{2066}',
        "RLI" => '\u{2067}',
        "FSI" => '\u{2068}',
        "PDI" => '\u{2069}',
        _ => '!',
    }
}

#[test]
fn bidi_test_lines() {
    // (classes, paragraph direction, levels, order) from BidiTest.txt.
    let cases: [(&str, Option<Direction>, &str, &str); 8] = [
        // X8: the separator is at the paragraph level, so an embedding
        // opened right before it does not change the end of the run.
        ("R ES RLE B", Some(Direction::Ltr), "1 0 x 0", "0 1 3"),
        ("AN ET RLO B", None, "2 0 x 0", "0 1 3"),
        // X6 leaves BN alone: an override does not keep it from X9.
        ("R ES RLO BN", Some(Direction::Ltr), "1 0 x x", "0 1"),
        ("LRO BN PDF NSM", Some(Direction::Rtl), "x x x 1", "3"),
        // X5a to X5c: an isolate inside an override opens at its own
        // direction, and still matches its PDI.
        ("RLO RLI ES B", None, "x 1 3 0", "2 1 3"),
        ("RLO RLI L B", Some(Direction::Ltr), "x 1 4 0", "2 1 3"),
        ("LRO RLI R B", None, "x 2 3 0", "1 2 3"),
        ("LRO FSI R B", None, "x 2 3 0", "1 2 3"),
    ];
    for (classes, direction, want_levels, want_order) in cases {
        let text: String = classes.split_whitespace().map(class_char).collect();
        let (got_levels, got_order) = resolve(&text, direction);
        assert_eq!(got_levels, levels(want_levels), "{classes}");
        assert_eq!(got_order, order(want_order), "{classes}");
    }
}

#[test]
fn bidi_character_test_lines() {
    let lines = [
        // FSI inside an override.
        "202D 05D0 202B 05D1 202C 2068 05D2 2069 202B 05D3 202C 05D4 202C;2;1;\
         x 2 x 3 x 2 3 2 x 3 x 2 x;1 3 5 6 7 9 11",
        // N0: marks that follow a resolved bracket take its type.
        "0061 0028 0062 0029 0331;1;1;2 2 2 2 2;0 1 2 3 4",
        "0061 0028 0332 0062 0029 0333;1;1;2 2 2 2 2 2;0 1 2 3 4 5",
        "05D0 0028 05D1 0029 0331;0;0;1 1 1 1 1;4 3 2 1 0",
        // BD16: canonically equivalent brackets pair.
        "0061 0020 2329 0062 002E 0031 3009;1;1;2 2 2 2 2 2 2;0 1 2 3 4 5 6",
        "05D0 0020 3008 05D1 002E 0031 232A;0;0;1 1 1 1 1 2 1;6 5 4 3 2 1 0",
    ];
    for line in lines {
        let fields: Vec<&str> = line.split(';').collect();
        let text: String = fields[0]
            .split_whitespace()
            .filter_map(|t| u32::from_str_radix(t, 16).ok().and_then(char::from_u32))
            .collect();
        let direction = match fields[1] {
            "0" => Some(Direction::Ltr),
            "1" => Some(Direction::Rtl),
            _ => None,
        };
        let (got_levels, got_order) = resolve(&text, direction);
        assert_eq!(got_levels, levels(fields[3]), "{line}");
        assert_eq!(got_order, order(fields[4]), "{line}");
    }
}

#[test]
fn a_full_bracket_stack_stops_pairing() {
    // BidiCharacterTest.txt: "a", 64 opening parentheses, "b", 64
    // closing ones, right to left. The 64th opener finds the stack
    // full (BD16), so no pair forms and N1 and N2 resolve the brackets:
    // the openers sit between two L letters, the closers between "b"
    // and the paragraph end.
    let mut text = String::from("a");
    text.extend(core::iter::repeat('(').take(64));
    text.push('b');
    text.extend(core::iter::repeat(')').take(64));
    let (got_levels, got_order) = resolve(&text, Some(Direction::Rtl));
    let mut want_levels = alloc::vec![Some(2); 66];
    want_levels.extend(core::iter::repeat(Some(1)).take(64));
    assert_eq!(got_levels, want_levels);
    let mut want_order: Vec<usize> = (66..130).rev().collect();
    want_order.extend(0..66);
    assert_eq!(got_order, want_order);
    // One fewer opener fits, and every pair forms.
    let mut text = String::from("a");
    text.extend(core::iter::repeat('(').take(63));
    text.push('b');
    text.extend(core::iter::repeat(')').take(63));
    let (got_levels, _) = resolve(&text, Some(Direction::Rtl));
    assert!(got_levels.iter().all(|&l| l == Some(2)));
}
