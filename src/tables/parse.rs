//! Primitives for reading OpenType / SFNT tables.
//!
//! OpenType stores everything big-endian, so this module exposes a tiny
//! cursor-style reader that tracks an offset and surfaces
//! [`Error::Truncated`] the moment the cursor would overrun. Every table
//! parser in sigilbuzz is built on top of [`Reader`].
//!
//! The reader never allocates, never panics on malformed input, and does
//! not use `unsafe`. It is meant to be the one obvious place where byte
//! parsing lives, so that other modules never reach for raw
//! `try_into().unwrap()` calls or ad-hoc `u16::from_be_bytes` shuffles.

use crate::error::{Error, Result};

/// Forward-only cursor over a byte slice, big-endian.
///
/// The cursor owns the current offset but borrows the underlying slice,
/// so an entire table parse can operate on `&'a [u8]` without copying.
#[derive(Debug, Clone, Copy)]
pub struct Reader<'a> {
    data: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    /// Wraps a slice. Starts at byte zero.
    #[must_use]
    pub const fn new(data: &'a [u8]) -> Self {
        Self { data, offset: 0 }
    }

    /// Creates a reader positioned at `offset` bytes from the start of
    /// `data`.
    pub fn at(data: &'a [u8], offset: usize) -> Result<Self> {
        if offset > data.len() {
            return Err(Error::Truncated {
                offset,
                context: "Reader::at offset past end",
            });
        }
        Ok(Self { data, offset })
    }

    /// Returns the absolute offset the reader is about to read from.
    #[must_use]
    pub const fn position(&self) -> usize {
        self.offset
    }

    /// Number of bytes remaining ahead of the cursor.
    #[must_use]
    pub const fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.offset)
    }

    /// True if no more bytes can be read.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.remaining() == 0
    }

    /// Advances the cursor by `n` bytes without reading them.
    pub fn skip(&mut self, n: usize) -> Result<()> {
        self.ensure(n, "skip")?;
        self.offset += n;
        Ok(())
    }

    /// Jumps the cursor to an absolute offset.
    pub fn seek(&mut self, offset: usize) -> Result<()> {
        if offset > self.data.len() {
            return Err(Error::Truncated {
                offset,
                context: "seek past end",
            });
        }
        self.offset = offset;
        Ok(())
    }

    /// Peeks the next `n` bytes without advancing.
    pub fn peek_bytes(&self, n: usize) -> Result<&'a [u8]> {
        self.slice_ahead(n, "peek_bytes")
    }

    /// Reads the next `n` bytes, advancing the cursor.
    pub fn read_bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        let bytes = self.slice_ahead(n, "read_bytes")?;
        self.offset += n;
        Ok(bytes)
    }

    /// Reads a `u8`.
    pub fn read_u8(&mut self) -> Result<u8> {
        let b = self.read_bytes(1)?;
        Ok(b[0])
    }

    /// Reads a big-endian `i8`.
    pub fn read_i8(&mut self) -> Result<i8> {
        Ok(self.read_u8()? as i8)
    }

    /// Reads a big-endian `u16`.
    pub fn read_u16(&mut self) -> Result<u16> {
        let b = self.read_bytes(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    /// Reads a big-endian `i16`.
    pub fn read_i16(&mut self) -> Result<i16> {
        let b = self.read_bytes(2)?;
        Ok(i16::from_be_bytes([b[0], b[1]]))
    }

    /// Reads a big-endian `u32`.
    pub fn read_u32(&mut self) -> Result<u32> {
        let b = self.read_bytes(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// Reads a big-endian `i32`.
    pub fn read_i32(&mut self) -> Result<i32> {
        let b = self.read_bytes(4)?;
        Ok(i32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// Reads an SFNT tag: four ASCII-ish bytes treated as an opaque
    /// `[u8; 4]`. Parsers compare tags against byte literals like
    /// `b"cmap"`, which is both deterministic and `no_std`-friendly.
    pub fn read_tag(&mut self) -> Result<[u8; 4]> {
        let b = self.read_bytes(4)?;
        Ok([b[0], b[1], b[2], b[3]])
    }

    /// Reads a big-endian F2DOT14 fixed-point number (signed 2.14,
    /// stored as `i16`) and returns it as `f32`. Used throughout
    /// the variable font tables for normalized axis coordinates
    /// and region boundaries; the raw range `[-2.0, 2.0)` narrows
    /// in practice to `[-1.0, 1.0]`.
    pub fn read_f2dot14(&mut self) -> Result<f32> {
        let raw = self.read_i16()?;
        Ok(f32::from(raw) / 16384.0)
    }

    /// Reads a big-endian F16DOT16 fixed-point number (signed
    /// 16.16, stored as `i32`) and returns it as `f32`. Used for
    /// user-space axis coordinates in `fvar` and `avar`.
    pub fn read_f16dot16(&mut self) -> Result<f32> {
        let raw = self.read_i32()?;
        Ok(raw as f32 / 65536.0)
    }

    // ------------------------------------------------------------------
    // Private helpers
    // ------------------------------------------------------------------

    fn ensure(&self, n: usize, context: &'static str) -> Result<()> {
        if self.remaining() < n {
            return Err(Error::Truncated {
                offset: self.offset,
                context,
            });
        }
        Ok(())
    }

    fn slice_ahead(&self, n: usize, context: &'static str) -> Result<&'a [u8]> {
        self.ensure(n, context)?;
        Ok(&self.data[self.offset..self.offset + n])
    }
}

/// Absolute value of an `f32`.
///
/// `f32::abs` is not available in `core` on the minimum supported Rust
/// version, so `no_std` builds cannot call it. Clearing the sign bit gives
/// the same result for every input, NaN included.
pub(crate) fn abs_f32(x: f32) -> f32 {
    f32::from_bits(x.to_bits() & 0x7fff_ffff)
}

/// The largest integer not above `x`.
///
/// `f32::floor` is not available in `core` on the minimum supported Rust
/// version. Every `f32` of magnitude 2^23 or more is already an integer, so
/// only smaller values need the truncate-and-step-down below. Infinities
/// and NaN come back unchanged.
pub(crate) fn floor_f32(x: f32) -> f32 {
    if x.is_nan() || abs_f32(x) >= 8_388_608.0 {
        return x;
    }
    // Exact: the value fits an i32 and the truncation is a whole number.
    let truncated = x as i32 as f32;
    if truncated > x {
        truncated - 1.0
    } else {
        truncated
    }
}

/// HarfBuzz's `roundf`, which `hb-algs.hh` redefines as
/// `floorf (x + .5f)`: halves round up, toward positive infinity, so
/// `-2.5` rounds to `-2`. The addition happens in `f32`, as in HarfBuzz,
/// so the largest `f32` below one half rounds to one.
pub(crate) fn hb_roundf(x: f32) -> f32 {
    floor_f32(x + 0.5)
}

/// `x` rounded with [`hb_roundf`] to a multiple of `1 / scale`, for a
/// power-of-two `scale`: 65536 for HarfBuzz's 16.16 coordinates, 16384
/// for F2DOT14. Scaling by a power of two is exact, so only the rounding
/// changes the value.
pub(crate) fn hb_round_to(x: f32, scale: f32) -> f32 {
    hb_roundf(x * scale) / scale
}

/// [`hb_roundf`] as an `i32`, saturating at the type's bounds; NaN gives 0.
pub(crate) fn hb_round(x: f32) -> i32 {
    // `as` saturates and maps NaN to zero.
    hb_roundf(x) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abs_f32_matches_std_abs() {
        for x in [
            0.0f32,
            -0.0,
            1.5,
            -1.5,
            f32::MIN,
            f32::MAX,
            f32::INFINITY,
            f32::NEG_INFINITY,
        ] {
            assert_eq!(abs_f32(x).to_bits(), x.abs().to_bits());
        }
        assert!(abs_f32(f32::NAN).is_nan());
        assert!(abs_f32(-f32::NAN).is_sign_positive());
    }

    #[test]
    fn floor_f32_matches_std_floor() {
        for x in [
            0.0f32,
            -0.0,
            0.25,
            -0.25,
            1.0,
            -1.0,
            2.5,
            -2.5,
            8_388_607.5,
            -8_388_607.5,
            16_777_216.0,
            f32::MIN,
            f32::MAX,
            f32::INFINITY,
            f32::NEG_INFINITY,
        ] {
            assert_eq!(floor_f32(x), x.floor(), "{x}");
        }
        assert!(floor_f32(f32::NAN).is_nan());
    }

    #[test]
    fn hb_round_rounds_halves_up() {
        assert_eq!(hb_round(13.5), 14);
        assert_eq!(hb_round(-13.5), -13);
        assert_eq!(hb_round(-13.6), -14);
        assert_eq!(hb_round(-0.5), 0);
        assert_eq!(hb_round(0.49), 0);
        // The sum is rounded to f32 first, as in HarfBuzz.
        assert_eq!(hb_round(0.499_999_97), 1);
        assert_eq!(hb_round(f32::NAN), 0);
        assert_eq!(hb_round(1e20), i32::MAX);
        assert_eq!(hb_round(-1e20), i32::MIN);
        assert_eq!(hb_roundf(-2.5), -2.0);
    }

    #[test]
    fn reads_big_endian_integers_in_sequence() {
        let data = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
        let mut r = Reader::new(&data);
        assert_eq!(r.read_u8().unwrap(), 0x01);
        assert_eq!(r.read_u16().unwrap(), 0x0203);
        assert_eq!(r.read_u32().unwrap(), 0x0405_0607);
        assert_eq!(r.read_u8().unwrap(), 0x08);
        assert!(r.is_empty());
    }

    #[test]
    fn signed_integers_round_trip() {
        let data = [0xFF, 0xFF, 0xFF, 0xFE];
        let mut r = Reader::new(&data);
        assert_eq!(r.read_i16().unwrap(), -1);
        assert_eq!(r.read_i16().unwrap(), -2);
    }

    #[test]
    fn truncated_read_surfaces_offset_and_context() {
        let data = [0x00, 0x01];
        let mut r = Reader::new(&data);
        assert_eq!(r.read_u16().unwrap(), 0x0001);
        let err = r.read_u16().unwrap_err();
        assert!(matches!(
            err,
            Error::Truncated {
                offset: 2,
                context: "read_bytes"
            }
        ));
    }

    #[test]
    fn peek_does_not_advance() {
        let data = [0xAA, 0xBB];
        let mut r = Reader::new(&data);
        assert_eq!(r.peek_bytes(2).unwrap(), &[0xAA, 0xBB]);
        assert_eq!(r.position(), 0);
        assert_eq!(r.read_u8().unwrap(), 0xAA);
    }

    #[test]
    fn seek_clamps_at_end_and_rejects_past_end() {
        let data = [0x00; 4];
        let mut r = Reader::new(&data);
        r.seek(4).expect("seek to end is fine");
        assert!(r.is_empty());
        assert!(r.seek(5).is_err());
    }

    #[test]
    fn read_tag_returns_fixed_array() {
        let data = *b"cmap";
        let mut r = Reader::new(&data);
        assert_eq!(r.read_tag().unwrap(), *b"cmap");
    }
}
