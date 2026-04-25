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

use alloc::vec;
use alloc::vec::Vec;

use sigilbuzz::Glyph;

use crate::linebreak::{line_break_opportunities, BreakOpportunity};

/// Inputs to [`wrap_lines`].
#[derive(Debug, Clone, Copy)]
pub struct WrapOptions {
    /// Maximum advance width per line, in the same units as
    /// `Glyph::x_advance` (font design units, unless the caller scales
    /// them first).
    pub max_width: f32,
    /// When `true`, the wrapper only breaks at allowed UAX 14
    /// opportunities. When `false`, it falls back to mid-cluster
    /// breaks when no opportunity is reachable inside the budget — a
    /// safety valve for very narrow `max_width` values that would
    /// otherwise produce a single overflowing line.
    pub break_at_word_boundaries: bool,
}

impl Default for WrapOptions {
    fn default() -> Self {
        Self {
            max_width: f32::INFINITY,
            break_at_word_boundaries: true,
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
    /// trailing UAX 14 space-class characters ignored (LB7: trailing
    /// spaces hang into the right margin and do not count toward the
    /// line's measured width). This matches the budget the wrapper
    /// enforced when picking the break.
    pub width: f32,
}

/// Walks a slice of shaped [`Glyph`]s alongside its source `text`,
/// breaking at UAX 14 opportunities whenever the running advance would
/// exceed `options.max_width`.
///
/// Each glyph's [`sigilbuzz::Glyph::cluster`] field is treated as the
/// byte offset of the source codepoint. Multi-glyph clusters
/// (ligatures, mark stacks) accumulate their advance onto the same
/// source span; the wrapper never splits a cluster.
///
/// The typical call site is
/// `wrap_lines(&shape(font, buffer, &[])?.glyphs, buffer.text(), options)`.
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
        line_break_opportunities(text).collect();
    if opportunities.last().map(|(p, _)| *p) != Some(text.len()) {
        opportunities.push((text.len(), BreakOpportunity::Mandatory));
    }

    // Build a prefix-sum of advances. `prefix[i]` is the total advance
    // for bytes in `[0, i)`, so the width of any half-open span
    // `[a, b)` is just `prefix[b] - prefix[a]`.
    let mut prefix: Vec<f32> = Vec::with_capacity(text.len() + 2);
    prefix.push(0.0);
    for advance in advance_at_byte.iter().take(text.len() + 1) {
        let last = *prefix.last().expect("non-empty");
        prefix.push(last + advance);
    }
    let span_width = |from: usize, to: usize| prefix[to] - prefix[from];
    // UAX 14 LB7: trailing spaces hang into the right margin and do
    // not count toward the line's measured width. Walk back from `to`
    // skipping space-class characters before computing the budget.
    let trim_end = |to: usize| -> usize {
        let mut end = to;
        while end > 0 {
            match text[..end].char_indices().next_back() {
                Some((b, ch)) if ch == ' ' || ch == '\t' || ch == '\u{3000}' => {
                    end = b;
                }
                _ => break,
            }
        }
        end
    };
    let measure = |from: usize, to: usize| span_width(from, trim_end(to));

    // The per-line `width` we report to callers is the *measured*
    // width — i.e. trailing space-class characters do not contribute
    // to it, matching UAX 14 LB7 ("trailing spaces hang into the
    // right margin"). The wrapping decisions above already use
    // `measure`; we have to use it here too so the public field
    // agrees with the budget the wrapper enforced.
    let line_width = |from: usize, to: usize| span_width(from, trim_end(to));

    let mut lines: Vec<LineRange> = Vec::new();
    let mut line_start = 0usize;
    let mut last_allowed: Option<usize> = None;

    for &(offset, kind) in &opportunities {
        if offset <= line_start {
            continue;
        }
        let measured = measure(line_start, offset);
        match kind {
            BreakOpportunity::Mandatory => {
                lines.push(LineRange {
                    start_byte: line_start,
                    end_byte: offset,
                    width: line_width(line_start, offset),
                });
                line_start = offset;
                last_allowed = None;
            }
            BreakOpportunity::Allowed => {
                if measured <= options.max_width {
                    // Still fits; remember as the latest valid break
                    // and keep packing.
                    last_allowed = Some(offset);
                } else {
                    // We just overflowed. Fall back to the previous
                    // allowed break, if any.
                    if let Some(prev) = last_allowed {
                        lines.push(LineRange {
                            start_byte: line_start,
                            end_byte: prev,
                            width: line_width(line_start, prev),
                        });
                        line_start = prev;
                        // The current opportunity may itself fit on
                        // the new line — re-evaluate.
                        let new_measured = measure(line_start, offset);
                        if new_measured <= options.max_width {
                            last_allowed = Some(offset);
                        } else {
                            last_allowed = None;
                            if !options.break_at_word_boundaries {
                                // Hard split at the current offset
                                // even though it overflows, so the
                                // wrapper makes forward progress.
                                lines.push(LineRange {
                                    start_byte: line_start,
                                    end_byte: offset,
                                    width: line_width(line_start, offset),
                                });
                                line_start = offset;
                            }
                        }
                    } else if !options.break_at_word_boundaries {
                        lines.push(LineRange {
                            start_byte: line_start,
                            end_byte: offset,
                            width: line_width(line_start, offset),
                        });
                        line_start = offset;
                        last_allowed = None;
                    } else {
                        // Forced to keep this oversized run on one
                        // line — there is no earlier breakpoint.
                        last_allowed = Some(offset);
                    }
                }
            }
            BreakOpportunity::Prohibited => {}
        }
    }

    // Flush any tail that did not get a Mandatory sentinel.
    if line_start < text.len() {
        lines.push(LineRange {
            start_byte: line_start,
            end_byte: text.len(),
            width: line_width(line_start, text.len()),
        });
    }

    lines
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
            },
        );
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].start_byte, 0);
        assert_eq!(lines[0].end_byte, 5);
    }

    #[test]
    fn the_quick_brown_fox_breaks_after_quick() {
        // Each char advance == 10. "The quick" is 9 chars → 90.
        // max_width == 90 forces a break right after "quick".
        let text = "The quick brown fox";
        let shaped = shape_uniform(text, 10);
        let lines = wrap_lines(
            &shaped,
            text,
            WrapOptions {
                max_width: 90.0,
                break_at_word_boundaries: true,
            },
        );
        assert!(lines.len() >= 2, "got {} lines: {:?}", lines.len(), lines);
        // First line should contain "The quick" — break lands at the
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
            },
        );
        assert!(lines.len() >= 3);
        for line in &lines {
            assert!(line.width <= 25.0); // a bit of slack for sentinel
        }
    }

    #[test]
    fn trailing_spaces_do_not_count_toward_line_width() {
        // "abc   " — 3 letters + 3 trailing spaces, advance 10 each.
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
    fn mixed_cjk_latin_wraps_in_either_regime() {
        let text = "Hello世界";
        let shaped = shape_uniform(text, 10);
        // Wide budget — fits everything.
        let wide = wrap_lines(
            &shaped,
            text,
            WrapOptions {
                max_width: 1000.0,
                break_at_word_boundaries: true,
            },
        );
        assert_eq!(wide.len(), 1);

        // Narrow budget — must break at the Latin/CJK boundary at minimum.
        let narrow = wrap_lines(
            &shaped,
            text,
            WrapOptions {
                max_width: 50.0,
                break_at_word_boundaries: true,
            },
        );
        assert!(narrow.len() >= 2);
    }
}
