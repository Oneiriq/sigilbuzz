//! Bidirectional paragraphs, shaped the way HarfBuzz callers shape them.
//! See [`BidiParagraph`] for the model and the line layout flow.

mod line;
mod shaping;

use alloc::string::String;
use alloc::vec::Vec;
use core::ops::Range;

use crate::buffer::{Direction, Glyph};
use crate::unicode::bidi::{paragraph_ranges, BidiInfo};

#[cfg(doc)]
use crate::buffer::{Buffer, BufferFlags};

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

/// One paragraph of a [`BidiParagraph`]'s text (UAX #9 rule P1): its
/// byte range and its paragraph embedding level.
///
/// [`BidiParagraph::paragraphs`] lists them in logical order.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BidiParagraphSpan {
    /// Byte range of the paragraph in the text, its closing paragraph
    /// separator included.
    pub range: Range<usize>,
    /// Paragraph embedding level: 0 for a left-to-right paragraph, 1 for
    /// a right-to-left one.
    pub level: u8,
}

impl BidiParagraphSpan {
    /// True for a right-to-left paragraph.
    ///
    /// ```
    /// use sigilbuzz::BidiParagraphSpan;
    ///
    /// assert!(BidiParagraphSpan { range: 0..2, level: 1 }.is_rtl());
    /// ```
    #[must_use]
    pub const fn is_rtl(&self) -> bool {
        self.level % 2 == 1
    }

    /// The paragraph direction: [`Direction::Rtl`] for level 1,
    /// [`Direction::Ltr`] for level 0.
    ///
    /// ```
    /// use sigilbuzz::{BidiParagraphSpan, Direction};
    ///
    /// let span = BidiParagraphSpan { range: 0..1, level: 0 };
    /// assert_eq!(span.direction(), Direction::Ltr);
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

/// Text of one or more paragraphs with its UAX #9 embedding levels,
/// ready to be shaped run by run.
///
/// HarfBuzz leaves the Unicode bidirectional algorithm (UAX #9) to its
/// caller. The caller resolves the paragraph's embedding levels, cuts
/// the text into runs of one level, shapes each run in logical order
/// with the run's direction (odd levels are right to left), and then
/// puts the runs in visual order. [`BidiParagraph`] does the same
/// thing:
///
/// - [`BidiParagraph::new`] splits the text into paragraphs (UAX #9
///   rule P1, see [Paragraphs](#paragraphs)), resolves each paragraph's
///   levels (through rule L1, with the whole paragraph as one line), and
///   splits the text into [`BidiRun`]s, each a byte range and a level,
///   in logical order.
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
/// # Paragraphs
///
/// Rule P1 splits the text after every paragraph separator (Bidi_Class
/// B: LF, CR, U+001C to U+001E, NEL, and U+2029 PARAGRAPH SEPARATOR).
/// The separator belongs to the paragraph it ends, and a separator at
/// the end of the text starts no empty paragraph. Each paragraph then
/// goes through the rest of the algorithm on its own:
///
/// - It gets its own base level: from its first strong character (rules
///   P2 and P3) when [`BidiParagraph::new`] gets `None`, or the forced
///   direction for every paragraph when it gets `Some`.
/// - Embeddings, overrides, and isolates end at its end (rule X8), and
///   its separator takes its base level (rule L1).
/// - Runs, lines, and visual order never cross a paragraph boundary.
///   [`BidiParagraph::runs`] can end one paragraph and start the next
///   at the same level. A line range that spans paragraphs is ordered
///   as one line per paragraph, the paragraphs in logical order.
/// - Shaping context stops at the paragraph edges, and a run that
///   starts or ends a paragraph keeps the buffer's
///   [`BufferFlags::BOT`] or [`BufferFlags::EOT`], so each paragraph
///   shapes as it would in a [`BidiParagraph`] of its own.
///
/// UAX #9 leaves two choices to the implementation, made here as ICU's
/// `ubidi_setPara` makes them: a CR directly followed by an LF is one
/// separator, so the pair ends one paragraph, and a forced direction
/// applies to every paragraph. [`BidiParagraph::paragraphs`] lists the
/// paragraphs with their levels.
///
/// ```
/// use sigilbuzz::{BidiParagraph, BidiParagraphSpan, BidiRun};
///
/// // A Hebrew paragraph, then a Latin one.
/// let text = "\u{05D0}\u{05D1} ab\u{2029}cd \u{05D2}";
/// let paragraph = BidiParagraph::new(text, None);
/// assert_eq!(
///     paragraph.paragraphs(),
///     [
///         BidiParagraphSpan { range: 0..10, level: 1 },
///         BidiParagraphSpan { range: 10..15, level: 0 },
///     ]
/// );
/// // Each paragraph is ordered on its own.
/// assert_eq!(
///     paragraph.visual_runs(),
///     [
///         BidiRun { range: 7..10, level: 1 },
///         BidiRun { range: 5..7, level: 2 },
///         BidiRun { range: 0..5, level: 1 },
///         BidiRun { range: 10..13, level: 0 },
///         BidiRun { range: 13..15, level: 1 },
///     ]
/// );
/// ```
///
/// # Laying out lines
///
/// UAX #9 reorders each line on its own, so a paragraph that wraps must
/// not be reordered as a whole. A layout engine works in logical order
/// until it knows the lines:
///
/// 1. Build a [`BidiParagraph`] from the text. Pass `Some(direction)` to
///    force the base direction, `None` to take each paragraph's from its
///    first strong character. A new paragraph starts a new line, so
///    break each of [`BidiParagraph::paragraphs`] into lines on its own.
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
    /// The text, in logical order.
    text: String,
    /// Embedding level (after L1) of every byte of `text`.
    levels: Vec<u8>,
    /// Maximal spans of one level inside one paragraph, in logical
    /// order.
    runs: Vec<BidiRun>,
    /// The paragraphs (rule P1), in logical order.
    paragraphs: Vec<BidiParagraphSpan>,
    /// The first paragraph's direction ([`Direction::Ltr`] or
    /// [`Direction::Rtl`]), or the forced one for empty text.
    direction: Direction,
}

impl BidiParagraph {
    /// Splits `text` into paragraphs, resolves their embedding levels,
    /// and splits them into runs.
    ///
    /// `direction` forces the direction of every paragraph:
    /// `Some(Direction::Rtl)` for right to left, any other `Some` for left
    /// to right. `None` takes each paragraph's from its first strong
    /// character outside an isolate (UAX #9 rules P2 and P3), left to
    /// right when there is none.
    ///
    /// Paragraph separators split the text into paragraphs (UAX #9 rule
    /// P1). See [Paragraphs](#paragraphs).
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
    ///
    /// // A newline starts a paragraph with its own direction.
    /// let two = BidiParagraph::new("\u{05D0}\u{05D1}\nabc", None);
    /// let directions: Vec<Direction> = two.paragraphs().iter().map(|p| p.direction()).collect();
    /// assert_eq!(directions, [Direction::Rtl, Direction::Ltr]);
    /// ```
    #[must_use]
    pub fn new(text: &str, direction: Option<Direction>) -> Self {
        let forced = direction.map(|d| {
            if d == Direction::Rtl {
                Direction::Rtl
            } else {
                Direction::Ltr
            }
        });
        let mut levels = Vec::with_capacity(text.len());
        let mut runs: Vec<BidiRun> = Vec::new();
        let mut paragraphs = Vec::new();
        for range in paragraph_ranges(text) {
            let Some(paragraph_text) = text.get(range.clone()) else {
                continue;
            };
            let info = BidiInfo::new(paragraph_text, forced);
            let first_run = runs.len();
            let mut byte = range.start;
            for (ch, &level) in paragraph_text.chars().zip(info.levels()) {
                let end = byte + ch.len_utf8();
                levels.extend(core::iter::repeat(level).take(end - byte));
                // Runs of an earlier paragraph are out of reach.
                match runs.get_mut(first_run..).and_then(<[BidiRun]>::last_mut) {
                    Some(run) if run.level == level => run.range.end = end,
                    _ => runs.push(BidiRun {
                        range: byte..end,
                        level,
                    }),
                }
                byte = end;
            }
            let level = u8::from(info.paragraph_direction() == Direction::Rtl);
            paragraphs.push(BidiParagraphSpan { range, level });
        }
        let direction = paragraphs.first().map_or(
            forced.unwrap_or(Direction::Ltr),
            BidiParagraphSpan::direction,
        );
        Self {
            text: String::from(text),
            levels,
            runs,
            paragraphs,
            direction,
        }
    }

    /// The text, in logical order.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The paragraph direction, [`Direction::Ltr`] or [`Direction::Rtl`].
    /// When the text holds several paragraphs, the first one's (see
    /// [`Self::paragraphs`] for each). For empty text, the forced
    /// direction, or [`Direction::Ltr`].
    #[must_use]
    pub const fn direction(&self) -> Direction {
        self.direction
    }

    /// The paragraph embedding level: 0 for a left-to-right paragraph,
    /// 1 for a right-to-left one. When the text holds several
    /// paragraphs, the first one's, as for [`Self::direction`].
    #[must_use]
    pub const fn base_level(&self) -> u8 {
        match self.direction {
            Direction::Rtl => 1,
            _ => 0,
        }
    }

    /// The paragraphs of the text (UAX #9 rule P1), in logical order,
    /// each with its embedding level. They cover the text without gaps.
    /// Empty text has none.
    ///
    /// ```
    /// use sigilbuzz::{BidiParagraph, BidiParagraphSpan, Direction};
    ///
    /// // CR LF is one separator. The text is forced right to left.
    /// let paragraph = BidiParagraph::new("ab\r\ncd", Some(Direction::Rtl));
    /// assert_eq!(
    ///     paragraph.paragraphs(),
    ///     [
    ///         BidiParagraphSpan { range: 0..4, level: 1 },
    ///         BidiParagraphSpan { range: 4..6, level: 1 },
    ///     ]
    /// );
    /// ```
    #[must_use]
    pub fn paragraphs(&self) -> &[BidiParagraphSpan] {
        &self.paragraphs
    }

    /// The paragraph holding byte `offset`, or `None` at or past the end
    /// of the text.
    ///
    /// ```
    /// use sigilbuzz::BidiParagraph;
    ///
    /// let paragraph = BidiParagraph::new("ab\n\u{05D0}", None);
    /// assert_eq!(paragraph.paragraph_at(1).map(|p| p.level), Some(0));
    /// assert_eq!(paragraph.paragraph_at(3).map(|p| p.level), Some(1));
    /// assert_eq!(paragraph.paragraph_at(5), None);
    /// ```
    #[must_use]
    pub fn paragraph_at(&self, offset: usize) -> Option<&BidiParagraphSpan> {
        let index = self
            .paragraphs
            .partition_point(|paragraph| paragraph.range.end <= offset);
        self.paragraphs
            .get(index)
            .filter(|paragraph| paragraph.range.start <= offset)
    }

    /// The runs of one embedding level, in logical order. They cover the
    /// text without gaps. A run never crosses a paragraph boundary, so
    /// the last run of one paragraph and the first of the next can share
    /// a level. Within a paragraph, adjacent runs never do.
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
