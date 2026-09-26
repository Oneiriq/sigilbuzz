//! Visual order for one line: rule L1 at the line end, then rule L2
//! over the line's runs.

use alloc::vec::Vec;
use core::ops::Range;

use super::{BidiParagraph, BidiRun};
use crate::unicode::bidi::{is_l1_trailing, reorder_visual};
use crate::unicode::bidi_class::bidi_class;

impl BidiParagraph {
    /// The runs of one line, in visual order (left to right).
    ///
    /// `line` is a byte range of the text, for example one line a line
    /// breaker chose. The runs are the paragraph runs cut to the line,
    /// with two rules applied to the line alone:
    ///
    /// - L1: the whitespace, isolate controls, and other invisible
    ///   controls at the end of the line take the paragraph level, so a
    ///   right-to-left paragraph keeps its trailing spaces at the line's
    ///   left end whatever direction the text before them has.
    /// - L2: from the highest level down to the lowest odd level, every
    ///   stretch of runs at that level or higher is reversed.
    ///
    /// A run cut by a line break yields just the part inside the line.
    /// Adjacent runs never share a level.
    ///
    /// # Panics
    ///
    /// Panics unless `line` lies inside the text and both of its ends
    /// are character boundaries.
    ///
    /// ```
    /// use sigilbuzz::{BidiParagraph, BidiRun};
    ///
    /// // "abc", then Hebrew alef bet, space, gimel dalet: a left-to-right
    /// // paragraph with one right-to-left run from byte 4 to 13.
    /// let text = "abc \u{05D0}\u{05D1} \u{05D2}\u{05D3}";
    /// let paragraph = BidiParagraph::new(text, None);
    ///
    /// // On one line the Hebrew run is a single right-to-left run.
    /// assert_eq!(paragraph.line_runs(0..text.len()).len(), 2);
    ///
    /// // Break after the space inside the Hebrew run. On the first line
    /// // that space trails the line and drops to the paragraph level.
    /// assert_eq!(
    ///     paragraph.line_runs(0..9),
    ///     [
    ///         BidiRun { range: 0..4, level: 0 },
    ///         BidiRun { range: 4..8, level: 1 },
    ///         BidiRun { range: 8..9, level: 0 },
    ///     ]
    /// );
    /// assert_eq!(paragraph.line_runs(9..13), [BidiRun { range: 9..13, level: 1 }]);
    /// ```
    #[must_use]
    pub fn line_runs(&self, line: Range<usize>) -> Vec<BidiRun> {
        self.check_range(&line);
        // L1 for the end of this line.
        let body_end = self.text[line.clone()]
            .char_indices()
            .rev()
            .take_while(|&(_, ch)| is_l1_trailing(bidi_class(ch)))
            .last()
            .map_or(line.end, |(i, _)| line.start + i);

        let mut runs: Vec<BidiRun> = Vec::new();
        let first = self.runs.partition_point(|run| run.range.end <= line.start);
        for run in self.runs[first..]
            .iter()
            .take_while(|run| run.range.start < body_end)
        {
            let start = run.range.start.max(line.start);
            let end = run.range.end.min(body_end);
            push_run(&mut runs, start..end, run.level);
        }
        push_run(&mut runs, body_end..line.end, self.base_level());

        let levels: Vec<u8> = runs.iter().map(|run| run.level).collect();
        reorder_visual(&levels)
            .into_iter()
            .map(|i| runs[i].clone())
            .collect()
    }

    /// The paragraph's runs in visual order, with the whole text as one
    /// line: [`Self::line_runs`] over the full text.
    ///
    /// ```
    /// use sigilbuzz::{BidiParagraph, BidiRun};
    ///
    /// // A right-to-left paragraph: the Latin run inside it is drawn
    /// // left of the Hebrew run that precedes it logically.
    /// let paragraph = BidiParagraph::new("\u{05D0}\u{05D1} abc", None);
    /// assert_eq!(
    ///     paragraph.visual_runs(),
    ///     [
    ///         BidiRun { range: 5..8, level: 2 },
    ///         BidiRun { range: 0..5, level: 1 },
    ///     ]
    /// );
    /// ```
    #[must_use]
    pub fn visual_runs(&self) -> Vec<BidiRun> {
        self.line_runs(0..self.text.len())
    }

    /// Rule L2 over a sequence of items with the given embedding levels,
    /// such as the runs of a line: returns the item indices in visual
    /// order, left to right.
    ///
    /// [`Self::line_runs`] already orders the paragraph's own runs. This
    /// is for engines that cut a line into finer items (a font or style
    /// change inside a run): give it the items of one line in logical
    /// order, after applying L1 to the line end as
    /// [`Self::line_runs`] does.
    ///
    /// ```
    /// use sigilbuzz::BidiParagraph;
    ///
    /// // Levels of four items in logical order: LTR, RTL, LTR inside
    /// // the RTL embedding, RTL.
    /// assert_eq!(BidiParagraph::reorder_visual(&[0, 1, 2, 1]), [0, 3, 2, 1]);
    /// ```
    #[must_use]
    pub fn reorder_visual(levels: &[u8]) -> Vec<usize> {
        reorder_visual(levels)
    }
}

/// Appends `range` at `level` to `runs`, merging it into the last run
/// when the levels agree. Empty ranges are dropped.
fn push_run(runs: &mut Vec<BidiRun>, range: Range<usize>, level: u8) {
    if range.is_empty() {
        return;
    }
    if let Some(last) = runs.last_mut() {
        if last.level == level && last.range.end == range.start {
            last.range.end = range.end;
            return;
        }
    }
    runs.push(BidiRun { range, level });
}
