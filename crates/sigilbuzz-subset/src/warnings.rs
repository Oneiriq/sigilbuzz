//! Diagnostics for source data left out of the output.
//!
//! The subsetter and the instancer follow HarfBuzz when a piece of a
//! table cannot be read: they leave that piece out and carry on, the
//! way HarfBuzz's sanitizer neuters a broken offset, rather than fail
//! the whole run. Each such piece is recorded as a [`SubsetWarning`]
//! naming the table, the byte offset where the problem was found, what
//! was wrong, and what the output left out, and the list comes back in
//! [`crate::SubsetOutput::warnings`] and
//! [`crate::InstancedOutput::warnings`].
//!
//! Rewriters report through a [`Diag`], which knows the table being
//! read. Most of them walk sub-slices of the table, so a `Diag` can
//! turn a position inside any sub-slice back into an offset from the
//! start of the table.

use alloc::collections::BTreeSet;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::fmt;

use sigilbuzz::Error;

/// A piece of the source font that the output leaves out because it
/// could not be read.
///
/// The subsetter and instancer drop a malformed structure, such as a
/// truncated GDEF AttachPoint or a Device table with an unknown delta
/// format, instead of failing, the way HarfBuzz does. Each dropped
/// piece is reported once, so a font can be checked and repaired.
///
/// # Examples
///
/// ```
/// use sigilbuzz::Face;
/// use sigilbuzz_subset::{subset, SubsetInput};
///
/// let data = include_bytes!("../../../tests/fixtures/opensans_regular.ttf");
/// let face = Face::parse_bytes(data, 0)?;
/// let input = SubsetInput {
///     gids: vec![0, 36, 37],
///     ..SubsetInput::default()
/// };
/// let out = subset(&face, &input).expect("Open Sans subsets");
/// for warning in &out.warnings {
///     eprintln!("left out of the subset: {warning}");
/// }
/// assert!(out.warnings.is_empty(), "Open Sans is well formed");
/// # Ok::<(), sigilbuzz::Error>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub struct SubsetWarning {
    /// Tag of the source table the piece belongs to, such as `*b"GDEF"`.
    pub table: [u8; 4],
    /// Byte offset of the problem, counted from the start of `table`
    /// in the source font. For a GSUB or GPOS subtable that is left out
    /// as a whole, the offset of the subtable itself.
    pub offset: usize,
    /// What is wrong at `offset`.
    pub context: &'static str,
    /// What the output left out because of it.
    pub dropped: &'static str,
}

impl fmt::Display for SubsetWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let tag = core::str::from_utf8(&self.table).unwrap_or("????");
        write!(
            f,
            "'{tag}' byte {}: {}; left out {}",
            self.offset, self.context, self.dropped
        )
    }
}

/// The reason a parse error gives, for a warning that locates the
/// problem itself.
pub(crate) fn error_context(err: &Error) -> &'static str {
    match *err {
        Error::Truncated { context, .. }
        | Error::Malformed { context, .. }
        | Error::Unsupported { context } => context,
        Error::MissingTable { .. } => "a table the parser needs is missing",
    }
}

/// Most distinct warnings one run keeps. A hostile font can hold far
/// more malformed pieces than anyone reads, and every one would cost
/// memory. The first ones in sort order are kept.
pub(crate) const MAX_WARNINGS: usize = 1 << 16;

/// Collects the warnings of one subset or instance run, without
/// repeats: some rewriters read a structure twice (the GSUB and GPOS
/// context lookups are rewritten again once lookup indices are known),
/// and a piece shared by many records is visited once per record.
#[derive(Debug, Default)]
pub(crate) struct Warnings {
    list: RefCell<BTreeSet<SubsetWarning>>,
}

impl Warnings {
    /// Records one dropped piece.
    pub(crate) fn push(
        &self,
        table: [u8; 4],
        offset: usize,
        context: &'static str,
        dropped: &'static str,
    ) {
        let mut list = self.list.borrow_mut();
        list.insert(SubsetWarning {
            table,
            offset,
            context,
            dropped,
        });
        if list.len() > MAX_WARNINGS {
            list.pop_last();
        }
    }

    /// Records a parse error found in `table`, whose offsets count from
    /// byte `base` of the table. An error without an offset is placed
    /// at `base`.
    pub(crate) fn parse_error(
        &self,
        table: [u8; 4],
        base: usize,
        err: &Error,
        dropped: &'static str,
    ) {
        let offset = match *err {
            Error::Truncated { offset, .. } | Error::Malformed { offset, .. } => {
                base.saturating_add(offset)
            }
            Error::Unsupported { .. } | Error::MissingTable { .. } => base,
        };
        self.push(table, offset, error_context(err), dropped);
    }

    /// The recorded warnings, sorted and without repeats, at most
    /// [`MAX_WARNINGS`] of them.
    pub(crate) fn into_sorted(self) -> Vec<SubsetWarning> {
        self.list.into_inner().into_iter().collect()
    }
}

/// Where the rewriters of one table report dropped pieces: the sink,
/// the table's tag, and its source bytes. Without a sink (as in unit
/// tests that drive a rewriter directly) every report is ignored.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Diag<'a> {
    sink: Option<&'a Warnings>,
    table: [u8; 4],
    bytes: &'a [u8],
}

impl<'a> Diag<'a> {
    /// A `Diag` that ignores every report.
    pub(crate) const NONE: Diag<'static> = Diag {
        sink: None,
        table: [0; 4],
        bytes: &[],
    };

    /// Reports into `sink`, for no table yet (see [`Diag::for_table`]).
    pub(crate) const fn new(sink: &'a Warnings) -> Self {
        Self {
            sink: Some(sink),
            table: [0; 4],
            bytes: &[],
        }
    }

    /// The same sink, reporting against `table`, whose source bytes are
    /// `bytes`.
    pub(crate) const fn for_table(self, table: [u8; 4], bytes: &'a [u8]) -> Self {
        Self {
            sink: self.sink,
            table,
            bytes,
        }
    }

    /// Offset from the start of the table of byte `rel` of `part`, a
    /// sub-slice of the table's bytes. A slice from elsewhere (only
    /// unit tests build one) is measured from its own start.
    pub(crate) fn offset_of(&self, part: &[u8], rel: usize) -> usize {
        let base = self.bytes.as_ptr() as usize;
        let start = part.as_ptr() as usize;
        if start >= base && start - base <= self.bytes.len() {
            start - base + rel
        } else {
            rel
        }
    }

    /// Reports a problem at `offset` from the start of the table.
    pub(crate) fn at(&self, offset: usize, context: &'static str, dropped: &'static str) {
        if let Some(sink) = self.sink {
            sink.push(self.table, offset, context, dropped);
        }
    }

    /// Reports a problem at byte `rel` of `part` (see
    /// [`Diag::offset_of`]).
    pub(crate) fn in_part(
        &self,
        part: &[u8],
        rel: usize,
        context: &'static str,
        dropped: &'static str,
    ) {
        self.at(self.offset_of(part, rel), context, dropped);
    }

    /// Reports a parse error whose offsets count from the start of the
    /// table.
    pub(crate) fn error(&self, err: &Error, dropped: &'static str) {
        if let Some(sink) = self.sink {
            sink.parse_error(self.table, 0, err, dropped);
        }
    }

    /// Reports a parse error whose offsets count from the start of
    /// `part`, a sub-slice of the table's bytes.
    pub(crate) fn part_error(&self, part: &[u8], err: &Error, dropped: &'static str) {
        if let Some(sink) = self.sink {
            sink.parse_error(self.table, self.offset_of(part, 0), err, dropped);
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::format;
    use alloc::vec;

    use super::{Diag, SubsetWarning, Warnings};
    use sigilbuzz::Error;

    #[test]
    fn parts_are_located_from_the_start_of_the_table() {
        let table = vec![0u8; 64];
        let sink = Warnings::default();
        let diag = Diag::new(&sink).for_table(*b"GPOS", &table);
        diag.in_part(&table[20..], 4, "bad", "a subtable");
        diag.part_error(
            &table[40..],
            &Error::Truncated {
                offset: 2,
                context: "short",
            },
            "an anchor",
        );
        diag.at(7, "odd", "a list");
        let got: vec::Vec<(usize, &str)> = sink
            .into_sorted()
            .iter()
            .map(|w| (w.offset, w.context))
            .collect();
        assert_eq!(got, [(7, "odd"), (24, "bad"), (42, "short")]);
    }

    #[test]
    fn repeats_collapse_and_the_list_is_sorted() {
        let sink = Warnings::default();
        for table in [*b"GSUB", *b"GDEF", *b"GSUB"] {
            sink.push(table, 12, "x", "y");
        }
        let list = sink.into_sorted();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].table, *b"GDEF");
    }

    #[test]
    fn without_a_sink_nothing_is_recorded_or_panics() {
        Diag::NONE.at(3, "x", "y");
        Diag::NONE.in_part(&[1, 2, 3], 1, "x", "y");
    }

    #[test]
    fn display_names_table_offset_and_drop() {
        let w = SubsetWarning {
            table: *b"GDEF",
            offset: 58,
            context: "GDEF AttachPoint truncated",
            dropped: "one glyph's AttachPoint",
        };
        assert_eq!(
            format!("{w}"),
            "'GDEF' byte 58: GDEF AttachPoint truncated; left out one glyph's AttachPoint"
        );
    }

    #[test]
    fn the_list_stops_growing_at_the_cap() {
        let sink = Warnings::default();
        for offset in 0..super::MAX_WARNINGS + 10 {
            sink.push(*b"GPOS", offset, "x", "y");
        }
        let list = sink.into_sorted();
        assert_eq!(list.len(), super::MAX_WARNINGS);
        // The first ones in sort order stay.
        assert_eq!(list.first().map(|w| w.offset), Some(0));
        assert_eq!(list.last().map(|w| w.offset), Some(super::MAX_WARNINGS - 1));
    }
}
