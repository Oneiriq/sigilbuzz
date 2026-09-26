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

    /// Reads the alpha at `(x, y)`. Returns `0` for out-of-bounds,
    /// including a pixel that `data` is too short to hold.
    #[must_use]
    pub fn get(&self, x: u32, y: u32) -> u8 {
        pixel_index(self.width, self.height, x, y)
            .and_then(|idx| self.data.get(idx))
            .copied()
            .unwrap_or(0)
    }

    /// Writes the alpha at `(x, y)`. Out-of-bounds writes are ignored.
    pub fn set(&mut self, x: u32, y: u32, a: u8) {
        if let Some(px) =
            pixel_index(self.width, self.height, x, y).and_then(|idx| self.data.get_mut(idx))
        {
            *px = a;
        }
    }
}

/// Row-major index of `(x, y)` in a `width x height` grid, or `None`
/// when the pixel lies outside the grid. The pixmap fields are public,
/// so `data` can be shorter than the dimensions claim. Callers still
/// bounds-check the returned index against `data`.
fn pixel_index(width: u32, height: u32, x: u32, y: u32) -> Option<usize> {
    if x >= width || y >= height {
        return None;
    }
    (y as usize)
        .checked_mul(width as usize)?
        .checked_add(x as usize)
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
    /// Allocates a fully transparent color pixmap.
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

    /// Reads RGBA at `(x, y)`. Returns `[0;4]` for out-of-bounds,
    /// including a pixel that `data` is too short to hold.
    #[must_use]
    pub fn get(&self, x: u32, y: u32) -> [u8; 4] {
        let px = pixel_index(self.width, self.height, x, y).and_then(|idx| {
            let start = idx.checked_mul(4)?;
            self.data.get(start..start.checked_add(4)?)
        });
        match px {
            Some(&[r, g, b, a]) => [r, g, b, a],
            _ => [0; 4],
        }
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

    #[test]
    fn accessors_tolerate_a_buffer_shorter_than_the_dimensions() {
        // The fields are public, so a caller can pair large dimensions
        // with a short buffer. These reads and writes used to index
        // past the end of `data`.
        let mut p = Pixmap {
            width: 4,
            height: 4,
            data: alloc::vec![7; 3],
        };
        assert_eq!(p.get(2, 0), 7);
        assert_eq!(p.get(3, 3), 0);
        p.set(3, 3, 9);
        assert_eq!(p.data, [7, 7, 7]);
        let c = ColorPixmap {
            width: 4,
            height: 4,
            data: alloc::vec![1; 6],
        };
        assert_eq!(c.get(0, 0), [1; 4]);
        assert_eq!(c.get(1, 0), [0; 4]);
        assert_eq!(c.get(3, 3), [0; 4]);
    }
}
