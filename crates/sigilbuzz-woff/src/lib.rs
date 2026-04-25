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
//! - [`unwrap_woff1`] / [`wrap_woff1`]: WOFF1 envelope, **uncompressed
//!   pass-through only** for now. Compressed WOFF1 returns
//!   `WoffError::Unsupported`. Most modern WOFF1 producers ship only
//!   WOFF2 anyway — compressed WOFF1 is a legacy curiosity that
//!   rarely shows up in practice.
//! - [`unwrap_woff2`]: WOFF2 header + directory parsing, Brotli
//!   decompression (via the `brotli-decompressor` crate, gated on
//!   the default `woff2` feature), and the inverse `glyf`/`loca`
//!   transform. Reconstructs simple glyphs (with bbox, end-of-contour
//!   list, instructions, and triplet-decoded coordinate deltas) and
//!   composite glyphs (verbatim component records plus optional
//!   trailing instructions).
//! - `wrap_woff2`: **deferred to 0.7.0**. The forward `glyf`
//!   transform plus Brotli encoding doubles the implementation
//!   surface and ships in its own follow-up.
//!
//! # Feature flags
//!
//! - `default = ["std", "woff2"]`. Disable `woff2` to drop the
//!   Brotli runtime dep — `unwrap_woff2` then returns
//!   `WoffError::Woff2Disabled` at runtime, while WOFF1 stays
//!   functional.
//! - `std`: currently a no-op marker; reserved for future no_std
//!   callers wanting Vec-free APIs.
//!
//! See `docs/deps.md` in the workspace root for the rationale on the
//! single new runtime dependency this crate brings (`brotli-decompressor`).

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

pub use error::{Result, WoffError};
pub use woff1::{unwrap_woff1, wrap_woff1};
#[cfg(feature = "woff2")]
pub use woff2::unwrap_woff2;

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
