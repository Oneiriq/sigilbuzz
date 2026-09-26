//! Script segmentation: splits the post-cmap code points into
//! same-script runs and picks the script tags each run's lookups try.

use alloc::vec::Vec;

use crate::buffer::script_priority_for;
use crate::unicode::{script_of, Script};

/// One shape-time segment: a maximal run of codepoints that share a
/// resolved script. `cp_range` is a half-open range into the
/// `codepoints` vector (not into the buffer text, because
/// preprocessing and normalization change the characters; the
/// normalizer rewrites the ranges after it runs). Pre-GSUB,
/// `glyphs[cp_range]` covers exactly the same glyphs.
#[derive(Debug)]
pub(super) struct Segment {
    pub(super) cp_range: core::ops::Range<usize>,
    pub(super) script: Script,
    pub(super) script_priority: &'static [[u8; 4]],
}

/// Post-GSUB slice of the fully-assembled `glyphs` vector, one per
/// pre-shaped segment. The GPOS loop reads this back to dispatch
/// kern / mark / mkmk / dist / user-enabled features against the
/// correct script tag priority for each slice.
#[derive(Debug)]
pub(super) struct ProcessedSegment {
    pub(super) range: core::ops::Range<usize>,
    pub(super) script_priority: &'static [[u8; 4]],
}

/// The script tags a segment of `script` tries. The Han bucket also
/// holds Hiragana and Katakana, which HarfBuzz tags `kana`, not `hani`
/// (`hb_ot_tags_from_script`); a segment whose first script-bearing
/// character is kana takes `kana`, as HarfBuzz's buffer would.
fn segment_priority(script: Script, cps: &[char]) -> &'static [[u8; 4]] {
    const KANA_PRIORITY: &[[u8; 4]] = &[*b"kana", *b"DFLT"];
    let kana = script == Script::Han
        && cps
            .iter()
            .find(|&&c| !is_common_for_segmentation(c))
            .is_some_and(|&c| matches!(c as u32, 0x3040..=0x30FF));
    if kana {
        KANA_PRIORITY
    } else {
        script_priority_for(script)
    }
}

/// Splits the post-cmap codepoint stream into [`Segment`]s whose
/// scripts agree with the buffer-level [`crate::buffer::Buffer::script_runs`]
/// segmentation: COMMON codepoints (ASCII space/digits/punctuation,
/// ZWJ/ZWNJ/bidi marks) extend whichever real-script segment ran
/// before them. A leading COMMON-only run joins the first real script
/// after it, the way HarfBuzz gives a buffer the script of its first
/// non-COMMON character, and text with no real script at all shapes
/// as `Script::Other` under DFLT. Always returns at least one segment
/// covering the whole `codepoints` range for a non-empty input.
pub(super) fn build_segments(codepoints: &[char]) -> Vec<Segment> {
    let mut segments: Vec<Segment> = Vec::new();
    if codepoints.is_empty() {
        return segments;
    }
    let leading = codepoints
        .iter()
        .copied()
        .find(|&c| !is_common_for_segmentation(c))
        .map_or(Script::Other, script_of);
    let mut current_start = 0usize;
    let mut current_script: Option<Script> = None;
    for (i, &ch) in codepoints.iter().enumerate() {
        let raw = script_of(ch);
        let resolved = if is_common_for_segmentation(ch) {
            current_script.unwrap_or(leading)
        } else {
            raw
        };
        match current_script {
            Some(s) if s == resolved => {}
            Some(s) => {
                segments.push(Segment {
                    cp_range: current_start..i,
                    script: s,
                    script_priority: segment_priority(s, &codepoints[current_start..i]),
                });
                current_start = i;
                current_script = Some(resolved);
            }
            None => {
                current_script = Some(resolved);
            }
        }
    }
    if let Some(s) = current_script {
        segments.push(Segment {
            cp_range: current_start..codepoints.len(),
            script: s,
            script_priority: segment_priority(s, &codepoints[current_start..]),
        });
    }
    segments
}

/// Shape-time COMMON / INHERITED predicate: the buffer-level
/// `is_common_or_inherited` in `buffer.rs`, plus the default
/// ignorables of those scripts. Kept inside `shape.rs` so the
/// Khmer-split synthetic codepoints (which never land in the buffer's
/// text) still segment correctly.
///
/// A default ignorable (ZWSP, word joiner, variation selectors, tag
/// characters, ...) has to stay in its neighbors' segment: GSUB and
/// GPOS match across it (see the `skip_iter` module), which they
/// cannot do when it splits the run. The Khmer and Mongolian ones
/// have their own script and segment with it anyway.
pub(super) const fn is_common_for_segmentation(ch: char) -> bool {
    let cp = ch as u32;
    matches!(
        cp,
        0x0000..=0x002F
        | 0x0030..=0x0040
        | 0x005B..=0x0060
        | 0x007B..=0x007F
        | 0x00A0..=0x00BF
        | 0x200C | 0x200D | 0x200E | 0x200F | 0x061C
        // U+25CC DOTTED CIRCLE is Common: the one `hb_insert_dotted_circle`
        // puts at the start of the text belongs to the mark after it.
        | 0x25CC
        // INHERITED combining-mark blocks: must extend the preceding
        // real-script segment so GSUB dispatches under the right
        // priority. Matches `buffer::is_common_or_inherited`.
        | 0x0300..=0x036F
        | 0x1DC0..=0x1DFF
        | 0x20D0..=0x20FF
        | 0xFE20..=0xFE2F
    ) || crate::unicode::is_scriptless_default_ignorable(ch)
}
