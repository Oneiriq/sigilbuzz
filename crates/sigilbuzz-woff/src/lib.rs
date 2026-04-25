//! sigilbuzz-woff — WOFF1 and WOFF2 wrapping / unwrapping.
//!
//! Browsers serve fonts as either [WOFF1] (the legacy zlib-framed
//! format) or [WOFF2] (Brotli + a transformed `glyf`/`loca` pair).
//! sigilbuzz's shaping core works on plain SFNT (TTF/OTF). This
//! crate is the bridge: hand it WOFF bytes, get SFNT bytes back.
//!
//! ```ignore
//! use sigilbuzz_woff::{unwrap_woff2};
//! use sigilbuzz::{Blob, Face};
//!
//! let woff2: &[u8] = std::fs::read("font.woff2")?.leak();
//! let sfnt = unwrap_woff2(woff2)?;
//! let face = Face::parse_bytes(&sfnt, 0)?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # What ships here
//!
//! - [`unwrap_woff1`] / [`wrap_woff1`]: WOFF1 envelope. Both directions
//!   handle zlib-compressed table bodies when the `woff1-deflate`
//!   feature is on (default). With the feature off, `unwrap_woff1`
//!   rejects compressed tables with `Unsupported` and `wrap_woff1`
//!   only emits the uncompressed pass-through layout.
//!   [`wrap_woff1_with_options`] / [`WrapWoff1Options`] expose the
//!   per-table deflate quality knob.
//! - [`unwrap_woff2`]: WOFF2 header + directory parsing, Brotli
//!   decompression (via the `brotli` crate, gated on the default
//!   `woff2` feature), and the inverse `glyf`/`loca` transform.
//!   Reconstructs simple glyphs (with bbox, end-of-contour list,
//!   instructions, and triplet-decoded coordinate deltas) and
//!   composite glyphs (verbatim component records plus optional
//!   trailing instructions).
//! - [`wrap_woff2`]: forward `glyf`/`loca` transform plus Brotli
//!   encoding. Takes raw SFNT bytes and produces a WOFF2 file. Hmtx
//!   transform v1 is not emitted; `hmtx` stays untransformed.
//!
//! # Feature flags
//!
//! - `default = ["std", "woff2", "woff1-deflate"]`.
//! - `woff2`: pulls in the `brotli` runtime dep. Disable it to drop
//!   the encoder + decoder; `unwrap_woff2` / `wrap_woff2` then return
//!   `WoffError::Woff2Disabled` while WOFF1 stays functional.
//! - `woff1-deflate`: pulls in `miniz_oxide` for the WOFF1 zlib codec.
//!   With it disabled, `unwrap_woff1` rejects compressed tables and
//!   `wrap_woff1` only emits uncompressed pass-through bodies.
//! - `std`: currently a no-op marker; reserved for future no_std
//!   callers wanting Vec-free APIs.
//!
//! See `docs/deps.md` in the workspace root for the rationale on the
//! runtime dependencies this crate brings (`brotli`, `miniz_oxide`).

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;
#[cfg(feature = "std")]
extern crate std;

#[cfg(not(feature = "woff2"))]
use alloc::vec::Vec;

mod error;
mod reader;
mod woff1;
#[cfg(feature = "woff2")]
mod woff2;
#[cfg(feature = "woff1-deflate")]
mod zlib;

pub use error::{Result, WoffError};
pub use woff1::{unwrap_woff1, wrap_woff1, wrap_woff1_with_options, WrapWoff1Options};
#[cfg(feature = "woff2")]
pub use woff2::{unwrap_woff2, wrap_woff2, wrap_woff2_with_options, WrapOptions};

#[cfg(not(feature = "woff2"))]
/// Stub returned when the `woff2` feature is disabled.
///
/// Always returns [`WoffError::Woff2Disabled`]. Linking against the
/// stub means a WOFF1-only consumer doesn't pay the cost of the
/// Brotli runtime dep but still compiles against the same public
/// API surface.
pub fn unwrap_woff2(_woff2_bytes: &[u8]) -> Result<Vec<u8>> {
    Err(WoffError::Woff2Disabled)
}

#[cfg(not(feature = "woff2"))]
/// Stub returned when the `woff2` feature is disabled.
///
/// Always returns [`WoffError::Woff2Disabled`].
pub fn wrap_woff2(_sfnt_bytes: &[u8]) -> Result<Vec<u8>> {
    Err(WoffError::Woff2Disabled)
}

#[cfg(all(test, not(feature = "woff2")))]
mod feature_disabled_tests {
    //! Sanity checks for the WOFF2 stub path: with the feature off,
    //! both `unwrap_woff2` and `wrap_woff2` must return
    //! `WoffError::Woff2Disabled` rather than panicking. These tests
    //! also serve as a build-time guard that the workspace still
    //! compiles without `brotli`.

    use super::{unwrap_woff2, wrap_woff2, WoffError};

    #[test]
    fn unwrap_returns_disabled_marker() {
        // Anything goes in — the stub never inspects the bytes.
        let err = unwrap_woff2(&[0u8; 4]).unwrap_err();
        assert!(matches!(err, WoffError::Woff2Disabled));
    }

    #[test]
    fn wrap_returns_disabled_marker() {
        let err = wrap_woff2(&[0u8; 4]).unwrap_err();
        assert!(matches!(err, WoffError::Woff2Disabled));
    }
}
