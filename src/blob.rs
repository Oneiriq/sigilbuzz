//! Container for raw font bytes.
//!
//! Borrowed and owned data are treated uniformly through
//! [`alloc::borrow::Cow`], so callers can feed sigilbuzz a memory-mapped
//! slice or a freshly read `Vec<u8>` without thinking about which path
//! they are on. The rest of the crate sees a `&[u8]` via
//! [`Blob::as_bytes`].

use alloc::borrow::Cow;
use alloc::vec::Vec;

/// Owned or borrowed font data.
///
/// Construction is cheap in both flavors. Neither path copies the
/// bytes. The borrow form is preferred when the caller has already
/// mapped the font into memory.
#[derive(Debug, Clone)]
pub struct Blob<'a> {
    data: Cow<'a, [u8]>,
}

impl<'a> Blob<'a> {
    /// Wraps a borrowed byte slice.
    #[must_use]
    pub const fn new(data: &'a [u8]) -> Self {
        // `Cow::Borrowed` is not `const`-callable on stable yet, so the
        // const constructor exists only for the borrowed case via a
        // manual construction below in the non-const path.
        Self::from_borrowed(data)
    }

    const fn from_borrowed(data: &'a [u8]) -> Self {
        Self {
            data: Cow::Borrowed(data),
        }
    }

    /// Wraps an owned `Vec<u8>`. The returned blob has `'static`
    /// lifetime.
    #[must_use]
    pub fn from_vec(data: Vec<u8>) -> Blob<'static> {
        Blob {
            data: Cow::Owned(data),
        }
    }

    /// Length of the underlying data in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// True when no bytes are carried.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Direct byte-slice view used by every parser in the crate.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.data
    }
}

impl AsRef<[u8]> for Blob<'_> {
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}

#[cfg(feature = "std")]
impl Blob<'static> {
    /// Reads an entire file from disk into an owned blob. Only
    /// available with the `std` feature.
    pub fn from_path(path: impl AsRef<std::path::Path>) -> std::io::Result<Self> {
        let bytes = std::fs::read(path)?;
        Ok(Self::from_vec(bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn borrowed_blob_reports_length_and_bytes() {
        let b = Blob::new(b"hello");
        assert_eq!(b.len(), 5);
        assert!(!b.is_empty());
        assert_eq!(b.as_bytes(), b"hello");
    }

    #[test]
    fn owned_blob_has_static_lifetime() {
        let owned: Blob<'static> = Blob::from_vec(b"world".to_vec());
        assert_eq!(owned.as_bytes(), b"world");
    }

    #[test]
    fn empty_blob_is_empty() {
        let b = Blob::new(&[]);
        assert!(b.is_empty());
    }

    #[test]
    fn as_ref_yields_same_bytes() {
        let b = Blob::new(b"abc");
        let r: &[u8] = b.as_ref();
        assert_eq!(r, b"abc");
    }
}
