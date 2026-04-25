//! Output pixmap types.

use alloc::vec;
use alloc::vec::Vec;

/// 8-bit alpha pixmap. `data.len() == width * height` and each byte is
/// the pixel coverage in `0..=255`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pixmap {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Row-major alpha samples.
    pub data: Vec<u8>,
}

impl Pixmap {
    /// Allocates a fully transparent pixmap of the requested size.
    #[must_use]
    pub fn new(width: u32, height: u32) -> Self {
        let len = (width as usize).saturating_mul(height as usize);
        Self {
            width,
            height,
            data: vec![0u8; len],
        }
    }

    /// True if either dimension is zero.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// Reads the alpha at `(x, y)`. Returns `0` for out-of-bounds.
    #[must_use]
    pub fn get(&self, x: u32, y: u32) -> u8 {
        if x >= self.width || y >= self.height {
            return 0;
        }
        let idx = y as usize * self.width as usize + x as usize;
        self.data[idx]
    }

    /// Writes the alpha at `(x, y)`. Out-of-bounds writes are ignored.
    pub fn set(&mut self, x: u32, y: u32, a: u8) {
        if x >= self.width || y >= self.height {
            return;
        }
        let idx = y as usize * self.width as usize + x as usize;
        self.data[idx] = a;
    }
}

/// Premultiplied RGBA pixmap. `data.len() == 4 * width * height`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColorPixmap {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Row-major RGBA samples, premultiplied alpha.
    pub data: Vec<u8>,
}

impl ColorPixmap {
    /// Allocates a fully transparent colour pixmap.
    #[must_use]
    pub fn new(width: u32, height: u32) -> Self {
        let len = (width as usize)
            .saturating_mul(height as usize)
            .saturating_mul(4);
        Self {
            width,
            height,
            data: vec![0u8; len],
        }
    }

    /// True if either dimension is zero.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// Reads RGBA at `(x, y)`. Returns `[0;4]` for out-of-bounds.
    #[must_use]
    pub fn get(&self, x: u32, y: u32) -> [u8; 4] {
        if x >= self.width || y >= self.height {
            return [0; 4];
        }
        let idx = (y as usize * self.width as usize + x as usize) * 4;
        [
            self.data[idx],
            self.data[idx + 1],
            self.data[idx + 2],
            self.data[idx + 3],
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixmap_zeroed_on_construction() {
        let p = Pixmap::new(4, 3);
        assert_eq!(p.width, 4);
        assert_eq!(p.height, 3);
        assert_eq!(p.data.len(), 12);
        assert!(p.data.iter().all(|&b| b == 0));
    }

    #[test]
    fn pixmap_set_get_roundtrip() {
        let mut p = Pixmap::new(2, 2);
        p.set(1, 1, 200);
        assert_eq!(p.get(1, 1), 200);
        assert_eq!(p.get(0, 0), 0);
    }

    #[test]
    fn pixmap_oob_set_is_ignored() {
        let mut p = Pixmap::new(2, 2);
        p.set(99, 99, 200);
        assert!(p.data.iter().all(|&b| b == 0));
        assert_eq!(p.get(99, 99), 0);
    }

    #[test]
    fn pixmap_empty_dims() {
        assert!(Pixmap::new(0, 5).is_empty());
        assert!(Pixmap::new(5, 0).is_empty());
        assert!(!Pixmap::new(1, 1).is_empty());
    }

    #[test]
    fn color_pixmap_zeroed_and_sized() {
        let p = ColorPixmap::new(2, 3);
        assert_eq!(p.data.len(), 24);
        assert_eq!(p.get(0, 0), [0, 0, 0, 0]);
    }
}
