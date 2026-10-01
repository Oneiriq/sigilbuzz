//! Script segmentation: splits the post-cmap code points into
//! same-script runs and picks the script tags each run's lookups try.

use alloc::vec::Vec;

use crate::buffer::script_priority_for;
use crate::unicode::{is_hangul_tone_mark, script_of, Script};

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
/// before them, and so does a Hangul tone mark. A leading COMMON-only
/// run joins the first real script after it, the way HarfBuzz gives a
/// buffer the script of its first non-COMMON character, and text with
/// no real script at all shapes as `Script::Other` under DFLT. Always
/// returns at least one segment covering the whole `codepoints` range
/// for a non-empty input.
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
        } else if is_hangul_tone_mark(ch) {
            // A combining mark of the Hangul script. HarfBuzz shapes
            // the buffer as one run, so the mark normalizes with the
            // character before it and joins its cluster. It stays in
            // that character's segment, and only starts a Hangul one.
            current_script.unwrap_or(raw)
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

/// Shape-time COMMON / INHERITED predicate: the characters that take
/// the script of the segment around them (see
/// [`crate::unicode::is_common_or_inherited`]). The same predicate
/// splits [`crate::buffer::Buffer::script_runs`], so both agree.
pub(super) const fn is_common_for_segmentation(ch: char) -> bool {
    crate::unicode::is_common_or_inherited(ch)
}

/// Rebuilds the post-GSUB segment ranges after `morx` changed the
/// glyph count. `origins[k]` is the pre-morx index output glyph `k`
/// came from, or an out-of-range value for a glyph `morx` inserted.
/// Each output glyph joins the segment of its origin (an inserted
/// glyph joins its left neighbor's), and consecutive glyphs of the
/// same segment form one range.
pub(super) fn remap_segments(
    segments: &[ProcessedSegment],
    origins: &[usize],
) -> Vec<ProcessedSegment> {
    let segment_of = |origin: usize| {
        let idx = segments.partition_point(|s| s.range.end <= origin);
        segments
            .get(idx)
            .filter(|s| s.range.contains(&origin))
            .map(|_| idx)
    };
    let mut out: Vec<ProcessedSegment> = Vec::new();
    let mut current: Option<usize> = None;
    for (k, &origin) in origins.iter().enumerate() {
        let seg = segment_of(origin).or(current).unwrap_or(0);
        let Some(priority) = segments.get(seg).map(|s| s.script_priority) else {
            // No segments at all: nothing to attach the glyph to.
            continue;
        };
        match out.last_mut() {
            Some(last) if current == Some(seg) && last.range.end == k => last.range.end = k + 1,
            _ => out.push(ProcessedSegment {
                range: k..k + 1,
                script_priority: priority,
            }),
        }
        current = Some(seg);
    }
    out
}
