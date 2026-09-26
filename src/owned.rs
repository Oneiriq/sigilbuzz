//! Lifetime-free font face for caching and cross-thread sharing.
//!
//! [`Face`] borrows the font bytes it was parsed from, which makes it
//! impossible to store in a cache, send to a worker pool, or hold in a
//! `'static` registry without keeping the source [`crate::Blob`] alive
//! alongside it. Consumers that extract many glyph outlines (an MSDF
//! glyph rasterizer, a subsetting service, a batch renderer) ended up
//! re-running [`Face::parse`] once per glyph just to satisfy the
//! borrow.
//!
//! [`OwnedFace`] removes that constraint: it owns the font bytes behind
//! an `Arc<[u8]>`, parses the SFNT table directory exactly once, and
//! hands out short-lived [`Face`] views on demand. It is `Send + Sync`
//! and cheap to clone, so one parse can serve every thread for the
//! lifetime of the font.
//!
//! ```no_run
//! use sigilbuzz::OwnedFace;
//!
//! let bytes = std::fs::read("font.ttf").unwrap();
//! let owned = OwnedFace::parse(bytes, 0).unwrap();
//!
//! // Parse once, extract outlines forever. No lifetime to fight.
//! let face = owned.as_face();
//! let outline = face.glyph_outline(42).unwrap();
//! ```

use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::error::Result;
use crate::face::{Face, TableRecord};

/// A parsed SFNT face that owns its font bytes.
///
/// The owned analog of [`Face`]: same parse, same validation, but the
/// data rides along behind an `Arc<[u8]>` instead of a borrow. Build one
/// with [`OwnedFace::parse`], then call [`OwnedFace::as_face`] to get a
/// [`Face`] view scoped to any call site that needs the full table API.
///
/// Cloning is cheap: the font bytes are shared, only the short table
/// directory is copied.
#[derive(Debug, Clone)]
pub struct OwnedFace {
    data: Arc<[u8]>,
    sfnt_version: u32,
    records: Vec<TableRecord>,
}

impl OwnedFace {
    /// Parses the SFNT directory at the start of `data`, taking
    /// ownership of the bytes.
    ///
    /// Accepts anything convertible into `Arc<[u8]>`: a `Vec<u8>` or
    /// boxed slice moves without copying, an existing `Arc<[u8]>` is
    /// shared, and a `&[u8]` is copied once. `index` selects a font in
    /// a TrueType Collection and must be zero for plain TTF/OTF files,
    /// exactly as with [`Face::parse`].
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Face::parse`]: `Malformed` for a
    /// bad header or out-of-bounds table record, `Unsupported` for
    /// TrueType Collections.
    pub fn parse(data: impl Into<Arc<[u8]>>, index: u32) -> Result<Self> {
        let data: Arc<[u8]> = data.into();
        let (sfnt_version, records) = {
            let face = Face::parse_bytes(&data, index)?;
            (face.sfnt_version(), face.records().to_vec())
        };
        Ok(Self {
            data,
            sfnt_version,
            records,
        })
    }

    /// Returns a borrowed [`Face`] view over the owned bytes.
    ///
    /// This does **not** re-parse the font. The table directory
    /// captured at [`OwnedFace::parse`] time is reused (the record list
    /// is a small copy, typically a few hundred bytes). The returned
    /// `Face` borrows from `self`, so it is meant to be created where
    /// needed and dropped at the end of the call, not stored.
    #[must_use]
    pub fn as_face(&self) -> Face<'_> {
        // Invariant: `records` came from a successful `parse_bytes` of
        // exactly these bytes, so every record range is still in bounds.
        Face::from_raw_parts(&self.data, self.sfnt_version, self.records.clone())
    }

    /// The raw font bytes.
    #[must_use]
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// A shared handle to the raw font bytes.
    ///
    /// Useful when a consumer needs to key a cache by the underlying
    /// allocation or hand the bytes to another owner without copying.
    #[must_use]
    pub fn data_arc(&self) -> Arc<[u8]> {
        Arc::clone(&self.data)
    }

    /// Raw SFNT version word, as [`Face::sfnt_version`].
    #[must_use]
    pub fn sfnt_version(&self) -> u32 {
        self.sfnt_version
    }

    /// Number of tables in the directory, as [`Face::num_tables`].
    #[must_use]
    pub fn num_tables(&self) -> usize {
        self.records.len()
    }
}

// One parse serving many threads is the whole point. Pin it at compile
// time so a future field can't silently take it away.
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<OwnedFace>();
};

#[cfg(test)]
mod tests {
    use super::*;

    // A minimal valid SFNT: TrueType version, one empty table.
    fn tiny_font() -> Vec<u8> {
        let mut d = Vec::new();
        d.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // sfntVersion
        d.extend_from_slice(&1u16.to_be_bytes()); // numTables
        d.extend_from_slice(&[0u8; 6]); // search/entry/range
        d.extend_from_slice(b"test"); // tag
        d.extend_from_slice(&0u32.to_be_bytes()); // checksum
        d.extend_from_slice(&28u32.to_be_bytes()); // offset (just past directory)
        d.extend_from_slice(&4u32.to_be_bytes()); // length
        d.extend_from_slice(&[1, 2, 3, 4]); // table payload
        d
    }

    #[test]
    fn parse_matches_borrowed_face() {
        let bytes = tiny_font();
        let owned = OwnedFace::parse(bytes.clone(), 0).expect("owned parse");
        let face = Face::parse_bytes(&bytes, 0).expect("borrowed parse");

        assert_eq!(owned.sfnt_version(), face.sfnt_version());
        assert_eq!(owned.num_tables(), face.num_tables());
        assert_eq!(owned.as_face().records(), face.records());
        assert_eq!(owned.data(), bytes.as_slice());
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!(OwnedFace::parse(&[0u8; 4][..], 0).is_err());
    }

    #[test]
    fn as_face_reads_tables_without_reparse() {
        let owned = OwnedFace::parse(tiny_font(), 0).expect("parse");
        let face = owned.as_face();
        assert_eq!(face.table_bytes(*b"test").expect("table"), &[1, 2, 3, 4]);
    }

    #[test]
    fn clones_share_data() {
        let owned = OwnedFace::parse(tiny_font(), 0).expect("parse");
        let clone = owned.clone();
        assert!(core::ptr::eq(owned.data().as_ptr(), clone.data().as_ptr()));
    }
}
