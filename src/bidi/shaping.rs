//! Shaping a paragraph run by run.

use alloc::vec::Vec;
use core::ops::Range;

use super::{BidiParagraph, BidiRun, ShapedBidiRun};
use crate::buffer::{Buffer, BufferFlags, ShapedRun};
use crate::error::Result;
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
    /// `buffer` carries the shaping settings: script, language, NFC
    /// composition, dotted circles, and any other option a [`Buffer`]
    /// holds. Its text, direction, and context are not used. The run
    /// gets the paragraph text in its range, the run's direction, and
    /// the paragraph text before and after the range as pre- and
    /// post-context, so letters at the run's edges join the way they do
    /// in the paragraph.
    ///
    /// The glyphs come in visual order (a right-to-left run reversed, as
    /// [`crate::shape`] returns it), and each glyph's cluster is a byte
    /// offset into the paragraph text.
    ///
    /// # Errors
    ///
    /// Returns the error [`crate::shape`] returns for the font.
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
        let mut run_buffer = buffer.clone();
        run_buffer.set_text(&self.text[range.clone()]);
        run_buffer.set_direction(run.direction());
        run_buffer.set_pre_context(&self.text[..range.start]);
        run_buffer.set_post_context(&self.text[range.end..]);
        run_buffer.set_flags(run_flags(buffer.flags(), &range, self.text.len()));
        let mut shaped = shape(font, &run_buffer, features)?;
        // `new` checked that the text fits in u32, so the sum does too.
        let offset = range.start as u32;
        for glyph in &mut shaped.glyphs {
            glyph.cluster += offset;
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

/// The flags a run's buffer gets from the template's. `BOT` and `EOT`
/// say a buffer starts or ends the text, which a run only does when it
/// starts or ends the paragraph, so a run elsewhere drops them. Every
/// other flag carries over unchanged.
fn run_flags(template: BufferFlags, range: &Range<usize>, text_len: usize) -> BufferFlags {
    let mut flags = template;
    flags.set(
        BufferFlags::BOT,
        template.contains(BufferFlags::BOT) && range.start == 0,
    );
    flags.set(
        BufferFlags::EOT,
        template.contains(BufferFlags::EOT) && range.end == text_len,
    );
    flags
}

#[cfg(test)]
mod flag_tests {
    use super::*;

    #[test]
    fn only_the_paragraph_edges_keep_bot_and_eot() {
        let all = BufferFlags::BOT | BufferFlags::EOT | BufferFlags::PRESERVE_DEFAULT_IGNORABLES;
        assert_eq!(run_flags(all, &(0..10), 10), all);
        assert_eq!(
            run_flags(all, &(0..4), 10),
            BufferFlags::BOT | BufferFlags::PRESERVE_DEFAULT_IGNORABLES
        );
        assert_eq!(
            run_flags(all, &(4..10), 10),
            BufferFlags::EOT | BufferFlags::PRESERVE_DEFAULT_IGNORABLES
        );
        assert_eq!(
            run_flags(all, &(2..6), 10),
            BufferFlags::PRESERVE_DEFAULT_IGNORABLES
        );
    }

    #[test]
    fn a_template_without_bot_or_eot_gains_neither() {
        let none = BufferFlags::DO_NOT_INSERT_DOTTED_CIRCLE;
        assert_eq!(run_flags(none, &(0..10), 10), none);
    }
}
