//! Shaping a paragraph run by run.

use alloc::vec::Vec;
use core::ops::Range;

use super::{BidiParagraph, BidiRun, ShapedBidiRun};
use crate::buffer::{Buffer, BufferFlags, ShapedRun};
use crate::error::{Error, Result};
use crate::font::Font;
use crate::shape::{shape, Feature};

impl BidiParagraph {
    /// Shapes one run, or any piece of one, in the run's direction.
    ///
    /// `run.range` does not have to be one of [`Self::runs`]: a line
    /// breaker passes the part of a run that falls on one line. The text
    /// in the range is shaped in logical order with the direction of
    /// `run.level` (right to left for odd levels), whatever the levels
    /// inside the range are.
    ///
    /// `buffer` carries the shaping settings: script, language, cluster
    /// level, buffer flags, and any other option a [`Buffer`]
    /// holds. Its text, direction, and context are not used. The run
    /// gets the text in its range, the run's direction, and the text of
    /// its paragraph before and after the range as pre- and
    /// post-context, so letters at the run's edges join the way they do
    /// in the paragraph. Context stops at the paragraph edges, and the
    /// buffer's [`BufferFlags::BOT`] and [`BufferFlags::EOT`] carry over
    /// only to a run that starts or ends a paragraph. A range that spans
    /// paragraphs (no run of [`Self::runs`] does) takes its pre-context
    /// from the paragraph it starts in and its post-context from the one
    /// it ends in.
    ///
    /// The glyphs come in visual order (a right-to-left run reversed, as
    /// [`crate::shape`] returns it), and each glyph's cluster is a byte
    /// offset into the paragraph text.
    ///
    /// # Errors
    ///
    /// Returns the error [`crate::shape`] returns for the font, and
    /// [`Error::Unsupported`] when the range ends past `u32::MAX` bytes,
    /// since [`crate::Glyph::cluster`] is a `u32` byte offset.
    ///
    /// # Panics
    ///
    /// Panics unless `run.range` lies inside the text and both of its
    /// ends are character boundaries.
    ///
    /// ```
    /// use sigilbuzz::{BidiParagraph, Buffer, Face, Font};
    ///
    /// let data = include_bytes!("../../tests/fixtures/amiri_regular.ttf");
    /// let font = Font::new(Face::parse_bytes(data, 0)?, 16.0);
    ///
    /// let paragraph = BidiParagraph::new("ab \u{0628}\u{0628}", None);
    /// let arabic = &paragraph.runs()[1];
    /// let run = paragraph.shape_run(&font, &Buffer::new(), &[], arabic)?;
    /// let clusters: Vec<u32> = run.glyphs.iter().map(|g| g.cluster).collect();
    /// // Right to left: the second beh (byte 5) is drawn first.
    /// assert_eq!(clusters, [5, 3]);
    /// # Ok::<(), sigilbuzz::Error>(())
    /// ```
    pub fn shape_run(
        &self,
        font: &Font<'_>,
        buffer: &Buffer,
        features: &[Feature],
        run: &BidiRun,
    ) -> Result<ShapedRun> {
        self.check_range(&run.range);
        let range = run.range.clone();
        // Every cluster is at most the range's end once offset below.
        if u32::try_from(range.end).is_err() {
            return Err(Error::Unsupported {
                context: "bidi run ends past u32::MAX bytes",
            });
        }
        let mut run_buffer = buffer.clone();
        run_buffer.set_text(&self.text[range.clone()]);
        run_buffer.set_direction(run.direction());
        let (context_start, context_end) = self.context_bounds(&range);
        run_buffer.set_pre_context(&self.text[context_start..range.start]);
        run_buffer.set_post_context(&self.text[range.end..context_end]);
        run_buffer.set_flags(run_flags(
            buffer.flags(),
            range.start == context_start,
            range.end == context_end,
        ));
        let mut shaped = shape(font, &run_buffer, features)?;
        // The range ends within u32 (checked above), and every cluster
        // is an offset into the range, so the sums fit.
        let offset = range.start as u32;
        for glyph in &mut shaped.glyphs {
            glyph.cluster = glyph.cluster.saturating_add(offset);
        }
        Ok(shaped)
    }

    /// Shapes one line: the runs of [`Self::line_runs`], each with
    /// [`Self::shape_run`], in visual order (left to right).
    ///
    /// # Errors
    ///
    /// Returns the error [`crate::shape`] returns for the font.
    ///
    /// # Panics
    ///
    /// Panics unless `line` lies inside the text and both of its ends
    /// are character boundaries.
    ///
    /// ```
    /// use sigilbuzz::{BidiParagraph, Buffer, Face, Font};
    ///
    /// let data = include_bytes!("../../tests/fixtures/amiri_regular.ttf");
    /// let font = Font::new(Face::parse_bytes(data, 0)?, 16.0);
    ///
    /// let text = "ab \u{0628}\u{0628} cd";
    /// let paragraph = BidiParagraph::new(text, None);
    /// let line = paragraph.shape_line(&font, &Buffer::new(), &[], 0..text.len())?;
    /// let levels: Vec<u8> = line.iter().map(|piece| piece.run.level).collect();
    /// assert_eq!(levels, [0, 1, 0]);
    /// assert_eq!(line[1].glyphs.len(), 2);
    /// # Ok::<(), sigilbuzz::Error>(())
    /// ```
    pub fn shape_line(
        &self,
        font: &Font<'_>,
        buffer: &Buffer,
        features: &[Feature],
        line: Range<usize>,
    ) -> Result<Vec<ShapedBidiRun>> {
        self.line_runs(line)
            .into_iter()
            .map(|run| {
                let glyphs = self.shape_run(font, buffer, features, &run)?.glyphs;
                Ok(ShapedBidiRun { run, glyphs })
            })
            .collect()
    }

    /// Shapes the paragraph as a single line: every run in its own
    /// direction, the runs in visual order, the glyphs left to right.
    ///
    /// The result reads like a [`crate::shape`] result for the whole
    /// line, except that the clusters of a right-to-left run inside a
    /// left-to-right paragraph (and the other way round) shrink where the
    /// run's glyphs are drawn. See [`Self::shape_run`] for `buffer`.
    ///
    /// # Errors
    ///
    /// Returns the error [`crate::shape`] returns for the font.
    ///
    /// ```
    /// use sigilbuzz::{BidiParagraph, Buffer, Face, Font};
    ///
    /// let data = include_bytes!("../../tests/fixtures/amiri_regular.ttf");
    /// let font = Font::new(Face::parse_bytes(data, 0)?, 16.0);
    ///
    /// // A right-to-left paragraph with an embedded number.
    /// let text = "\u{0628} 12";
    /// let paragraph = BidiParagraph::new(text, None);
    /// let run = paragraph.shape(&font, &Buffer::new(), &[])?;
    /// let clusters: Vec<u32> = run.glyphs.iter().map(|g| g.cluster).collect();
    /// // The digits read left to right, left of the space and the beh.
    /// assert_eq!(clusters, [3, 4, 2, 0]);
    /// # Ok::<(), sigilbuzz::Error>(())
    /// ```
    pub fn shape(
        &self,
        font: &Font<'_>,
        buffer: &Buffer,
        features: &[Feature],
    ) -> Result<ShapedRun> {
        let line = self.shape_line(font, buffer, features, 0..self.text.len())?;
        let mut glyphs = Vec::with_capacity(line.iter().map(|piece| piece.glyphs.len()).sum());
        for piece in line {
            glyphs.extend(piece.glyphs);
        }
        Ok(ShapedRun { glyphs })
    }
}

impl BidiParagraph {
    /// Where a shaped range's context stops: the start of the paragraph
    /// holding `range.start` and the end of the paragraph holding the
    /// range's last byte (the start's paragraph for an empty range).
    /// Both are the text end for an empty range at the text end.
    fn context_bounds(&self, range: &Range<usize>) -> (usize, usize) {
        let text_end = self.text.len();
        let start = self
            .paragraph_at(range.start)
            .map_or(text_end, |paragraph| paragraph.range.start);
        let last = if range.is_empty() {
            range.start
        } else {
            range.end - 1
        };
        let end = self
            .paragraph_at(last)
            .map_or(text_end, |paragraph| paragraph.range.end);
        (start.min(range.start), end.max(range.end))
    }
}

/// The flags a run's buffer gets from the template's. `BOT` and `EOT`
/// say a buffer starts or ends a paragraph of text, which a run only
/// does when it starts or ends its paragraph (`at_start`, `at_end`), so
/// a run elsewhere drops them. Every other flag carries over unchanged.
fn run_flags(template: BufferFlags, at_start: bool, at_end: bool) -> BufferFlags {
    let mut flags = template;
    flags.set(
        BufferFlags::BOT,
        template.contains(BufferFlags::BOT) && at_start,
    );
    flags.set(
        BufferFlags::EOT,
        template.contains(BufferFlags::EOT) && at_end,
    );
    flags
}

#[cfg(test)]
mod flag_tests {
    use super::*;

    #[test]
    fn only_the_paragraph_edges_keep_bot_and_eot() {
        let all = BufferFlags::BOT | BufferFlags::EOT | BufferFlags::PRESERVE_DEFAULT_IGNORABLES;
        assert_eq!(run_flags(all, true, true), all);
        assert_eq!(
            run_flags(all, true, false),
            BufferFlags::BOT | BufferFlags::PRESERVE_DEFAULT_IGNORABLES
        );
        assert_eq!(
            run_flags(all, false, true),
            BufferFlags::EOT | BufferFlags::PRESERVE_DEFAULT_IGNORABLES
        );
        assert_eq!(
            run_flags(all, false, false),
            BufferFlags::PRESERVE_DEFAULT_IGNORABLES
        );
    }

    #[test]
    fn a_template_without_bot_or_eot_gains_neither() {
        let none = BufferFlags::DO_NOT_INSERT_DOTTED_CIRCLE;
        assert_eq!(run_flags(none, true, true), none);
    }

    #[test]
    fn context_stops_at_the_paragraph_edges() {
        // Paragraphs: "ab\n" (0..3), "cd\r\n" (3..7), "ef" (7..9).
        let paragraph = BidiParagraph::new("ab\ncd\r\nef", None);
        assert_eq!(paragraph.context_bounds(&(0..2)), (0, 3));
        assert_eq!(paragraph.context_bounds(&(4..5)), (3, 7));
        assert_eq!(paragraph.context_bounds(&(3..7)), (3, 7));
        assert_eq!(paragraph.context_bounds(&(7..9)), (7, 9));
        // A range across paragraphs takes both ends' paragraphs.
        assert_eq!(paragraph.context_bounds(&(1..8)), (0, 9));
        // Empty ranges.
        assert_eq!(paragraph.context_bounds(&(3..3)), (3, 7));
        assert_eq!(paragraph.context_bounds(&(9..9)), (9, 9));
        assert_eq!(BidiParagraph::new("", None).context_bounds(&(0..0)), (0, 0));
    }
}
