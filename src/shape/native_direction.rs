//! Shaping in the script's native direction, HarfBuzz's
//! `hb_ensure_native_direction` (`hb-ot-shape.cc`).
//!
//! HarfBuzz reads a buffer whose direction differs from its script's
//! native horizontal direction as text already in that visual order:
//! an LTR buffer of Arabic holds the letters left to right as drawn.
//! It reverses the order of the buffer's graphemes, shapes in the
//! native direction (so joining and ligatures see logical order), and
//! the usual end-of-positioning reversal of backward runs brings the
//! glyphs back to the order the caller asked for. Vertical buffers
//! other than top-to-bottom are handled the same way, as top-to-bottom.
//! A run of digits (or regional indicators) with no letter in an RTL
//! script counts as LTR.
//!
//! The buffer's script is its first script-bearing character's, as
//! HarfBuzz guesses it. Scripts sigilbuzz has no bucket for take their
//! direction from their first strong bidi class instead, so an RTL
//! script without a sigilbuzz shaper (Syriac, Thaana, ...) still counts
//! as RTL. HarfBuzz reports no native direction for the bidirectional
//! scripts (Old Hungarian, Old Italic, Runic, Tifinagh); those show up
//! here as left to right.
//!
//! A grapheme is a character and the continuation characters after it,
//! following `hb_set_unicode_props`: marks, ZWJ and an
//! Extended_Pictographic character right after one, emoji modifiers,
//! the second of a regional indicator pair, the halfwidth katakana
//! voiced sound marks, and tag characters.

use alloc::vec::Vec;

use crate::buffer::{Direction, Glyph};
use crate::unicode::bidi_class::{bidi_class, BidiClass};
use crate::unicode::general_category::{
    general_category_class, is_extended_pictographic, GeneralCategoryClass,
};
use crate::unicode::Script;

const fn is_regional_indicator(ch: char) -> bool {
    matches!(ch as u32, 0x1F1E6..=0x1F1FF)
}

/// The script's native horizontal direction: [`Script::horizontal_direction`],
/// or for text sigilbuzz has no script bucket for, the direction of
/// its first strong character (left to right when there is none, as
/// for HarfBuzz's Common script).
fn native_horizontal(script: Option<Script>, cps: &[char]) -> Direction {
    match script {
        Some(script) if script != Script::Other => script.horizontal_direction(),
        _ => cps
            .iter()
            .find_map(|&c| match bidi_class(c) {
                BidiClass::L => Some(Direction::Ltr),
                BidiClass::R | BidiClass::Al => Some(Direction::Rtl),
                _ => None,
            })
            .unwrap_or(Direction::Ltr),
    }
}

/// The direction to shape `cps` in when the caller asked for
/// `direction`: its reverse when that is not the script's native one.
pub(super) fn resolve(direction: Direction, script: Option<Script>, cps: &[char]) -> Direction {
    if !direction.is_horizontal() {
        return Direction::Ttb;
    }
    let mut native = native_horizontal(script, cps);
    if native == Direction::Rtl && direction == Direction::Ltr {
        let (mut number, mut ri) = (false, false);
        for &c in cps {
            match general_category_class(c) {
                Some(GeneralCategoryClass::Letter) => {
                    number = false;
                    ri = false;
                    break;
                }
                Some(GeneralCategoryClass::DecimalNumber) => number = true,
                _ if is_regional_indicator(c) => ri = true,
                _ => {}
            }
        }
        if number || ri {
            native = Direction::Ltr;
        }
    }
    native
}

/// True at each index of `cps` that continues the grapheme before it.
fn continuations(cps: &[char]) -> Vec<bool> {
    let mut cont = alloc::vec![false; cps.len()];
    let mut i = 0;
    while i < cps.len() {
        let c = cps[i];
        let cp = c as u32;
        if cp >= 0x80 && general_category_class(c) == Some(GeneralCategoryClass::Mark) {
            cont[i] = true;
        } else if (0x1F3FB..=0x1F3FF).contains(&cp) {
            // Emoji modifiers.
            cont[i] = true;
        } else if is_regional_indicator(c) {
            if i > 0 && is_regional_indicator(cps[i - 1]) && !cont[i - 1] {
                cont[i] = true;
            }
        } else if c == '\u{200D}' {
            cont[i] = true;
            if cps.get(i + 1).is_some_and(|&n| is_extended_pictographic(n)) {
                i += 1;
                cont[i] = true;
            }
        } else if matches!(cp, 0xFF9E..=0xFF9F | 0xE0020..=0xE007F) {
            cont[i] = true;
        }
        i += 1;
    }
    cont
}

/// Reverses the order of the graphemes of `cps`, keeping each
/// grapheme's own order, and moves `glyphs` and `mirrored` (one entry
/// per code point) along.
pub(super) fn reverse_graphemes(cps: &mut [char], glyphs: &mut [Glyph], mirrored: &mut [bool]) {
    if glyphs.len() != cps.len() || mirrored.len() != cps.len() {
        return;
    }
    let cont = continuations(cps);
    // Reverse everything, then put each grapheme back in order.
    cps.reverse();
    glyphs.reverse();
    mirrored.reverse();
    let len = cps.len();
    let mut end = 0;
    while end < len {
        // In reversed order a grapheme is its continuations followed
        // by the character that starts it.
        let start = end;
        while end < len && cont[len - 1 - end] {
            end += 1;
        }
        end = (end + 1).min(len);
        cps[start..end].reverse();
        glyphs[start..end].reverse();
        mirrored[start..end].reverse();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_native_horizontal_directions_flip() {
        let arabic = Some(Script::Arabic);
        let cps: Vec<char> = "\u{0628}\u{062A}".chars().collect();
        assert_eq!(resolve(Direction::Ltr, arabic, &cps), Direction::Rtl);
        assert_eq!(resolve(Direction::Rtl, arabic, &cps), Direction::Rtl);
        let latin: Vec<char> = "fi".chars().collect();
        assert_eq!(
            resolve(Direction::Rtl, Some(Script::Latin), &latin),
            Direction::Ltr
        );
        assert_eq!(
            resolve(Direction::Ltr, Some(Script::Latin), &latin),
            Direction::Ltr
        );
        // Vertical runs shape top to bottom.
        assert_eq!(
            resolve(Direction::Btt, Some(Script::Mongolian), &latin),
            Direction::Ttb
        );
    }

    #[test]
    fn digits_in_an_rtl_script_stay_ltr() {
        let arabic = Some(Script::Arabic);
        let digits: Vec<char> = "\u{0661}\u{0662}".chars().collect();
        assert_eq!(resolve(Direction::Ltr, arabic, &digits), Direction::Ltr);
        let mixed: Vec<char> = "\u{0661} \u{0628}".chars().collect();
        assert_eq!(resolve(Direction::Ltr, arabic, &mixed), Direction::Rtl);
    }

    #[test]
    fn unbucketed_scripts_use_their_strong_direction() {
        let syriac: Vec<char> = "\u{0710}\u{0712}".chars().collect();
        assert_eq!(
            resolve(Direction::Rtl, Some(Script::Other), &syriac),
            Direction::Rtl
        );
        assert_eq!(
            resolve(Direction::Ltr, Some(Script::Other), &syriac),
            Direction::Rtl
        );
        let digits: Vec<char> = "12".chars().collect();
        assert_eq!(resolve(Direction::Rtl, None, &digits), Direction::Ltr);
    }

    #[test]
    fn graphemes_reverse_as_units() {
        let mut cps: Vec<char> = "ab\u{0301}\u{0302}c\u{200D}\u{1F600}".chars().collect();
        let mut glyphs: Vec<Glyph> = (0..cps.len() as u32).map(|i| Glyph::new(i, i)).collect();
        let mut mirrored = alloc::vec![false; cps.len()];
        mirrored[0] = true;
        reverse_graphemes(&mut cps, &mut glyphs, &mut mirrored);
        let expected: Vec<char> = "c\u{200D}\u{1F600}b\u{0301}\u{0302}a".chars().collect();
        assert_eq!(cps, expected);
        let ids: Vec<u32> = glyphs.iter().map(|g| g.glyph_id).collect();
        assert_eq!(ids, [4, 5, 6, 1, 2, 3, 0]);
        assert_eq!(mirrored.last(), Some(&true));
    }

    #[test]
    fn regional_indicators_pair_up() {
        let flags = "\u{1F1EB}\u{1F1F7}\u{1F1E9}\u{1F1EA}\u{1F1EF}";
        let cont = continuations(&flags.chars().collect::<Vec<_>>());
        assert_eq!(cont, [false, true, false, true, false]);
    }
}
