//! Checked Offset16s for the rebuilt GSUB and GPOS subtables.
//!
//! Nearly every offset inside a layout subtable is an Offset16 measured
//! from a parent table, and the rewriters rebuild those parents. A
//! rebuilt parent can grow past what an Offset16 reaches even though
//! the source fit: anchors and ValueRecords get private copies of the
//! Device and VariationIndex tables the source shared between them,
//! and PairSets or rule bodies the source shared may be written once
//! per reference. A narrowing `as u16` would wrap silently and point
//! the offset into unrelated bytes.
//!
//! So every rewriter narrows through [`Offset16Guard`]. A distance past
//! 64 KiB is written as 0 and the guard remembers it; the lookup
//! drivers check the guard after each subtable and report
//! [`SubsetError::Unsupported`] naming the subtable type, so a wrapped
//! or zeroed offset never reaches the output. The mark attachment and
//! PairPos format 1 rewriters avoid most overflows by laying out the
//! small tables first and splitting an oversized subtable into several
//! (see [`crate::gpos`]); the guard fires only when even one piece
//! cannot be addressed.

use core::cell::Cell;

use crate::SubsetError;

/// Remembers whether a rebuild met an Offset16 that could not reach
/// its target.
#[derive(Debug, Default)]
pub(crate) struct Offset16Guard {
    overflowed: Cell<bool>,
}

impl Offset16Guard {
    /// Narrows `distance` to an Offset16. A distance that does not fit
    /// is recorded and written as 0.
    pub(crate) fn narrow(&self, distance: usize) -> u16 {
        u16::try_from(distance).unwrap_or_else(|_| {
            self.record();
            0
        })
    }

    /// Records an overflow found by a writer that narrows on its own.
    pub(crate) fn record(&self) {
        self.overflowed.set(true);
    }

    /// Clears the guard and reports a recorded overflow as an error
    /// carrying `what`, the name of the structure being rebuilt.
    pub(crate) fn check(&self, what: &'static str) -> Result<(), SubsetError> {
        if self.overflowed.replace(false) {
            Err(SubsetError::Unsupported(what))
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Offset16Guard;
    use crate::SubsetError;

    #[test]
    fn distances_that_fit_pass_through() {
        let guard = Offset16Guard::default();
        assert_eq!(guard.narrow(0), 0);
        assert_eq!(guard.narrow(0xFFFF), 0xFFFF);
        assert_eq!(guard.check("x"), Ok(()));
    }

    #[test]
    fn an_overflow_is_zeroed_reported_once_and_cleared() {
        let guard = Offset16Guard::default();
        assert_eq!(guard.narrow(0x1_0000), 0);
        assert_eq!(guard.narrow(12), 12);
        assert_eq!(guard.check("what"), Err(SubsetError::Unsupported("what")));
        assert_eq!(guard.check("what"), Ok(()));
        guard.record();
        assert!(guard.check("again").is_err());
    }
}
