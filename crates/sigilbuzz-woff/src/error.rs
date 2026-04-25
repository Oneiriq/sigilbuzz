//! Error type shared by the WOFF1 and WOFF2 paths.

use core::fmt;

/// Reasons a WOFF wrap or unwrap can fail.
///
/// Keeping the enum tight makes it easy for callers to discriminate
/// between "your input was malformed" (most variants) and "your build
/// didn't enable the feature you need" (`Woff2Disabled`).
#[derive(Debug)]
pub enum WoffError {
    /// Input is too short for the field we tried to read.
    UnexpectedEof {
        /// Byte offset at which we ran out of bytes.
        offset: usize,
        /// What we were trying to read when we hit EOF.
        context: &'static str,
    },
    /// A magic word, reserved field, or version did not match the spec.
    BadMagic {
        /// Byte offset at which the bad value sat.
        offset: usize,
        /// What we were validating (e.g. "WOFF2 signature").
        context: &'static str,
    },
    /// The file's internal lengths and offsets don't add up.
    Malformed {
        /// Byte offset at which the inconsistency was detected.
        offset: usize,
        /// Human-readable description of the problem.
        context: &'static str,
    },
    /// The brotli payload would not decompress.
    BrotliDecode {
        /// Underlying decoder message.
        context: &'static str,
    },
    /// `unwrap_woff2` was called on a build without the `woff2` feature.
    Woff2Disabled,
    /// The crate doesn't yet handle this corner of the spec.
    Unsupported {
        /// What was requested but not yet implemented.
        context: &'static str,
    },
}

impl fmt::Display for WoffError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnexpectedEof { offset, context } => {
                write!(f, "unexpected end of input at offset {offset} while reading {context}")
            }
            Self::BadMagic { offset, context } => {
                write!(f, "bad magic / reserved value at offset {offset} ({context})")
            }
            Self::Malformed { offset, context } => {
                write!(f, "malformed WOFF data at offset {offset}: {context}")
            }
            Self::BrotliDecode { context } => write!(f, "brotli decode failed: {context}"),
            Self::Woff2Disabled => f.write_str(
                "WOFF2 support is gated behind the `woff2` cargo feature, which is currently disabled",
            ),
            Self::Unsupported { context } => write!(f, "unsupported WOFF feature: {context}"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for WoffError {}

/// Convenience alias used throughout the crate.
pub type Result<T> = core::result::Result<T, WoffError>;
