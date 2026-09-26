//! Bidirectional paragraphs, shaped the way HarfBuzz callers shape them.
//! See [`BidiParagraph`] for the model and the line layout flow.

mod line;
mod shaping;

use alloc::string::String;
use alloc::vec::Vec;
use core::ops::Range;

use crate::buffer::{Direction, Glyph};
use crate::unicode::bidi::BidiInfo;

#[cfg(doc)]
use crate::buffer::Buffer;

/// A run of text at one embedding level: the unit HarfBuzz shapes.
///
/// [`BidiParagraph::runs`] lists a paragraph's runs in logical order;
/// [`BidiParagraph::line_runs`] lists a line's in visual order. A run
/// built by hand (for example a piece of a run cut by a line break) can
/// be passed to [`BidiParagraph::shape_run`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BidiRun {
    /// Byte range of the run in the paragraph text.
    pub range: Range<usize>,
    /// Embedding level. Odd levels are right to left.
    pub level: u8,
}

impl BidiRun {
    /// True for odd (right-to-left) levels.
    ///
    /// ```
    /// use sigilbuzz::BidiRun;
    ///
    /// assert!(BidiRun { range: 0..2, level: 1 }.is_rtl());
    /// assert!(!BidiRun { range: 0..2, level: 2 }.is_rtl());
    /// ```
    #[must_use]
    pub const fn is_rtl(&self) -> bool {
        self.level % 2 == 1
    }

    /// The direction the run is shaped in: [`Direction::Rtl`] for odd
    /// levels, [`Direction::Ltr`] for even ones.
    ///
    /// ```
    /// use sigilbuzz::{BidiRun, Direction};
    ///
    /// assert_eq!(BidiRun { range: 0..1, level: 0 }.direction(), Direction::Ltr);
    /// assert_eq!(BidiRun { range: 0..1, level: 3 }.direction(), Direction::Rtl);
    /// ```
    #[must_use]
    pub const fn direction(&self) -> Direction {
        if self.is_rtl() {
            Direction::Rtl
        } else {
            Direction::Ltr
        }
    }
}

/// One shaped piece of a line: the run and its glyphs.
///
/// [`BidiParagraph::shape_line`] returns these in visual order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShapedBidiRun {
    /// The run that was shaped, with the level it was shaped at.
    pub run: BidiRun,
    /// The run's glyphs, left to right. Clusters are byte offsets into
    /// the paragraph text.
    pub glyphs: Vec<Glyph>,
}

/// A paragraph of text with its UAX #9 embedding levels, ready to be
/// shaped run by run.
///
/// HarfBuzz leaves the Unicode bidirectional algorithm (UAX #9) to its
/// caller. The caller resolves the paragraph's embedding levels, cuts
/// the text into runs of one level, shapes each run in logical order
/// with the run's direction (odd levels are right to left), and then
/// puts the runs in visual order. [`BidiParagraph`] does the same
/// thing:
///
/// - [`BidiParagraph::new`] resolves the levels (UAX #9 through rule
///   L1, with the whole text as one line) and splits the text into
///   [`BidiRun`]s, each a byte range and a level, in logical order.
/// - [`BidiParagraph::shape_run`] shapes one run, or any piece of one,
///   with its direction. The paragraph text around the piece becomes
///   the buffer's pre- and post-context, so Arabic joining and other
///   cursive connections see the letters across run edges. Each glyph's
///   [`Glyph::cluster`] is a byte offset into the paragraph text.
/// - [`BidiParagraph::line_runs`] orders the runs of one line for
///   display: rule L1 for the end of that line, then rule L2 over the
///   runs.
/// - [`BidiParagraph::shape_line`] and [`BidiParagraph::shape`] put the
///   two together for a line and for a paragraph that fits on one line.
///
/// The text is never reordered. Every glyph's cluster points at the
/// character it came from in the text the caller passed in, and script
/// segmentation inside a run works as it does for [`crate::shape`].
///
/// ```
/// use sigilbuzz::{BidiParagraph, Buffer, Direction, Face, Font};
///
/// let data = include_bytes!("../tests/fixtures/amiri_regular.ttf");
/// let font = Font::new(Face::parse_bytes(data, 0)?, 16.0);
///
/// // "abc " then two Arabic letters: an LTR paragraph with one RTL run.
/// let text = "abc \u{0628}\u{0627}";
/// let paragraph = BidiParagraph::new(text, None);
/// assert_eq!(paragraph.direction(), Direction::Ltr);
/// assert_eq!(paragraph.runs().len(), 2);
///
/// let run = paragraph.shape(&font, &Buffer::new(), &[])?;
/// let clusters: Vec<u32> = run.glyphs.iter().map(|g| g.cluster).collect();
/// // Left to right: a, b, c, space, then the Arabic run in visual order,
/// // alef (byte 6) left of beh (byte 4).
/// assert_eq!(clusters, [0, 1, 2, 3, 6, 4]);
/// # Ok::<(), sigilbuzz::Error>(())
/// ```
///
/// # Laying out lines
///
/// UAX #9 reorders each line on its own, so a paragraph that wraps must
/// not be reordered as a whole. A layout engine works in logical order
/// until it knows the lines:
///
/// 1. Build one [`BidiParagraph`] per paragraph (UAX #9 rule P1 is the
///    caller's: split the text at paragraph separators first). Pass
///    `Some(direction)` to force the base direction, `None` to take it
///    from the first strong character.
/// 2. Measure. Shape every run of [`BidiParagraph::runs`] with
///    [`BidiParagraph::shape_run`] (or the whole paragraph with
///    [`BidiParagraph::shape`]). Clusters are logical byte offsets, so
///    the advance of any byte range is the sum over the glyphs whose
///    cluster falls in it, in whatever order the glyphs come.
///    `sigilbuzz-text-layout`'s `wrap_lines` takes such a glyph slice
///    and the paragraph text as they are.
/// 3. Break the paragraph into lines, each a byte range of the text.
/// 4. For each line, call [`BidiParagraph::shape_line`]. It cuts the
///    line into runs in visual order ([`BidiParagraph::line_runs`]),
///    reshapes each run (a run cut by a line break is shaped as its own
///    piece, with the text beyond the break as context), and returns
///    the pieces left to right. Whitespace at the end of the line takes
///    the paragraph level, as rule L1 asks, so it sits at the line's
///    visual end. An engine with its own items (font or style changes
///    inside a run) intersects them with [`BidiParagraph::line_runs`],
///    shapes each piece with [`BidiParagraph::shape_run`], and orders
///    them with [`BidiParagraph::reorder_visual`].
/// 5. Draw the pieces from the line's left edge. A right-to-left
///    paragraph is usually aligned to the right edge instead.
///
/// # Carets and hit testing
///
/// Within a piece, glyphs come left to right. In a left-to-right piece
/// the clusters grow from left to right; in a right-to-left piece they
/// shrink. A glyph covers the text from its cluster up to the next
/// larger cluster in the piece (or the piece end).
///
/// - Logical offset to x: find the piece whose range holds the offset,
///   then the glyph with the largest cluster not above it. The caret
///   goes on that glyph's left edge in a left-to-right piece and on its
///   right edge in a right-to-left one (inside a ligature, split the
///   glyph's advance between the characters it covers).
/// - x to logical offset: find the glyph under x. In a left-to-right
///   piece the left half maps to the glyph's cluster and the right half
///   to the end of the text it covers; a right-to-left piece swaps the
///   halves.
/// - Affinity: at an offset where the level changes, the caret has two
///   places, after the previous character (upstream) and before the
///   next one (downstream), usually far apart on screen.
///   [`BidiParagraph::level_at`] of `offset - 1` and of `offset` tells
///   which runs they belong to; an engine keeps the affinity with the
///   caret and draws the caret on that side. Where the two levels agree
///   the places coincide.
///
/// ```
/// use sigilbuzz::{BidiParagraph, Buffer, Face, Font, ShapedBidiRun};
///
/// /// Caret x (in font units) for the character at `offset`.
/// fn caret_x(line: &[ShapedBidiRun], offset: usize) -> Option<i32> {
///     let mut x = 0;
///     for piece in line {
///         for glyph in &piece.glyphs {
///             if glyph.cluster as usize == offset {
///                 let edge = if piece.run.is_rtl() { glyph.x_advance } else { 0 };
///                 return Some(x + edge);
///             }
///             x += glyph.x_advance;
///         }
///     }
///     None
/// }
///
/// let data = include_bytes!("../tests/fixtures/amiri_regular.ttf");
/// let font = Font::new(Face::parse_bytes(data, 0)?, 1000.0);
/// let text = "ab \u{0628}\u{0627}";
/// let paragraph = BidiParagraph::new(text, None);
/// let line = paragraph.shape_line(&font, &Buffer::new(), &[], 0..text.len())?;
///
/// // The Arabic run is right to left: its first character (beh, byte 3)
/// // is drawn to the right of alef (byte 5), so its caret is further
/// // right.
/// let beh = caret_x(&line, 3).expect("beh has a glyph");
/// let alef = caret_x(&line, 5).expect("alef has a glyph");
/// assert!(beh > alef);
/// assert_eq!(paragraph.level_at(3), Some(1));
/// # Ok::<(), sigilbuzz::Error>(())
/// ```
///
/// Shaping normalizes each run against the font, as HarfBuzz does
/// (decomposing, reordering marks, recomposing), but every glyph keeps
/// the byte offset of a character it came from, so clusters always
/// index the paragraph text.
///
/// # Limits
///
/// - Vertical text has no bidi runs. Shape it with [`crate::shape`] and
///   a vertical direction.
#[derive(Debug, Clone)]
pub struct BidiParagraph {
    /// The paragraph text, in logical order.
    text: String,
    /// Embedding level (after L1) of every byte of `text`.
    levels: Vec<u8>,
    /// Maximal spans of one level, in logical order.
    runs: Vec<BidiRun>,
    /// Paragraph direction: [`Direction::Ltr`] or [`Direction::Rtl`].
    direction: Direction,
}

impl BidiParagraph {
    /// Resolves the embedding levels of `text` and splits it into runs.
    ///
    /// `direction` forces the paragraph direction: `Some(Direction::Rtl)`
    /// for a right-to-left paragraph, any other `Some` for a left-to-right
    /// one. `None` takes it from the first strong character outside an
    /// isolate (UAX #9 rules P2 and P3), left to right when there is none.
    ///
    /// The whole text is one paragraph: split it at paragraph separators
    /// (UAX #9 rule P1) before calling this.
    ///
    /// ```
    /// use sigilbuzz::{BidiParagraph, BidiRun, Direction};
    ///
    /// let paragraph = BidiParagraph::new("\u{05D0}\u{05D1} abc", None);
    /// assert_eq!(paragraph.direction(), Direction::Rtl);
    /// assert_eq!(
    ///     paragraph.runs(),
    ///     [
    ///         BidiRun { range: 0..5, level: 1 },
    ///         BidiRun { range: 5..8, level: 2 },
    ///     ]
    /// );
    /// ```
    #[must_use]
    pub fn new(text: &str, direction: Option<Direction>) -> Self {
        let direction = direction.map(|d| {
            if d == Direction::Rtl {
                Direction::Rtl
            } else {
                Direction::Ltr
            }
        });
        let info = BidiInfo::new(text, direction);
        let direction = if info.paragraph_direction() == Direction::Rtl {
            Direction::Rtl
        } else {
            Direction::Ltr
        };
        let mut levels = Vec::with_capacity(text.len());
        for (ch, &level) in text.chars().zip(info.levels()) {
            levels.extend(core::iter::repeat(level).take(ch.len_utf8()));
        }
        let mut runs: Vec<BidiRun> = Vec::new();
        for (byte, &level) in levels.iter().enumerate() {
            match runs.last_mut() {
                Some(run) if run.level == level => run.range.end = byte + 1,
                _ => runs.push(BidiRun {
                    range: byte..byte + 1,
                    level,
                }),
            }
        }
        Self {
            text: String::from(text),
            levels,
            runs,
            direction,
        }
    }

    /// The paragraph text, in logical order.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The paragraph direction, [`Direction::Ltr`] or [`Direction::Rtl`].
    #[must_use]
    pub const fn direction(&self) -> Direction {
        self.direction
    }

    /// The paragraph embedding level: 0 for a left-to-right paragraph,
    /// 1 for a right-to-left one.
    #[must_use]
    pub const fn base_level(&self) -> u8 {
        match self.direction {
            Direction::Rtl => 1,
            _ => 0,
        }
    }

    /// The runs of one embedding level, in logical order. They cover the
    /// text without gaps.
    #[must_use]
    pub fn runs(&self) -> &[BidiRun] {
        &self.runs
    }

    /// The embedding level of the character holding byte `offset`, or
    /// `None` at or past the end of the text. Odd levels are right to
    /// left.
    ///
    /// ```
    /// use sigilbuzz::BidiParagraph;
    ///
    /// let paragraph = BidiParagraph::new("ab \u{05D0}", None);
    /// assert_eq!(paragraph.level_at(0), Some(0));
    /// assert_eq!(paragraph.level_at(4), Some(1)); // inside the alef
    /// assert_eq!(paragraph.level_at(5), None);
    /// ```
    #[must_use]
    pub fn level_at(&self, offset: usize) -> Option<u8> {
        self.levels.get(offset).copied()
    }

    /// The run holding byte `offset`, or `None` at or past the end of
    /// the text.
    ///
    /// ```
    /// use sigilbuzz::BidiParagraph;
    ///
    /// let paragraph = BidiParagraph::new("ab \u{05D0}", None);
    /// assert_eq!(paragraph.run_at(3).map(|run| run.range.clone()), Some(3..5));
    /// ```
    #[must_use]
    pub fn run_at(&self, offset: usize) -> Option<&BidiRun> {
        if offset >= self.text.len() {
            return None;
        }
        let index = self.runs.partition_point(|run| run.range.end <= offset);
        self.runs.get(index)
    }

    /// Panics unless `range` is a range of character boundaries inside
    /// the text.
    fn check_range(&self, range: &Range<usize>) {
        assert!(
            range.start <= range.end
                && range.end <= self.text.len()
                && self.text.is_char_boundary(range.start)
                && self.text.is_char_boundary(range.end),
            "byte range {range:?} is not a range of character boundaries in the paragraph"
        );
    }
}

#[cfg(test)]
mod tests;
