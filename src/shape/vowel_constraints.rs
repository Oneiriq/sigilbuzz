//! HarfBuzz's vowel constraints (`_hb_preprocess_text_vowel_constraints`
//! in `hb-ot-shaper-vowel-constraints.cc`, HarfBuzz 14.5.0).
//!
//! Some sequences of a vowel and a vowel sign look like another vowel:
//! Devanagari A followed by the vowel sign AA reads as the letter AA.
//! Microsoft's Universal Shaping Engine spec lists them
//! (`IndicShapingInvalidCluster.txt`), and HarfBuzz puts a dotted
//! circle before the last character of each one it finds, so the
//! sequence shows as broken. The Indic and Universal Shaping Engine
//! shapers do this in their `preprocess_text`, which runs on the whole
//! buffer after clusters form and before normalization. HarfBuzz's
//! Khmer and Myanmar shapers have no `preprocess_text`, and the file
//! lists no sequence of those scripts.
//!
//! The sequences are those of the buffer's script. The scan reads the
//! buffer from the start: after a sequence matches, the next one can
//! only start after it. Nothing is inserted when the buffer has
//! [`BufferFlags::DO_NOT_INSERT_DOTTED_CIRCLE`].
//!
//! HarfBuzz builds the circle from the glyph info of the character
//! after it (`output_glyph`), so it takes that character's cluster,
//! glyph flags, and Unicode properties. A circle before a mark is a
//! mark to normalization and to the glyph class HarfBuzz synthesizes
//! for fonts without GDEF classes.

mod table;

use alloc::vec::Vec;

use super::normalize::mark_props;
use crate::buffer::{BufferFlags, Glyph};
use crate::unicode::Script;

/// U+25CC DOTTED CIRCLE.
const DOTTED_CIRCLE: char = '\u{25CC}';

/// The sequences HarfBuzz checks for a buffer of `script`.
fn sequences(script: Script) -> &'static [&'static [char]] {
    script
        .iso15924_tag()
        .and_then(|tag| table::CONSTRAINTS.iter().find(|(t, _)| *t == tag))
        .map_or(&[], |(_, seqs)| seqs)
}

/// The length of the sequence of `seqs` that `text` starts with.
fn match_len(seqs: &[&[char]], text: &[char]) -> Option<usize> {
    let first = text.first()?;
    let start = seqs.partition_point(|s| s.first() < Some(first));
    seqs.get(start..)?
        .iter()
        .take_while(|s| s.first() == Some(first))
        .find(|s| text.starts_with(s))
        .map(|s| s.len())
}

/// Inserts the dotted circles of the vowel constraints of `script`, the
/// buffer's script, into a run's characters. `cps`, `glyphs`, and
/// `mirrored` are one to one, and stay so.
pub(super) fn insert_dotted_circles(
    script: Option<Script>,
    flags: BufferFlags,
    cps: &mut Vec<char>,
    glyphs: &mut Vec<Glyph>,
    mirrored: &mut Vec<bool>,
) {
    if flags.contains(BufferFlags::DO_NOT_INSERT_DOTTED_CIRCLE) {
        return;
    }
    let seqs = script.map_or(&[][..], sequences);
    if seqs.is_empty() || cps.len() != glyphs.len() || cps.len() != mirrored.len() {
        return;
    }
    // The indices of the characters that get a circle before them.
    let mut before: Vec<usize> = Vec::new();
    let mut i = 0;
    while i + 1 < cps.len() {
        match cps.get(i..).and_then(|rest| match_len(seqs, rest)) {
            Some(len) => {
                before.push(i + len - 1);
                i += len;
            }
            None => i += 1,
        }
    }
    if before.is_empty() {
        return;
    }
    let len = cps.len() + before.len();
    let mut out_cps = Vec::with_capacity(len);
    let mut out_glyphs = Vec::with_capacity(len);
    let mut out_mirrored = Vec::with_capacity(len);
    let mut next = before.into_iter().peekable();
    for (k, ((&ch, &glyph), &m)) in cps
        .iter()
        .zip(glyphs.iter())
        .zip(mirrored.iter())
        .enumerate()
    {
        if next.next_if_eq(&k).is_some() {
            let mut circle = glyph;
            (circle.char_class, circle.combining_class) = mark_props(ch);
            out_cps.push(DOTTED_CIRCLE);
            out_glyphs.push(circle);
            out_mirrored.push(false);
        }
        out_cps.push(ch);
        out_glyphs.push(glyph);
        out_mirrored.push(m);
    }
    *cps = out_cps;
    *glyphs = out_glyphs;
    *mirrored = out_mirrored;
}

#[cfg(test)]
mod tests;
