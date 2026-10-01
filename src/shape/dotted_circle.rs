//! Dotted circles for broken Myanmar syllables, HarfBuzz's
//! `hb_syllabic_insert_dotted_circles` (`hb-ot-shaper-syllabic.cc`).
//!
//! HarfBuzz's syllabic shapers find a "broken" syllable when a
//! dependent mark (a vowel sign, virama, medial, or other combining
//! sign) starts a syllable with no base to attach to. They then insert
//! U+25CC DOTTED CIRCLE at the start of that syllable, with the
//! syllable's cluster, so the marks shape around the circle as their
//! base. The insertion is skipped when the font has no glyph for
//! U+25CC or the buffer carries
//! `HB_BUFFER_FLAG_DO_NOT_INSERT_DOTTED_CIRCLE`
//! ([`crate::BufferFlags::DO_NOT_INSERT_DOTTED_CIRCLE`] here). The
//! dotted circle `BufferFlags::BOT` puts under a mark at the very start
//! of the text is inserted earlier, by the pipeline.
//!
//! The Indic, Khmer, and USE shapers insert their own circles, from
//! their own syllable machines. This module serves the Myanmar pass,
//! whose syllable scanner emits one broken syllable per orphan mark
//! where HarfBuzz's grammar takes a whole run of them as one broken
//! syllable, so consecutive broken syllables share one circle. The
//! circle goes in before the Myanmar pass runs, which then sees it as
//! a generic base.

use alloc::vec::Vec;

use crate::buffer::Glyph;
use crate::ot::myanmar::{category, Category};
use crate::ot::myanmar::{segment_syllables, SyllableKind};

/// U+25CC DOTTED CIRCLE.
const DOTTED_CIRCLE: char = '\u{25CC}';

/// True when a syllable starting with `ch` has no base.
fn orphan(ch: char) -> bool {
    matches!(
        category(ch),
        Category::H
            | Category::VPre
            | Category::VAbv
            | Category::VBlw
            | Category::VPst
            | Category::M
            | Category::FM
            | Category::CM
    )
}

/// Where the circles go: the code point index each broken run starts
/// at.
fn insertion_points(cps: &[char]) -> Vec<usize> {
    let mut points = Vec::new();
    let mut previous_end: Option<usize> = None;
    for s in segment_syllables(cps) {
        let start = s.start;
        let broken = s.kind == SyllableKind::Broken && cps.get(start).is_some_and(|&c| orphan(c));
        if broken {
            // A broken syllable right after another is part of the
            // same run of marks: one circle covers both.
            if previous_end != Some(start) {
                points.push(start);
            }
            previous_end = Some(s.end);
        } else {
            previous_end = None;
        }
    }
    points
}

/// Inserts a dotted circle (glyph `circle`) before each broken run of
/// the Myanmar segment `cps` / `glyphs`. Returns the segment's new code
/// points when it inserted any. `glyphs` then has the circles too.
/// Glyphs must still be one per code point.
pub(super) fn insert(cps: &[char], glyphs: &mut Vec<Glyph>, circle: u16) -> Option<Vec<char>> {
    if glyphs.len() != cps.len() {
        return None;
    }
    let points = insertion_points(cps);
    if points.is_empty() {
        return None;
    }
    let mut new_cps = Vec::with_capacity(cps.len() + points.len());
    let mut new_glyphs = Vec::with_capacity(glyphs.len() + points.len());
    let mut next = points.iter().peekable();
    for (i, (&ch, &glyph)) in cps.iter().zip(glyphs.iter()).enumerate() {
        if next.peek() == Some(&&i) {
            next.next();
            // The circle takes the cluster of the syllable it opens.
            new_cps.push(DOTTED_CIRCLE);
            new_glyphs.push(Glyph::new(u32::from(circle), glyph.cluster));
        }
        new_cps.push(ch);
        new_glyphs.push(glyph);
    }
    *glyphs = new_glyphs;
    Some(new_cps)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(text: &str) -> Option<(Vec<char>, Vec<u32>)> {
        let cps: Vec<char> = text.chars().collect();
        let mut glyphs: Vec<Glyph> = text
            .char_indices()
            .map(|(i, c)| Glyph::new(c as u32, i as u32))
            .collect();
        insert(&cps, &mut glyphs, 7).map(|new| (new, glyphs.iter().map(|g| g.cluster).collect()))
    }

    #[test]
    fn lone_vowel_sign_gets_a_circle_with_its_cluster() {
        let (cps, clusters) = run("\u{1031}").expect("inserted");
        assert_eq!(cps, ['\u{25CC}', '\u{1031}']);
        assert_eq!(clusters, [0, 0]);
    }

    #[test]
    fn one_circle_per_run_of_orphan_marks() {
        let (cps, _) = run("\u{1000} \u{1031}\u{102C}").expect("inserted");
        assert_eq!(cps, ['\u{1000}', ' ', '\u{25CC}', '\u{1031}', '\u{102C}']);
    }

    #[test]
    fn complete_syllables_are_left_alone() {
        assert_eq!(run("\u{1000}\u{1031}\u{102C}"), None);
        assert_eq!(run("\u{200D}\u{1000}"), None);
    }
}
