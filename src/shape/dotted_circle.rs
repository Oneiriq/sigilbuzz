//! Dotted circles for broken syllables, HarfBuzz's
//! `hb_syllabic_insert_dotted_circles` (`hb-ot-shaper-syllabic.cc`).
//!
//! The Indic, Khmer, Myanmar, and USE shapers find a "broken"
//! syllable when a dependent mark (a matra, virama, nukta, bindu, or
//! other combining sign) starts a syllable with no base to attach to,
//! as in a lone U+093F DEVANAGARI VOWEL SIGN I. HarfBuzz then inserts
//! U+25CC DOTTED CIRCLE at the start of that syllable (after a repha
//! that opens it), with the syllable's cluster, so the marks shape
//! around the circle as their base. It skips the insertion when the
//! font has no glyph for U+25CC or the buffer carries
//! `HB_BUFFER_FLAG_DO_NOT_INSERT_DOTTED_CIRCLE`
//! ([`crate::BufferFlags::DO_NOT_INSERT_DOTTED_CIRCLE`] here). The
//! dotted circle `BufferFlags::BOT` puts under a mark at the very start
//! of the text is inserted earlier, by the pipeline.
//!
//! sigilbuzz's syllable scanners emit one broken syllable per orphan
//! mark, where HarfBuzz's grammar takes a whole run of them as one
//! broken syllable, so consecutive broken syllables share one circle.
//! The circle goes in before the shaper runs, and the shaper then sees
//! it as the base (U+25CC is a consonant placeholder to the Indic
//! scanner and a generic base to USE).

use alloc::vec::Vec;

use crate::buffer::Glyph;
use crate::ot::indic::devanagari::{segment_syllables as indic_syllables, SyllableKind as Indic};
use crate::ot::indic::indic_config_for;
use crate::ot::use_shaper::{segment_syllables as use_syllables, SyllableKind as Use};
use crate::unicode::indic_category::{syllabic_category, IndicSyllabicCategory as Isc};
use crate::unicode::use_category::{use_category, UseCategory};
use crate::unicode::Script;

/// U+25CC DOTTED CIRCLE.
const DOTTED_CIRCLE: char = '\u{25CC}';

/// True when an Indic syllable starting with `ch` has no base: a
/// dependent sign at the start of a syllable.
fn indic_orphan(ch: char) -> bool {
    matches!(
        syllabic_category(ch),
        Isc::VowelDependent
            | Isc::Virama
            | Isc::Nukta
            | Isc::Bindu
            | Isc::Visarga
            | Isc::CantillationMark
            | Isc::ConsonantMedial
    )
}

/// True when a USE syllable starting with `ch` has no base.
fn use_orphan(ch: char) -> bool {
    matches!(
        use_category(ch),
        UseCategory::H
            | UseCategory::VPre
            | UseCategory::VAbv
            | UseCategory::VBlw
            | UseCategory::VPst
            | UseCategory::M
            | UseCategory::FM
            | UseCategory::CM
    )
}

/// Where the circles go: the code point index each broken run starts
/// at, past a leading repha.
fn insertion_points(script: Script, cps: &[char]) -> Vec<usize> {
    let mut points = Vec::new();
    // Syllable (start, end, broken) triples, in order.
    let syllables: Vec<(usize, usize, bool)> = if let Some(config) = indic_config_for(script) {
        indic_syllables(cps, &config)
            .iter()
            .map(|s| {
                let broken = matches!(s.kind, Indic::Broken | Indic::Standalone)
                    && indic_orphan(cps[s.start]);
                (s.start, s.end, broken)
            })
            .collect()
    } else if matches!(
        script,
        Script::Khmer
            | Script::Myanmar
            | Script::Buginese
            | Script::TaiTham
            | Script::Balinese
            | Script::Sundanese
            | Script::Lepcha
            | Script::Limbu
            | Script::Cham
            | Script::Brahmi
            | Script::Sharada
            | Script::Khojki
            | Script::Tirhuta
            | Script::Modi
    ) {
        use_syllables(cps)
            .iter()
            .map(|s| {
                let repha = use_category(cps[s.start]) == UseCategory::R;
                let first = if repha { s.start + 1 } else { s.start };
                let broken =
                    s.kind == Use::Broken && cps.get(first).is_some_and(|&c| use_orphan(c));
                (first, s.end, broken)
            })
            .collect()
    } else {
        return points;
    };
    let mut previous_end: Option<usize> = None;
    for (start, end, broken) in syllables {
        if broken {
            // A broken syllable right after another is part of the
            // same run of marks: one circle covers both.
            if previous_end != Some(start) {
                points.push(start);
            }
            previous_end = Some(end);
        } else {
            previous_end = None;
        }
    }
    points
}

/// Inserts a dotted circle (glyph `circle`) before each broken run of
/// the segment `cps` / `glyphs` of `script`. Returns the segment's new
/// code points when it inserted any; `glyphs` then has the circles
/// too. Glyphs must still be one per code point.
pub(super) fn insert(
    script: Script,
    cps: &[char],
    glyphs: &mut Vec<Glyph>,
    circle: u16,
) -> Option<Vec<char>> {
    if glyphs.len() != cps.len() {
        return None;
    }
    let points = insertion_points(script, cps);
    if points.is_empty() {
        return None;
    }
    let mut new_cps = Vec::with_capacity(cps.len() + points.len());
    let mut new_glyphs = Vec::with_capacity(glyphs.len() + points.len());
    let mut next = points.iter().peekable();
    for (i, (&ch, &glyph)) in cps.iter().zip(glyphs.iter()).enumerate() {
        if next.peek() == Some(&&i) {
            next.next();
            // The circle takes the cluster of the syllable it opens
            // (the repha's, when one comes first).
            let cluster = if i > 0 && use_category(cps[i - 1]) == UseCategory::R {
                glyphs[i - 1].cluster
            } else {
                glyph.cluster
            };
            new_cps.push(DOTTED_CIRCLE);
            new_glyphs.push(Glyph::new(u32::from(circle), cluster));
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

    fn run(script: Script, text: &str) -> Option<(Vec<char>, Vec<u32>)> {
        let cps: Vec<char> = text.chars().collect();
        let mut glyphs: Vec<Glyph> = text
            .char_indices()
            .map(|(i, c)| Glyph::new(c as u32, i as u32))
            .collect();
        insert(script, &cps, &mut glyphs, 7)
            .map(|new| (new, glyphs.iter().map(|g| g.cluster).collect()))
    }

    #[test]
    fn lone_matra_gets_a_circle_with_its_cluster() {
        let (cps, clusters) = run(Script::Devanagari, "\u{093F}").expect("inserted");
        assert_eq!(cps, ['\u{25CC}', '\u{093F}']);
        assert_eq!(clusters, [0, 0]);
    }

    #[test]
    fn one_circle_per_run_of_orphan_marks() {
        let (cps, _) =
            run(Script::Devanagari, "\u{0915} \u{093F}\u{0902}\u{094D}").expect("inserted");
        assert_eq!(
            cps,
            ['\u{0915}', ' ', '\u{25CC}', '\u{093F}', '\u{0902}', '\u{094D}']
        );
    }

    #[test]
    fn complete_syllables_and_other_scripts_are_left_alone() {
        assert_eq!(run(Script::Devanagari, "\u{0915}\u{093F}\u{0902}"), None);
        assert_eq!(run(Script::Devanagari, "\u{200D}\u{0915}"), None);
        assert_eq!(run(Script::Thai, "\u{0E31}"), None);
        assert_eq!(run(Script::Latin, "\u{0301}"), None);
    }

    #[test]
    fn khmer_orphan_vowel_gets_a_circle() {
        let (cps, clusters) = run(Script::Khmer, "\u{1780} \u{17C1}\u{1780}").expect("inserted");
        assert_eq!(cps, ['\u{1780}', ' ', '\u{25CC}', '\u{17C1}', '\u{1780}']);
        assert_eq!(clusters, [0, 3, 4, 4, 7]);
    }
}
