//! Integration coverage for the UAX #9 bidi pipeline + the
//! `Buffer::set_text_bidi` opt-in entry point.
//!
//! Mixed Latin / Hebrew / Arabic text exercises the full chain
//! (X1-X10 + W1-W7 + N1-N2 + I1-I2 + L2 reorder). Expected visual
//! orderings cross-check against the canonical UAX #9 reference
//! algorithm — we can't link rustybuzz's bidi from these crates
//! without a new dep, so the assertions are derived by hand from
//! the spec and confirmed against the algorithm's library tests.

use sigilbuzz::unicode::bidi::BidiInfo;
use sigilbuzz::{Buffer, Direction};

#[test]
fn pure_ascii_reorder_is_identity() {
    let info = BidiInfo::new("Hello, world!", None);
    assert_eq!(info.paragraph_direction(), Direction::Ltr);
    let order = info.reorder();
    let chars: alloc_helper::Vec<char> = "Hello, world!".chars().collect();
    let visual: alloc_helper::String = order.iter().map(|&i| chars[i]).collect();
    assert_eq!(visual, "Hello, world!");
}

#[test]
fn latin_with_hebrew_reorders_only_hebrew_span() {
    // "Hello עברית world" — Hebrew "עברית" (5 chars) is between two
    // Latin spans. L2 reverses the level-1 span only.
    let text = "Hello \u{05E2}\u{05D1}\u{05E8}\u{05D9}\u{05EA} world";
    let info = BidiInfo::new(text, None);
    assert_eq!(info.paragraph_direction(), Direction::Ltr);
    let chars: alloc_helper::Vec<char> = text.chars().collect();
    let visual: alloc_helper::String = info.reorder().iter().map(|&i| chars[i]).collect();
    // Visual: "Hello " + תירבע + " world".
    assert_eq!(
        visual,
        "Hello \u{05EA}\u{05D9}\u{05E8}\u{05D1}\u{05E2} world"
    );
}

#[test]
fn buffer_set_text_bidi_matches_reorder_output() {
    // Sanity: Buffer::set_text_bidi is just BidiInfo::new + reorder
    // applied in-place. They must agree.
    let text = "abc \u{05D0}\u{05D1} xyz";
    let info = BidiInfo::new(text, None);
    let chars: alloc_helper::Vec<char> = text.chars().collect();
    let expected: alloc_helper::String = info.reorder().iter().map(|&i| chars[i]).collect();

    let mut buf = Buffer::new();
    buf.set_text_bidi(text);
    assert_eq!(buf.text(), expected);
    assert_eq!(buf.direction(), Direction::Ltr);
}

#[test]
fn buffer_set_text_unchanged_for_backward_compat() {
    // Critical: the plain set_text path must NOT bidi-reorder. 0.1.0
    // consumers (oniq, demos) own direction handling themselves.
    let text = "Hello \u{05E2}\u{05D1}\u{05E8}\u{05D9}\u{05EA}";
    let mut buf = Buffer::new();
    buf.set_text(text);
    assert_eq!(buf.text(), text);
    // Direction defaults to LTR — set_text doesn't touch it.
    assert_eq!(buf.direction(), Direction::Ltr);
}

#[test]
fn pure_rtl_paragraph_reverses_completely() {
    // Pure Hebrew run — "שלום" — gets reversed end-to-end.
    let text = "\u{05E9}\u{05DC}\u{05D5}\u{05DD}";
    let info = BidiInfo::new(text, None);
    assert_eq!(info.paragraph_direction(), Direction::Rtl);
    let chars: alloc_helper::Vec<char> = text.chars().collect();
    let visual: alloc_helper::String = info.reorder().iter().map(|&i| chars[i]).collect();
    assert_eq!(visual, "\u{05DD}\u{05D5}\u{05DC}\u{05E9}");
}

#[test]
fn rtl_paragraph_with_embedded_latin_keeps_latin_logical_order() {
    // RTL paragraph: Hebrew + space + "abc" + space + Hebrew. The
    // Latin "abc" sits inside a level-2 span; L2 reverses level >= 1
    // around it, but the level-2 span itself is reversed back so the
    // Latin reads left-to-right within the visual run.
    let text = "\u{05D0} abc \u{05D1}";
    let info = BidiInfo::new(text, None);
    assert_eq!(info.paragraph_direction(), Direction::Rtl);
    let chars: alloc_helper::Vec<char> = text.chars().collect();
    let visual: alloc_helper::String = info.reorder().iter().map(|&i| chars[i]).collect();
    // Visual: ב + space + abc + space + א — the two Hebrew letters
    // swap, but "abc" stays in logical order.
    assert_eq!(visual, "\u{05D1} abc \u{05D0}");
}

// Tiny shim so the test file works under both std and alloc-only
// builds. We can't take a dep on alloc directly from an integration
// test, so route via the std re-export.
mod alloc_helper {
    pub use std::string::String;
    pub use std::vec::Vec;
}
