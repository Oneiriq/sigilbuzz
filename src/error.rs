//! Error type for every public fallible operation in sigilbuzz.
//!
//! A single error enum keeps the public surface small. Variants either
//! carry a byte offset (parse failures — so callers can bisect a bad
//! font) or a brief reason string. The type is `Copy`-free because the
//! reason strings are owned `&'static str` slices; no allocation on the
//! error path.

use core::fmt;

/// Convenience alias used throughout sigilbuzz.
pub type Result<T> = core::result::Result<T, Error>;

/// Every failure mode sigilbuzz can surface to a caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Font data ran out before the parser reached a required field.
    /// `offset` is the byte index at which the parser expected more
    /// data, counted from the start of the blob.
    Truncated {
        /// Byte offset at which more data was expected.
        offset: usize,
        /// Brief human-readable context.
        context: &'static str,
    },

    /// Font data contained a structurally invalid value (e.g. a table
    /// offset pointing outside the blob, a bad magic number, an
    /// unsupported format).
    Malformed {
        /// Byte offset where the invalid value was detected.
        offset: usize,
        /// Brief human-readable context.
        context: &'static str,
    },

    /// The font is structurally valid but omits a table sigilbuzz needs
    /// to service the caller's request.
    MissingTable {
        /// Four-byte SFNT tag (e.g. `b"cmap"`).
        tag: [u8; 4],
    },

    /// The feature in question is recognised but not yet implemented.
    /// Used sparingly during the bootstrap period — every `Unsupported`
    /// variant should have a tracking issue.
    Unsupported {
        /// Brief human-readable context.
        context: &'static str,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated { offset, context } => {
                write!(f, "truncated font data at byte {offset}: {context}")
            }
            Self::Malformed { offset, context } => {
                write!(f, "malformed font data at byte {offset}: {context}")
            }
            Self::MissingTable { tag } => {
                let s = core::str::from_utf8(tag).unwrap_or("????");
                write!(f, "font is missing required table '{s}'")
            }
            Self::Unsupported { context } => {
                write!(f, "unsupported: {context}")
            }
        }
    }
}

// core::error::Error is stable since Rust 1.81 and works in no_std.
impl core::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_truncated_carries_offset() {
        let e = Error::Truncated {
            offset: 42,
            context: "reading table directory",
        };
        let msg = alloc::format!("{e}");
        assert!(msg.contains("42"));
        assert!(msg.contains("table directory"));
    }

    #[test]
    fn display_missing_table_renders_tag_as_ascii() {
        let e = Error::MissingTable { tag: *b"cmap" };
        let msg = alloc::format!("{e}");
        assert!(msg.contains("'cmap'"));
    }

    #[test]
    fn display_missing_table_falls_back_on_non_ascii_tag() {
        let e = Error::MissingTable {
            tag: [0xFF, 0xFE, 0xFD, 0xFC],
        };
        let msg = alloc::format!("{e}");
        assert!(msg.contains("????"));
    }
}
