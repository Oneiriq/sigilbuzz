//! `COLR`: Layered color glyph table.
//!
//! Two generations coexist in a single `COLR` blob:
//!
//! - **v0** (2013 era): a base glyph maps to a flat list of
//!   *(glyph_id, palette_index)* layer records. The renderer draws
//!   each layer in order with the palette color.
//! - **v1** (2020 era): a base glyph maps to a tree of `Paint`
//!   operations (gradients, transforms, composites, nested clip
//!   glyphs), roughly matching SVG's native-paint model. Variable-font
//!   aware siblings (`PaintVar*`) carry `ItemVariationStore` deltas.
//!
//! sigilbuzz parses both. Evaluation (drawing pixels) is a renderer
//! concern; this module stops at structural traversal: enumerate the
//! v0 layers for a glyph, walk the v1 paint DAG for a glyph. The walk
//! returns byte-slice-backed views so callers do not pay to clone the
//! tree.
//!
//! The v1 paint enum is large. Every variant borrows `&'a [u8]` into
//! the original font data; sub-paints are represented by `Offset24`
//! (3-byte offsets relative to the start of the `LayerList` /
//! `BaseGlyphList` block) which consumers resolve through
//! [`Colr::paint_at`]. This keeps traversal zero-copy and avoids
//! constructing a recursive owned tree eagerly.

use crate::error::{Error, Result};
use crate::tables::parse::Reader;

mod clip;
mod paint;

pub use clip::{ClipBox, ClipList};
pub use paint::{
    ColorLine, ColorStop, ColrPaint, CompositeMode, Extend, F2Dot14, Fixed, Fword, VarIndexBase,
};

/// Offset (absolute, within the COLR table) to a v1 `Paint` node.
/// Stored as a u24 in the font, widened to u32 here.
pub type PaintOffset = u32;

// =========================================================================
// Header
// =========================================================================

/// Parsed `COLR` table.
///
/// Holds enough offsets to service both the v0 layer list and, when
/// present, the v1 paint-tree traversal API. Cloning is cheap:
/// just a handful of slice references.
#[derive(Debug, Clone, Copy)]
pub struct Colr<'a> {
    data: &'a [u8],
    /// Version word. `0` for the layer-only table, `1` for the
    /// paint-tree extension.
    version: u16,

    /// v0: base-glyph-record array slice. Each record is 6 bytes.
    base_glyph_records: &'a [u8],
    /// v0: layer-record array slice. Each record is 4 bytes.
    layer_records: &'a [u8],
    num_layer_records: u16,

    // --- v1-only offsets (all zero / None when version == 0) ---
    /// Absolute offset to the v1 BaseGlyphList block, or 0 if absent.
    base_glyph_list_off: u32,
    /// Absolute offset to the v1 LayerList block, or 0 if absent.
    layer_list_off: u32,
    /// Absolute offset to the v1 ClipList block, or 0 if absent.
    clip_list_off: u32,
    /// Absolute offset to the v1 DeltaSetIndexMap, or 0 if absent.
    var_index_map_off: u32,
    /// Absolute offset to the v1 ItemVariationStore, or 0 if absent.
    var_store_off: u32,
}

impl<'a> Colr<'a> {
    /// Parses a `COLR` table header.
    ///
    /// A version 1 header is 34 bytes: the 14-byte version 0 header
    /// followed by five Offset32 fields (BaseGlyphList, LayerList,
    /// ClipList, DeltaSetIndexMap, ItemVariationStore). A version 1
    /// table cut off before the end of those fields is rejected with
    /// [`Error::Truncated`], as HarfBuzz's sanitizer drops such a table
    /// entirely.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let version = r.read_u16()?;
        if version != 0 && version != 1 {
            return Err(Error::Malformed {
                offset: 0,
                context: "unsupported COLR version",
            });
        }
        let num_base_glyph_records = r.read_u16()?;
        let base_glyph_records_off = r.read_u32()? as usize;
        let layer_records_off = r.read_u32()? as usize;
        let num_layer_records = r.read_u16()?;

        let base_bytes = num_base_glyph_records as usize * 6;
        let layer_bytes = num_layer_records as usize * 4;

        let base_end = base_glyph_records_off
            .checked_add(base_bytes)
            .ok_or(Error::Malformed {
                offset: base_glyph_records_off,
                context: "COLR base glyph records overflow",
            })?;
        let layer_end = layer_records_off
            .checked_add(layer_bytes)
            .ok_or(Error::Malformed {
                offset: layer_records_off,
                context: "COLR layer records overflow",
            })?;
        if base_end > data.len() || layer_end > data.len() {
            return Err(Error::Truncated {
                offset: data.len(),
                context: "COLR v0 record arrays truncated",
            });
        }

        let base_glyph_records = if num_base_glyph_records == 0 {
            &data[..0]
        } else {
            &data[base_glyph_records_off..base_end]
        };
        let layer_records = if num_layer_records == 0 {
            &data[..0]
        } else {
            &data[layer_records_off..layer_end]
        };

        let mut base_glyph_list_off = 0u32;
        let mut layer_list_off = 0u32;
        let mut clip_list_off = 0u32;
        let mut var_index_map_off = 0u32;
        let mut var_store_off = 0u32;
        if version >= 1 {
            // v1 appends five Offset32 fields to the header.
            base_glyph_list_off = r.read_u32()?;
            layer_list_off = r.read_u32()?;
            clip_list_off = r.read_u32()?;
            var_index_map_off = r.read_u32()?;
            var_store_off = r.read_u32()?;
        }

        Ok(Self {
            data,
            version,
            base_glyph_records,
            layer_records,
            num_layer_records,
            base_glyph_list_off,
            layer_list_off,
            clip_list_off,
            var_index_map_off,
            var_store_off,
        })
    }

    /// Table version word. `0` for layer-only, `1` for paint-tree.
    #[must_use]
    pub const fn version(&self) -> u16 {
        self.version
    }

    /// Number of v0 base-glyph records.
    #[must_use]
    pub fn num_base_glyph_records(&self) -> u16 {
        (self.base_glyph_records.len() / 6) as u16
    }

    /// Number of v0 layer records in the pool.
    #[must_use]
    pub const fn num_layer_records(&self) -> u16 {
        self.num_layer_records
    }

    /// True when the font carries the v1 paint-tree extension.
    #[must_use]
    pub const fn has_v1(&self) -> bool {
        self.version >= 1 && self.base_glyph_list_off != 0
    }

    /// Returns the absolute offset of the v1 ItemVariationStore, or
    /// `None` when the font has no variation deltas on its paint
    /// tree.
    #[must_use]
    pub fn var_store_offset(&self) -> Option<u32> {
        if self.var_store_off == 0 {
            None
        } else {
            Some(self.var_store_off)
        }
    }

    /// Returns the absolute offset of the v1 DeltaSetIndexMap, or
    /// `None` when the table has none. When present, a paint's
    /// `varIndexBase + i` is mapped through it before the variation
    /// store lookup; when absent, the index itself splits into the
    /// store's outer (high 16 bits) and inner (low 16 bits) indices.
    #[must_use]
    pub fn var_index_map_offset(&self) -> Option<u32> {
        (self.var_index_map_off != 0).then_some(self.var_index_map_off)
    }

    /// Returns the absolute offset of the v1 ClipList, or `None` when
    /// the table has none.
    #[must_use]
    pub fn clip_list_offset(&self) -> Option<u32> {
        (self.clip_list_off != 0).then_some(self.clip_list_off)
    }

    /// Parses the v1 ClipList. Returns `None` when the table has no
    /// ClipList or its header does not fit in the table, which is how
    /// HarfBuzz treats a ClipList offset that fails to sanitize.
    #[must_use]
    pub fn clip_list(&self) -> Option<ClipList<'a>> {
        let off = self.clip_list_offset()?;
        ClipList::parse(self.data, off as usize).ok()
    }

    /// The clip box of `glyph_id`, from the ClipList, or `None` when the
    /// table has no ClipList or the glyph has no box.
    #[must_use]
    pub fn clip_box(&self, glyph_id: u16) -> Option<ClipBox> {
        self.clip_list()?.get(glyph_id)
    }

    /// Looks up v0 layers for `glyph_id`. Binary search on the sorted
    /// base-glyph-record array. Returns `None` when the glyph has no
    /// v0 record.
    #[must_use]
    pub fn v0_layers(&self, glyph_id: u16) -> Option<V0Layers<'a>> {
        let n = self.num_base_glyph_records() as usize;
        let mut lo = 0;
        let mut hi = n;
        while lo < hi {
            let mid = (lo + hi) / 2;
            let off = mid * 6;
            let gid = u16::from_be_bytes([
                self.base_glyph_records[off],
                self.base_glyph_records[off + 1],
            ]);
            match gid.cmp(&glyph_id) {
                core::cmp::Ordering::Equal => {
                    let first = u16::from_be_bytes([
                        self.base_glyph_records[off + 2],
                        self.base_glyph_records[off + 3],
                    ]);
                    let count = u16::from_be_bytes([
                        self.base_glyph_records[off + 4],
                        self.base_glyph_records[off + 5],
                    ]);
                    return Some(V0Layers {
                        records: self.layer_records,
                        first,
                        count,
                    });
                }
                core::cmp::Ordering::Less => lo = mid + 1,
                core::cmp::Ordering::Greater => hi = mid,
            }
        }
        None
    }

    /// Returns the v1 paint for `glyph_id` if the font has the v1
    /// extension and the glyph has a BaseGlyphPaintRecord. Callers
    /// walk the returned [`ColrPaint`] with its child-resolution
    /// helpers to traverse the tree.
    #[must_use]
    pub fn paint(&self, glyph_id: u16) -> Option<ColrPaint<'a>> {
        if !self.has_v1() {
            return None;
        }
        let list_start = self.base_glyph_list_off as usize;
        if list_start + 4 > self.data.len() {
            return None;
        }
        let count = u32::from_be_bytes([
            self.data[list_start],
            self.data[list_start + 1],
            self.data[list_start + 2],
            self.data[list_start + 3],
        ]) as usize;
        let recs_start = list_start + 4;
        let recs_end = recs_start.checked_add(count * 6)?;
        if recs_end > self.data.len() {
            return None;
        }
        // BaseGlyphPaintRecord: { u16 glyphID; Offset32 paintOffset }.
        // Binary search on glyphID.
        let mut lo = 0usize;
        let mut hi = count;
        while lo < hi {
            let mid = (lo + hi) / 2;
            let off = recs_start + mid * 6;
            let gid = u16::from_be_bytes([self.data[off], self.data[off + 1]]);
            match gid.cmp(&glyph_id) {
                core::cmp::Ordering::Equal => {
                    let paint_rel = u32::from_be_bytes([
                        self.data[off + 2],
                        self.data[off + 3],
                        self.data[off + 4],
                        self.data[off + 5],
                    ]);
                    // Paint offset is relative to the start of the
                    // BaseGlyphList subtable.
                    let abs = (list_start as u32).checked_add(paint_rel)?;
                    return self.paint_at(abs);
                }
                core::cmp::Ordering::Less => lo = mid + 1,
                core::cmp::Ordering::Greater => hi = mid,
            }
        }
        None
    }

    /// Resolves the `Paint` sitting at the absolute `offset` inside
    /// the COLR table. Returns `None` when the offset is out of range
    /// or the format byte is one sigilbuzz does not yet handle.
    #[must_use]
    pub fn paint_at(&self, offset: u32) -> Option<ColrPaint<'a>> {
        let start = offset as usize;
        if start >= self.data.len() {
            return None;
        }
        ColrPaint::parse(self.data, start).ok()
    }

    /// Resolves a layer in the v1 LayerList by index. Each layer is
    /// itself a Paint; callers iterate over them after reading a
    /// [`ColrPaint::ColrLayers`] header.
    #[must_use]
    pub fn layer_paint(&self, layer_index: u32) -> Option<ColrPaint<'a>> {
        if self.layer_list_off == 0 {
            return None;
        }
        let list_start = self.layer_list_off as usize;
        if list_start + 4 > self.data.len() {
            return None;
        }
        let count = u32::from_be_bytes([
            self.data[list_start],
            self.data[list_start + 1],
            self.data[list_start + 2],
            self.data[list_start + 3],
        ]);
        if layer_index >= count {
            return None;
        }
        let entry = list_start + 4 + layer_index as usize * 4;
        if entry + 4 > self.data.len() {
            return None;
        }
        let paint_rel = u32::from_be_bytes([
            self.data[entry],
            self.data[entry + 1],
            self.data[entry + 2],
            self.data[entry + 3],
        ]);
        let abs = (list_start as u32).checked_add(paint_rel)?;
        self.paint_at(abs)
    }

    /// Returns the full backing slice. Useful for renderers walking
    /// child offsets that were resolved into absolute positions.
    #[must_use]
    pub const fn data(&self) -> &'a [u8] {
        self.data
    }
}

// =========================================================================
// COLR v0: layer records
// =========================================================================

/// Iterator view over the v0 layer records for a single base glyph.
#[derive(Debug, Clone, Copy)]
pub struct V0Layers<'a> {
    records: &'a [u8],
    first: u16,
    count: u16,
}

impl V0Layers<'_> {
    /// Number of layers that make up this glyph.
    #[must_use]
    pub const fn len(&self) -> u16 {
        self.count
    }

    /// True when the glyph has no layers (spec-degenerate).
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Returns the layer at `index`. Layers are drawn in order,
    /// bottom-up: index 0 is behind index 1.
    #[must_use]
    pub fn get(&self, index: u16) -> Option<V0Layer> {
        if index >= self.count {
            return None;
        }
        let rec = self.first as usize + index as usize;
        let off = rec * 4;
        if off + 4 > self.records.len() {
            return None;
        }
        let gid = u16::from_be_bytes([self.records[off], self.records[off + 1]]);
        let palette_index = u16::from_be_bytes([self.records[off + 2], self.records[off + 3]]);
        Some(V0Layer {
            glyph_id: gid,
            palette_index,
        })
    }

    /// Yields every layer in draw order.
    pub fn iter(&self) -> impl Iterator<Item = V0Layer> + '_ {
        (0..self.count).filter_map(move |i| self.get(i))
    }
}

/// One layer in the v0 list: a glyph id and the palette entry it
/// should be painted with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V0Layer {
    /// Child glyph id to draw.
    pub glyph_id: u16,
    /// Index into the active `CPAL` palette. `0xFFFF` means "use the
    /// foreground text color".
    pub palette_index: u16,
}

#[cfg(test)]
mod tests;
