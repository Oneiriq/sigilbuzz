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
//! A grapheme is a character and the continuation characters after it
//! (see `cluster::continuations`). At
//! [`ClusterLevel::MonotoneCharacters`] each reversed grapheme's
//! clusters merge, as HarfBuzz's `_hb_ot_layout_reverse_graphemes`
//! does; the grapheme levels merged them already, and
//! [`ClusterLevel::Characters`] keeps them apart.

use alloc::vec::Vec;

use super::cluster;
use crate::buffer::{ClusterLevel, Direction, Glyph};
use crate::unicode::bidi_class::{bidi_class, BidiClass};
use crate::unicode::general_category::{general_category_class, GeneralCategoryClass};
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

/// The per-code-point state that moves with the glyphs when the
/// graphemes reverse: the code points themselves, the glyphs, and
/// which code points were mirrored.
pub(super) struct Run<'a> {
    pub(super) cps: &'a mut [char],
    pub(super) glyphs: &'a mut [Glyph],
    pub(super) mirrored: &'a mut [bool],
}

/// Reverses the order of the graphemes of `run`, keeping each
/// grapheme's own order. `cont` holds the continuation bits of the
/// code points before the reversal. Follows HarfBuzz's
/// `reverse_groups`: each grapheme is merged (at
/// [`ClusterLevel::MonotoneCharacters`] only) and reversed in place,
/// then the whole run is reversed.
pub(super) fn reverse_graphemes(run: Run<'_>, cont: &[bool], level: ClusterLevel) {
    let len = run.cps.len();
    if run.glyphs.len() != len || run.mirrored.len() != len || cont.len() != len {
        return;
    }
    let ranges: Vec<_> = cluster::graphemes(cont).collect();
    for range in ranges {
        if level == ClusterLevel::MonotoneCharacters {
            cluster::merge_clusters(run.glyphs, range.start, range.end, level);
        }
        run.cps[range.clone()].reverse();
        run.glyphs[range.clone()].reverse();
        run.mirrored[range].reverse();
    }
    run.cps.reverse();
    run.glyphs.reverse();
    run.mirrored.reverse();
}

#[cfg(test)]
mod tests {
    use alloc::string::String;

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

    /// Reverses `text` (glyph ids and clusters are the code point
    /// indices) and returns the code points, ids, clusters, and
    /// mirrored flags after.
    fn reversed(text: &str, level: ClusterLevel) -> (String, Vec<u32>, Vec<u32>, Vec<bool>) {
        let mut cps: Vec<char> = text.chars().collect();
        let cont = cluster::continuations(&cps);
        let mut glyphs: Vec<Glyph> = (0..cps.len() as u32).map(|i| Glyph::new(i, i)).collect();
        let mut mirrored = alloc::vec![false; cps.len()];
        mirrored[0] = true;
        let run = Run {
            cps: &mut cps,
            glyphs: &mut glyphs,
            mirrored: &mut mirrored,
        };
        reverse_graphemes(run, &cont, level);
        (
            cps.into_iter().collect(),
            glyphs.iter().map(|g| g.glyph_id).collect(),
            glyphs.iter().map(|g| g.cluster).collect(),
            mirrored,
        )
    }

    #[test]
    fn graphemes_reverse_as_units() {
        let text = "ab\u{0301}\u{0302}c\u{200D}\u{1F600}";
        let (cps, ids, clusters, mirrored) = reversed(text, ClusterLevel::Characters);
        assert_eq!(cps, "c\u{200D}\u{1F600}b\u{0301}\u{0302}a");
        assert_eq!(ids, [4, 5, 6, 1, 2, 3, 0]);
        assert_eq!(clusters, [4, 5, 6, 1, 2, 3, 0]);
        assert_eq!(mirrored.last(), Some(&true));
    }

    #[test]
    fn monotone_characters_merges_each_reversed_grapheme() {
        let text = "ab\u{0301}\u{0302}c\u{200D}\u{1F600}";
        let (_, ids, clusters, _) = reversed(text, ClusterLevel::MonotoneCharacters);
        assert_eq!(ids, [4, 5, 6, 1, 2, 3, 0]);
        assert_eq!(clusters, [4, 4, 4, 1, 1, 1, 0]);
        // The grapheme levels merge before the reversal, not here.
        for level in [ClusterLevel::MonotoneGraphemes, ClusterLevel::Graphemes] {
            let (_, _, clusters, _) = reversed(text, level);
            assert_eq!(clusters, [4, 5, 6, 1, 2, 3, 0], "{level:?}");
        }
    }
}
