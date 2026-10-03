//! Width-budget wrapping over a sigilbuzz glyph stream.
//!
//! [`wrap_lines`] walks the shaped glyph slice, accumulates advance
//! widths into a running cursor, and breaks at allowed UAX 14
//! opportunities whenever the cursor would exceed
//! [`WrapOptions::max_width`]. The walker stays inside the public
//! sigilbuzz API: glyph clusters tie back to source byte offsets, and
//! the cursor is summed from `Glyph::x_advance`. Callers obtain the
//! glyph slice from `&shape(font, buffer, &[])?.glyphs` (or whatever
//! shaping path they use).
//!
//! Mixed-direction text works the same way: pass
//! `&paragraph.shape(font, buffer, &[])?.glyphs` and `paragraph.text()`
//! for a `sigilbuzz::BidiParagraph`. Its clusters are offsets into the
//! logical text, and the widths are summed by cluster, so the visual
//! order of the glyphs does not matter. Lay each returned line out with
//! `BidiParagraph::shape_line`, which orders that line on its own.

use alloc::vec;
use alloc::vec::Vec;

use sigilbuzz::Glyph;

use crate::class::{line_break_class, LineBreakClass};
use crate::linebreak::{line_break_opportunities_with, BreakOpportunity, WordBreak};

/// Inputs to [`wrap_lines`].
///
/// The default has no width limit, keeps words whole, and uses the
/// default UAX 14 rules ([`WordBreak::Normal`]). Override single fields
/// with struct update syntax:
///
/// ```
/// use sigilbuzz_text_layout::{WordBreak, WrapOptions};
///
/// let options = WrapOptions {
///     max_width: 320.0,
///     word_break: WordBreak::KeepAll,
///     ..WrapOptions::default()
/// };
/// assert!(options.break_at_word_boundaries);
/// ```
#[derive(Debug, Clone, Copy)]
pub struct WrapOptions {
    /// Maximum advance width per line, in the same units as
    /// `Glyph::x_advance` (font design units, unless the caller scales
    /// them first).
    pub max_width: f32,
    /// When `true`, the wrapper only breaks at UAX 14 opportunities,
    /// and a word wider than `max_width` stays whole on its own
    /// overflowing line. When `false`, such a word is split between
    /// glyph clusters so each line fits the budget. A line always
    /// keeps at least one cluster, so a single cluster wider than the
    /// budget still gets a line of its own.
    pub break_at_word_boundaries: bool,
    /// How the UAX 14 opportunities treat letters, as CSS `word-break`
    /// does. [`WordBreak::KeepAll`] wraps Korean between words instead
    /// of between syllables.
    pub word_break: WordBreak,
}

impl Default for WrapOptions {
    fn default() -> Self {
        Self {
            max_width: f32::INFINITY,
            break_at_word_boundaries: true,
            word_break: WordBreak::Normal,
        }
    }
}

/// One output line: the byte range into the source text plus the
/// summed glyph advance for that range.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LineRange {
    /// Start byte offset (inclusive) into the source text.
    pub start_byte: usize,
    /// End byte offset (exclusive) into the source text.
    pub end_byte: usize,
    /// Total advance width consumed by the glyphs of this line, with
    /// trailing spaces and tabs and the line's own mandatory break
    /// characters ignored (LB7: trailing spaces hang
    /// into the right margin and do not count toward the line's
    /// measured width). This matches the budget the wrapper enforced
    /// when picking the break.
    pub width: f32,
}

/// Walks a slice of shaped [`Glyph`]s alongside its source `text`,
/// breaking at UAX 14 opportunities whenever the running advance would
/// exceed `options.max_width`. Mandatory breaks always end a line, and
/// the text before one is wrapped to the budget like any other text.
///
/// Each glyph's [`sigilbuzz::Glyph::cluster`] field is treated as the
/// byte offset of the source codepoint. Multi-glyph clusters
/// (ligatures, mark stacks) accumulate their advance onto the same
/// source span; the wrapper never splits a cluster.
///
/// The typical call site is
/// `wrap_lines(&shape(font, buffer, &[])?.glyphs, buffer.text(), options)`.
///
/// ```
/// use sigilbuzz::Glyph;
/// use sigilbuzz_text_layout::{wrap_lines, WordBreak, WrapOptions};
///
/// // One glyph per letter, each 10 units wide.
/// let text = "abcdef";
/// let glyphs: Vec<Glyph> = (0..6)
///     .map(|cluster| Glyph {
///         glyph_id: 1,
///         cluster,
///         x_advance: 10,
///         y_advance: 0,
///         x_offset: 0,
///         y_offset: 0,
///         unicode_props: 0,
///         indic_position: 0,
///         char_class: 0,
///         combining_class: 0,
///         syllable: 0,
///         flags: sigilbuzz::GlyphFlags::empty(),
///     })
///     .collect();
///
/// // The word stays whole by default.
/// let whole = WrapOptions {
///     max_width: 20.0,
///     break_at_word_boundaries: true,
///     word_break: WordBreak::Normal,
/// };
/// assert_eq!(wrap_lines(&glyphs, text, whole).len(), 1);
///
/// // Without word-boundary breaking it splits between clusters.
/// let split = WrapOptions {
///     max_width: 20.0,
///     break_at_word_boundaries: false,
///     word_break: WordBreak::Normal,
/// };
/// assert_eq!(wrap_lines(&glyphs, text, split).len(), 3);
/// ```
#[must_use]
pub fn wrap_lines(glyphs: &[Glyph], text: &str, options: WrapOptions) -> Vec<LineRange> {
    if text.is_empty() {
        return Vec::new();
    }

    // Sum advances per source byte offset. We index by the cluster
    // attached to each glyph; a cluster of N glyphs (e.g. a ligature)
    // collapses its total advance onto its starting byte.
    let mut advance_at_byte = vec![0.0_f32; text.len() + 1];
    for g in glyphs {
        let idx = (g.cluster as usize).min(text.len());
        advance_at_byte[idx] += g.x_advance as f32;
    }

    // Collect break opportunities from UAX 14, filtered to the
    // positions we'll actually consider. The very first iterator hop
    // is at the *current* prev character's tail, so the offsets are
    // already aligned with `text`.
    let mut opportunities: Vec<(usize, BreakOpportunity)> =
        line_break_opportunities_with(text, options.word_break).collect();
    if opportunities.last().map(|(p, _)| *p) != Some(text.len()) {
        opportunities.push((text.len(), BreakOpportunity::Mandatory));
    }

    // Build a prefix-sum of advances. `prefix[i]` is the total advance
    // for bytes in `[0, i)`, so the width of any half-open span
    // `[a, b)` is just `prefix[b] - prefix[a]`.
    let mut prefix: Vec<f32> = Vec::with_capacity(text.len() + 2);
    let mut running = 0.0_f32;
    prefix.push(running);
    for advance in advance_at_byte.iter().take(text.len() + 1) {
        running += advance;
        prefix.push(running);
    }

    // UAX 14 LB7: trailing spaces hang into the right margin and do
    // not count toward the line's measured width. `hang_end[i]` is `i`
    // with the run of hanging characters directly before it removed.
    // Hanging characters are the spaces (see `hangs`), and the
    // mandatory break classes `BK`, `CR`, `LF`, and `NL`, which end a
    // line without being drawn on it.
    // The table is built in one forward pass, so a long whitespace run
    // costs linear time instead of one backward walk per break
    // opportunity. Only char boundaries are filled in. Every offset
    // looked up below is a char boundary in `0..=text.len()`: break
    // offsets from the iterator, and glyph cluster starts checked with
    // `is_char_boundary`.
    let mut hang_end = vec![0usize; text.len() + 1];
    for (b, ch) in text.char_indices() {
        let next = b + ch.len_utf8();
        hang_end[next] = if hangs(ch) { hang_end[b] } else { next };
    }

    // Width of `[from, to)` without trailing hanging characters. The
    // wrapper uses it both for its break decisions and for the
    // reported `LineRange::width`, so the public field agrees with the
    // budget the wrapper enforced. A span made only of hanging
    // characters measures zero.
    let measure = |from: usize, to: usize| prefix[hang_end[to].max(from)] - prefix[from];

    // Glyph cluster starts, the only places a word may be split when
    // `break_at_word_boundaries` is off. Clusters that do not land on
    // a char boundary inside the text are ignored.
    let mut cluster_start = vec![false; text.len() + 1];
    if !options.break_at_word_boundaries {
        for g in glyphs {
            let idx = g.cluster as usize;
            if idx < text.len() && text.is_char_boundary(idx) {
                cluster_start[idx] = true;
            }
        }
    }

    let mut lines: Vec<LineRange> = Vec::new();
    let mut line_start = 0usize;
    let mut last_allowed: Option<usize> = None;

    for &(offset, kind) in &opportunities {
        if offset <= line_start || kind == BreakOpportunity::Prohibited {
            continue;
        }
        // The budget applies before every kind of break, so the text
        // before a mandatory break wraps like any other text.
        if measure(line_start, offset) > options.max_width {
            // Break at the last opportunity that still fit, if any.
            if let Some(prev) = last_allowed.take() {
                lines.push(line_range(line_start, prev, &measure));
                line_start = prev;
            }
            // What remains is one word wider than the budget. It stays
            // whole on its own line unless word-boundary breaking is
            // off, in which case it splits between glyph clusters.
            if !options.break_at_word_boundaries {
                line_start = split_between_clusters(
                    line_start,
                    offset,
                    &cluster_start,
                    options.max_width,
                    &measure,
                    &mut lines,
                );
            }
        }
        match kind {
            BreakOpportunity::Mandatory => {
                lines.push(line_range(line_start, offset, &measure));
                line_start = offset;
                last_allowed = None;
            }
            BreakOpportunity::Allowed => last_allowed = Some(offset),
            BreakOpportunity::Prohibited => {}
        }
    }

    // Flush any tail that did not get a Mandatory sentinel.
    if line_start < text.len() {
        lines.push(LineRange {
            start_byte: line_start,
            end_byte: text.len(),
            width: measure(line_start, text.len()),
        });
    }

    lines
}

/// Whether `ch` hangs into the right margin at the end of a line: tab,
/// the breaking space separators (U+0020, U+1680, U+2000..=U+200A,
/// U+205F, U+3000), and the mandatory break characters. UAX 14 puts
/// most of these spaces in class BA rather than SP, so the test is by
/// code point.
fn hangs(ch: char) -> bool {
    matches!(
        ch,
        '\t' | ' ' | '\u{1680}' | '\u{2000}'..='\u{200A}' | '\u{205F}' | '\u{3000}'
    ) || matches!(
        line_break_class(ch),
        LineBreakClass::BK | LineBreakClass::CR | LineBreakClass::LF | LineBreakClass::NL
    )
}

/// Builds the [`LineRange`] for `[from, to)`.
fn line_range(from: usize, to: usize, measure: &impl Fn(usize, usize) -> f32) -> LineRange {
    LineRange {
        start_byte: from,
        end_byte: to,
        width: measure(from, to),
    }
}

/// Splits the overflowing span `[start, end)` between glyph clusters.
/// Each line ends at the furthest cluster start that keeps it inside
/// `max_width`, or after its first cluster when even that overflows.
/// Pushes the full lines onto `lines` and returns the start of the
/// remainder, which fits the budget or has no cluster start left to
/// split at.
fn split_between_clusters(
    start: usize,
    end: usize,
    cluster_start: &[bool],
    max_width: f32,
    measure: &impl Fn(usize, usize) -> f32,
    lines: &mut Vec<LineRange>,
) -> usize {
    let mut pos = start;
    while measure(pos, end) > max_width {
        let mut cut = None;
        for b in (pos + 1..end).filter(|&b| cluster_start[b]) {
            let fits = measure(pos, b) <= max_width;
            if fits || cut.is_none() {
                cut = Some(b);
            }
            if !fits {
                break;
            }
        }
        let Some(cut) = cut else {
            break;
        };
        lines.push(line_range(pos, cut, measure));
        pos = cut;
    }
    pos
}

#[cfg(test)]
mod tests {
    use super::*;
    use sigilbuzz::Glyph;

    fn shape_uniform(text: &str, advance: i32) -> Vec<Glyph> {
        // Helper: build fake glyphs where each char is one glyph with
        // the given advance and cluster == byte offset.
        let mut glyphs = Vec::new();
        for (b, _ch) in text.char_indices() {
            glyphs.push(Glyph {
                glyph_id: 1,
                cluster: b as u32,
                x_advance: advance,
                y_advance: 0,
                x_offset: 0,
                y_offset: 0,
                unicode_props: 0,
                indic_position: 0,
                char_class: 0,
                combining_class: 0,
                syllable: 0,
                flags: sigilbuzz::GlyphFlags::empty(),
            });
        }
        glyphs
    }

    #[test]
    fn empty_text_yields_no_lines() {
        let glyphs: Vec<Glyph> = Vec::new();
        let opts = WrapOptions::default();
        assert!(wrap_lines(&glyphs, "", opts).is_empty());
    }

    #[test]
    fn short_text_fits_on_one_line() {
        let text = "hello";
        let glyphs = shape_uniform(text, 10);
        let lines = wrap_lines(
            &glyphs,
            text,
            WrapOptions {
                max_width: 1000.0,
                break_at_word_boundaries: true,
                ..WrapOptions::default()
            },
        );
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].start_byte, 0);
        assert_eq!(lines[0].end_byte, 5);
    }

    #[test]
    fn glyph_order_does_not_change_the_lines() {
        // A bidi paragraph hands over its glyphs in visual order, with
        // right-to-left runs reversed. Widths are summed by cluster, so
        // the lines are the same as for logical order.
        let text = "The quick brown fox";
        let logical = shape_uniform(text, 10);
        let mut visual = logical.clone();
        visual[4..15].reverse();
        let options = WrapOptions {
            max_width: 90.0,
            break_at_word_boundaries: true,
            ..WrapOptions::default()
        };
        assert_eq!(
            wrap_lines(&visual, text, options),
            wrap_lines(&logical, text, options)
        );
    }

    #[test]
    fn the_quick_brown_fox_breaks_after_quick() {
        // Each char advance == 10. "The quick" is 9 chars -> 90.
        // max_width == 90 forces a break right after "quick".
        let text = "The quick brown fox";
        let shaped = shape_uniform(text, 10);
        let lines = wrap_lines(
            &shaped,
            text,
            WrapOptions {
                max_width: 90.0,
                break_at_word_boundaries: true,
                ..WrapOptions::default()
            },
        );
        assert!(lines.len() >= 2, "got {} lines: {:?}", lines.len(), lines);
        // First line should contain "The quick": break lands at the
        // last allowed opportunity inside the 90-unit budget, which is
        // the space after "quick" (offset 9 or 10).
        let first = &text[lines[0].start_byte..lines[0].end_byte];
        assert!(
            first.contains("quick"),
            "expected first line to contain 'quick': {:?} (lines={:?})",
            first,
            lines
        );
    }

    #[test]
    fn hard_line_break_produces_two_ranges() {
        let text = "Line 1\nLine 2";
        let shaped = shape_uniform(text, 10);
        let lines = wrap_lines(
            &shaped,
            text,
            WrapOptions {
                max_width: 1000.0,
                break_at_word_boundaries: true,
                ..WrapOptions::default()
            },
        );
        assert_eq!(lines.len(), 2);
        assert!(text[lines[0].start_byte..lines[0].end_byte].starts_with("Line 1"));
        assert!(text[lines[1].start_byte..lines[1].end_byte].starts_with("Line 2"));
    }

    #[test]
    fn cjk_wraps_at_every_grapheme() {
        // Each ideograph is 3 bytes UTF-8. Force max_width that fits
        // exactly two ideographs.
        let text = "世界世界世界";
        let shaped = shape_uniform(text, 10);
        let lines = wrap_lines(
            &shaped,
            text,
            WrapOptions {
                max_width: 20.0,
                break_at_word_boundaries: true,
                ..WrapOptions::default()
            },
        );
        assert!(lines.len() >= 3);
        for line in &lines {
            assert!(line.width <= 25.0); // a bit of slack for sentinel
        }
    }

    #[test]
    fn trailing_spaces_do_not_count_toward_line_width() {
        // "abc   ": 3 letters + 3 trailing spaces, advance 10 each.
        // Per UAX 14 LB7 trailing spaces hang into the right margin,
        // so the reported width must be 30 (the letters only), not 60.
        let text = "abc   ";
        let shaped = shape_uniform(text, 10);
        let lines = wrap_lines(
            &shaped,
            text,
            WrapOptions {
                max_width: 30.0,
                break_at_word_boundaries: true,
                ..WrapOptions::default()
            },
        );
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].start_byte, 0);
        assert_eq!(lines[0].end_byte, 6);
        // Trailing whitespace excluded.
        assert!(
            (lines[0].width - 30.0).abs() < f32::EPSILON,
            "width={} should not count trailing spaces",
            lines[0].width
        );
    }

    #[test]
    fn trailing_unicode_spaces_excluded_from_line_width() {
        // U+2003 EM SPACE is UAX 14 SP, not only the ASCII space. A
        // trailing EM SPACE must be hung into the right margin and not
        // contribute to the reported line width.
        let text = "abc\u{2003}";
        let shaped = shape_uniform(text, 10);
        let lines = wrap_lines(
            &shaped,
            text,
            WrapOptions {
                max_width: 100.0,
                break_at_word_boundaries: true,
                ..WrapOptions::default()
            },
        );
        assert_eq!(lines.len(), 1);
        assert!(
            (lines[0].width - 30.0).abs() < f32::EPSILON,
            "EM SPACE must not count toward width, got {}",
            lines[0].width
        );
    }

    #[test]
    fn mixed_cjk_latin_wraps_in_either_regime() {
        let text = "Hello世界";
        let shaped = shape_uniform(text, 10);
        // Wide budget: fits everything.
        let wide = wrap_lines(
            &shaped,
            text,
            WrapOptions {
                max_width: 1000.0,
                break_at_word_boundaries: true,
                ..WrapOptions::default()
            },
        );
        assert_eq!(wide.len(), 1);

        // Narrow budget: must break at the Latin/CJK boundary at minimum.
        let narrow = wrap_lines(
            &shaped,
            text,
            WrapOptions {
                max_width: 50.0,
                break_at_word_boundaries: true,
                ..WrapOptions::default()
            },
        );
        assert!(narrow.len() >= 2);
    }

    /// Wraps `text` with one 10-unit glyph per char and returns the
    /// text of each line.
    fn wrap_texts(text: &str, max_width: f32, break_at_word_boundaries: bool) -> Vec<&str> {
        let options = WrapOptions {
            max_width,
            break_at_word_boundaries,
            ..WrapOptions::default()
        };
        wrap_texts_with(text, options)
    }

    /// Wraps `text` with one 10-unit glyph per char under `options` and
    /// returns the text of each line.
    fn wrap_texts_with(text: &str, options: WrapOptions) -> Vec<&str> {
        wrap_lines(&shape_uniform(text, 10), text, options)
            .iter()
            .map(|line| &text[line.start_byte..line.end_byte])
            .collect()
    }

    /// "한국어를 공부해요." ("I study Korean.")
    const STUDY: &str = "\u{D55C}\u{AD6D}\u{C5B4}\u{B97C} \u{ACF5}\u{BD80}\u{D574}\u{C694}.";

    fn word_break_options(max_width: f32, word_break: WordBreak) -> WrapOptions {
        WrapOptions {
            max_width,
            word_break,
            ..WrapOptions::default()
        }
    }

    #[test]
    fn keep_all_wraps_korean_between_words() {
        // Each word is 40 units wide (the period adds 10), and the
        // space between them 10. A trailing space hangs.
        let normal = wrap_texts_with(STUDY, word_break_options(60.0, WordBreak::Normal));
        assert_eq!(
            normal,
            [
                "\u{D55C}\u{AD6D}\u{C5B4}\u{B97C} \u{ACF5}",
                "\u{BD80}\u{D574}\u{C694}."
            ]
        );
        let keep_all = wrap_texts_with(STUDY, word_break_options(60.0, WordBreak::KeepAll));
        assert_eq!(
            keep_all,
            [
                "\u{D55C}\u{AD6D}\u{C5B4}\u{B97C} ",
                "\u{ACF5}\u{BD80}\u{D574}\u{C694}."
            ]
        );
    }

    #[test]
    fn keep_all_word_wider_than_the_line_overflows_or_splits() {
        // At 30 units neither word fits. Kept whole by default, split
        // between clusters when word-boundary breaking is off.
        let options = word_break_options(30.0, WordBreak::KeepAll);
        assert_eq!(
            wrap_texts_with(STUDY, options),
            [
                "\u{D55C}\u{AD6D}\u{C5B4}\u{B97C} ",
                "\u{ACF5}\u{BD80}\u{D574}\u{C694}."
            ]
        );
        let split = WrapOptions {
            break_at_word_boundaries: false,
            ..options
        };
        assert_eq!(
            wrap_texts_with(STUDY, split),
            [
                "\u{D55C}\u{AD6D}\u{C5B4}",
                "\u{B97C} ",
                "\u{ACF5}\u{BD80}\u{D574}",
                "\u{C694}."
            ]
        );
    }

    #[test]
    fn break_all_splits_latin_words_to_fit() {
        let options = word_break_options(30.0, WordBreak::BreakAll);
        assert_eq!(
            wrap_texts_with("abcdef ghi", options),
            ["abc", "def ", "ghi"]
        );
        let normal = word_break_options(30.0, WordBreak::Normal);
        assert_eq!(wrap_texts_with("abcdef ghi", normal), ["abcdef ", "ghi"]);
    }

    #[test]
    fn text_before_a_mandatory_break_wraps_to_the_budget() {
        let lines = wrap_texts("The quick brown\nfox", 90.0, true);
        assert_eq!(lines, ["The quick ", "brown\n", "fox"]);
    }

    #[test]
    fn line_break_characters_do_not_count_toward_width() {
        // "The quick" is exactly 90 wide. The newline ends the line
        // without being drawn, so it must not push the line over.
        let text = "The quick\r\nfox";
        let lines = wrap_lines(
            &shape_uniform(text, 10),
            text,
            WrapOptions {
                max_width: 90.0,
                break_at_word_boundaries: true,
                ..WrapOptions::default()
            },
        );
        assert_eq!(lines.len(), 2);
        assert_eq!(
            &text[lines[0].start_byte..lines[0].end_byte],
            "The quick\r\n"
        );
        assert!((lines[0].width - 90.0).abs() < f32::EPSILON);
    }

    #[test]
    fn blank_line_measures_zero() {
        let text = "a\n\nb";
        let lines = wrap_lines(&shape_uniform(text, 10), text, WrapOptions::default());
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[1].width, 0.0);
    }

    #[test]
    fn overlong_word_stays_whole_at_word_boundaries() {
        let lines = wrap_texts("a verylongword b c", 30.0, true);
        assert_eq!(lines, ["a ", "verylongword ", "b c"]);
    }

    #[test]
    fn overlong_word_splits_between_clusters_without_word_boundaries() {
        let lines = wrap_texts("a verylongword b", 30.0, false);
        assert_eq!(lines, ["a ", "ver", "ylo", "ngw", "ord ", "b"]);
    }

    #[test]
    fn cluster_split_applies_before_a_mandatory_break() {
        let lines = wrap_texts("abcdef\nxy", 20.0, false);
        assert_eq!(lines, ["ab", "cd", "ef\n", "xy"]);
    }

    #[test]
    fn cluster_split_never_splits_a_multi_glyph_cluster() {
        // Glyphs 0 and 1 form one cluster at byte 0 ("ab" as a
        // ligature), so the only cluster starts are bytes 0 and 2.
        let text = "abc";
        let mut glyphs = shape_uniform(text, 10);
        glyphs[1].cluster = 0;
        let lines = wrap_lines(
            &glyphs,
            text,
            WrapOptions {
                max_width: 10.0,
                break_at_word_boundaries: false,
                ..WrapOptions::default()
            },
        );
        let texts: Vec<&str> = lines
            .iter()
            .map(|line| &text[line.start_byte..line.end_byte])
            .collect();
        assert_eq!(texts, ["ab", "c"]);
    }
}
