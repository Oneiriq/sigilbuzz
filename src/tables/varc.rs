//! `VARC`: Variable Composite Glyphs.
//!
//! VARC is an OpenType 1.10 / 2024 extension (originating in the
//! HarfBuzz "boring expansion spec") that lets variable-font composites
//! carry per-axis variation deltas on the component transform matrix
//! and on each component's effective axis-coord vector.
//!
//! Where a classic `glyf` composite glues children with a static
//! 2x2 + translate, a VARC composite carries:
//!
//! * a flag word per component selecting which transform fields are
//!   present (translation, rotation, scale, skew, transformation
//!   center);
//! * an optional axis-indices index plus a `TupleValues` block of
//!   user-coord values for the child's nested coord vector;
//! * optional `MultiVarIdx` references that pull deltas from a
//!   [`MultiVarStore`] for the transform fields and the axis values.
//!
//! sigilbuzz parses the table on demand and exposes the resolved
//! component list at a given normalized coord vector via
//! [`Varc::composite`], or [`Varc::composite_with_font_coords`] for a
//! glyph reached through another composite, whose
//! `RESET_UNSPECIFIED_AXES` components start from the font's coords.
//! The actual outline flattening is the caller's job. See [`crate::Face::glyph_outline_at_coords`], which delegates
//! to VARC when the gid is covered.
//!
//! # Header
//!
//! ```text
//!   u16       majorVersion = 1
//!   u16       minorVersion = 0
//!   Offset32  coverage
//!   Offset32  multiVarStore
//!   Offset32  conditionList            gates components
//!   Offset32  axisIndicesList          CFF2 INDEX of TupleValues
//!   Offset32  glyphRecords             CFF2 INDEX of VarCompositeGlyph
//! ```
//!
//! # Component record
//!
//! Each component record is a flag-driven variable-length blob. See
//! the boring-expansion-spec `VARC.md` for the full table;
//! `Varc::read_component` reads the fields in record order, as
//! HarfBuzz's `VarComponent::decompile_record` does, and
//! `Varc::resolve_component` evaluates them as its `get_path_at` does.
//!
//! # Scope
//!
//! This module only reads VARC. Subsetting lives in the
//! `sigilbuzz-subset` crate.

use alloc::vec::Vec;

use crate::error::{Error, Result};
use crate::tables::layout::Coverage;
use crate::tables::multi_var_store::{decode_tuple_values, read_cff2_index, MultiVarStore};
use crate::tables::parse::{hb_roundf, Reader};

/// A parsed `VARC` table.
#[derive(Debug, Clone)]
pub struct Varc<'a> {
    coverage: Coverage<'a>,
    var_store: Option<MultiVarStore<'a>>,
    /// The ConditionList, from its first byte to the end of the table.
    condition_list: Option<&'a [u8]>,
    /// One axis-indices tuple per CFF2 INDEX entry; outer index of the
    /// component's `axisIndicesIndex` selects one of these.
    axis_indices_lists: Vec<Vec<u32>>,
    /// One byte slice per glyph record. Indexed by the position of
    /// `gid` inside the coverage table (i.e. the same `index_of`
    /// returns).
    glyph_records: Vec<&'a [u8]>,
}

/// One resolved component of a VARC composite, evaluated at a given
/// coord vector. Returned by [`Varc::composite`].
#[derive(Debug, Clone)]
pub struct VarcComponent {
    /// Glyph id of the child outline.
    pub gid: u16,
    /// 2x2 + translate affine, in row-major
    /// `[xx, xy, yx, yy, tx, ty]` order. Apply as
    /// `(x', y') = (xx*x + xy*y + tx, yx*x + yy*y + ty)`.
    pub transform: [f32; 6],
    /// Effective normalized axis coords for the child outline, in
    /// `fvar` axis order: the parent's coord vector (the font's when
    /// the component sets `RESET_UNSPECIFIED_AXES`) with any HAVE_AXES
    /// values written over the listed axes. The vector grows to cover
    /// the highest listed axis when the parent's is shorter. Each
    /// written value is the component's axis value plus its deltas,
    /// rounded to F2DOT14 (a multiple of 1/16384, halves up) as
    /// HarfBuzz stores coords.
    pub coords: Vec<f32>,
}

/// All components of a VARC composite at a specific coord vector.
#[derive(Debug, Clone, Default)]
pub struct VarcComposite {
    /// In source order: components paint back-to-front.
    pub components: Vec<VarcComponent>,
}

// Variable-component flag bits (per boring-expansion-spec).
const VC_RESET_UNSPECIFIED_AXES: u32 = 1 << 0;
const VC_HAVE_AXES: u32 = 1 << 1;
const VC_AXIS_VALUES_HAVE_VARIATION: u32 = 1 << 2;
const VC_TRANSFORM_HAS_VARIATION: u32 = 1 << 3;
const VC_HAVE_TRANSLATE_X: u32 = 1 << 4;
const VC_HAVE_TRANSLATE_Y: u32 = 1 << 5;
const VC_HAVE_ROTATION: u32 = 1 << 6;
const VC_HAVE_CONDITION: u32 = 1 << 7;
const VC_HAVE_SCALE_X: u32 = 1 << 8;
const VC_HAVE_SCALE_Y: u32 = 1 << 9;
const VC_HAVE_TCENTER_X: u32 = 1 << 10;
const VC_HAVE_TCENTER_Y: u32 = 1 << 11;
const VC_GID_IS_24BIT: u32 = 1 << 12;
const VC_HAVE_SKEW_X: u32 = 1 << 13;
const VC_HAVE_SKEW_Y: u32 = 1 << 14;
const VC_RESERVED_MASK: u32 = !((1u32 << 15) - 1);

/// `VarIdx` meaning "no variation".
const NO_VARIATION: u32 = 0xFFFF_FFFF;

/// HarfBuzz's `HB_VAR_COMPOSITE_MAX_AXES`: a component coord vector
/// holds at most this many axes, and an axis index past them is
/// ignored.
const MAX_COMPONENT_AXES: usize = 4096;

/// Condition tables one `composite_with_font_coords` call evaluates at
/// most. And and Or tables can share children, so a small table can
/// name a huge tree; past the budget every condition fails.
const MAX_CONDITION_TABLES: u32 = 1 << 16;

/// HarfBuzz's `HB_MAX_NESTING_LEVEL`: its sanitizer drops a condition
/// nested deeper, which then does not hold.
const MAX_CONDITION_DEPTH: usize = 64;

/// Upper bound on the coordinate values one composite carries across
/// all its components. Each component copies a coord vector, widened
/// to the highest axis index it lists, so a few record bytes can ask
/// for up to 65536 values. The cap keeps a long record of such
/// components from turning into a huge allocation.
const MAX_COMPOSITE_COORDS: usize = 1 << 20;

impl<'a> Varc<'a> {
    /// Parses a `VARC` table.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let major = r.read_u16()?;
        let _minor = r.read_u16()?;
        if major != 1 {
            return Err(Error::Malformed {
                offset: 0,
                context: "unsupported VARC majorVersion",
            });
        }
        let coverage_off = r.read_u32()? as usize;
        let var_store_off = r.read_u32()? as usize;
        let condition_list_off = r.read_u32()? as usize;
        let axis_indices_off = r.read_u32()? as usize;
        let glyph_records_off = r.read_u32()? as usize;

        let coverage = {
            let cov_bytes = data.get(coverage_off..).ok_or(Error::Truncated {
                offset: coverage_off,
                context: "VARC coverage offset past end",
            })?;
            Coverage::parse(cov_bytes)?
        };

        let var_store = if var_store_off == 0 {
            None
        } else {
            let bytes = data.get(var_store_off..).ok_or(Error::Truncated {
                offset: var_store_off,
                context: "VARC varStore offset past end",
            })?;
            Some(MultiVarStore::parse(bytes)?)
        };

        // axisIndicesList is a CFF2 INDEX of TupleValues blocks, one
        // absolute axis index per value.
        let axis_indices_lists = if axis_indices_off == 0 {
            Vec::new()
        } else {
            let mut sr = Reader::at(data, axis_indices_off)?;
            let entries = read_cff2_index(&mut sr)?;
            let mut out = Vec::with_capacity(entries.len());
            for entry in entries {
                out.push(decode_axis_indices(entry));
            }
            out
        };

        // A ConditionList that is absent or starts past the end holds
        // no conditions, so every condition reads as false, as with
        // HarfBuzz's Null table.
        let condition_list = match condition_list_off {
            0 => None,
            off => data.get(off..),
        };

        let glyph_records = if glyph_records_off == 0 {
            Vec::new()
        } else {
            let mut sr = Reader::at(data, glyph_records_off)?;
            read_cff2_index(&mut sr)?
        };

        Ok(Self {
            coverage,
            var_store,
            condition_list,
            axis_indices_lists,
            glyph_records,
        })
    }

    /// Returns true when `gid` has a VARC record and should be
    /// resolved through this table rather than `glyf`.
    #[must_use]
    pub fn covers(&self, gid: u16) -> bool {
        self.coverage.contains(gid)
    }

    /// Number of glyph records in the table. Useful for diagnostics
    /// and round-trip tests.
    #[must_use]
    pub fn glyph_record_count(&self) -> usize {
        self.glyph_records.len()
    }

    /// Resolves the component list for `gid` at the given normalized
    /// axis coords. Returns `None` for uncovered gids.
    ///
    /// `coords` are both the glyph's coords and the font's, which is
    /// right for a glyph drawn on its own. For a glyph reached through
    /// another VARC composite, see [`Varc::composite_with_font_coords`].
    #[must_use]
    pub fn composite(&self, gid: u16, coords: &[f32]) -> Option<VarcComposite> {
        self.composite_with_font_coords(gid, coords, coords)
    }

    /// Resolves the component list for `gid`, whose own coords are
    /// `coords`, in a font set to `font_coords`. Returns `None` for
    /// uncovered gids. The two differ when `gid` is a component of
    /// another VARC composite: `coords` are then that component's
    /// coords.
    ///
    /// Components are read as HarfBuzz's `decompile_record` reads them
    /// and evaluated as its `VarComponent::get_path_at` does:
    ///
    /// - A component with a condition is left out unless the condition
    ///   (from the table's ConditionList, evaluated at `coords`) holds.
    ///   An index past the list, or a condition that is malformed,
    ///   nested more than 64 deep, or of an unknown format, does not
    ///   hold. One call evaluates at most 65536 condition tables; past
    ///   that every condition fails.
    /// - The child's coords start from `coords`, or from `font_coords`
    ///   when the component sets `RESET_UNSPECIFIED_AXES` (and, as in
    ///   HarfBuzz, when `coords` hold more than 4096 axes); the axes the
    ///   component lists take its values.
    ///
    /// A component whose glyph id does not fit 16 bits names no glyph
    /// and is left out. A malformed component ends the list early. So
    /// does a component that would push the total length of all
    /// component coord vectors past 2^20 values.
    #[must_use]
    pub fn composite_with_font_coords(
        &self,
        gid: u16,
        coords: &[f32],
        font_coords: &[f32],
    ) -> Option<VarcComposite> {
        let idx = self.coverage.index_of(gid)? as usize;
        let raw = *self.glyph_records.get(idx)?;
        let mut composite = VarcComposite::default();
        let mut r = Reader::new(raw);
        let mut total_coords = 0usize;
        let mut condition_budget = MAX_CONDITION_TABLES;
        while !r.is_empty() {
            // A VarComponent stops when bytes run out. Reaching the
            // end mid-record means the font is malformed; we skip the
            // rest rather than error so a single bad glyph doesn't
            // tank the rest of the document.
            let Ok(record) = self.read_component(&mut r) else {
                break;
            };
            if let Some(index) = record.condition_index {
                if !self.condition_holds(index, coords, &mut condition_budget) {
                    continue;
                }
            }
            let Some(c) = self.resolve_component(&record, coords, font_coords) else {
                continue;
            };
            total_coords = total_coords.saturating_add(c.coords.len());
            if total_coords > MAX_COMPOSITE_COORDS {
                break;
            }
            composite.components.push(c);
        }
        Some(composite)
    }

    /// Decodes the component record at the reader's position, in the
    /// order the spec and HarfBuzz read it: flags, glyph id, condition
    /// index, axis indices index and axis values, the two variation
    /// indices, the transform fields, then one discarded uint32var per
    /// reserved flag bit.
    fn read_component(&self, r: &mut Reader<'_>) -> Result<ComponentRecord<'_>> {
        let flags = read_uint32var(r)?;
        let gid = if flags & VC_GID_IS_24BIT != 0 {
            let bytes = r.read_bytes(3)?;
            (u32::from(bytes[0]) << 16) | (u32::from(bytes[1]) << 8) | u32::from(bytes[2])
        } else {
            u32::from(r.read_u16()?)
        };
        let condition_index = if flags & VC_HAVE_CONDITION != 0 {
            Some(read_uint32var(r)?)
        } else {
            None
        };
        let (axis_indices, axis_values) = if flags & VC_HAVE_AXES != 0 {
            // An index past the list names an empty tuple, as in
            // HarfBuzz, so no axis values follow.
            let index = read_uint32var(r)? as usize;
            let indices = self
                .axis_indices_lists
                .get(index)
                .map_or(&[][..], Vec::as_slice);
            let values =
                decode_tuple_values_in_reader(r, indices.len()).ok_or(Error::Malformed {
                    offset: r.position(),
                    context: "VARC component axisValues TupleValues truncated",
                })?;
            (indices, values)
        } else {
            (&[][..], Vec::new())
        };
        let axis_values_var_index = if flags & VC_AXIS_VALUES_HAVE_VARIATION != 0 {
            Some(read_uint32var(r)?)
        } else {
            None
        };
        let transform_var_index = if flags & VC_TRANSFORM_HAS_VARIATION != 0 {
            Some(read_uint32var(r)?)
        } else {
            None
        };
        let mut fields = Vec::new();
        for (flag, field) in TRANSFORM_FIELDS {
            if flags & flag != 0 {
                fields.push((field, r.read_i16()?));
            }
        }
        for _ in 0..(flags & VC_RESERVED_MASK).count_ones() {
            read_uint32var(r)?;
        }
        Ok(ComponentRecord {
            flags,
            gid,
            condition_index,
            axis_indices,
            axis_values,
            axis_values_var_index,
            transform_var_index,
            fields,
        })
    }

    /// Evaluates `record` at `coords`, the coords of the glyph the
    /// component belongs to, in a font set to `font_coords`. `None` when
    /// the component draws nothing.
    fn resolve_component(
        &self,
        record: &ComponentRecord<'_>,
        coords: &[f32],
        font_coords: &[f32],
    ) -> Option<VarcComponent> {
        let gid = u16::try_from(record.gid).ok()?;

        // Axis values and their deltas, in F2DOT14 units.
        let mut axis_values: Vec<f32> = record.axis_values.iter().map(|&v| v as f32).collect();
        if let Some(index) = record.axis_values_var_index {
            self.add_deltas(index, coords, &mut axis_values);
        }

        // The child starts from the parent's coord vector, or with
        // RESET_UNSPECIFIED_AXES from the font's, and the listed axes
        // take the component's values. HarfBuzz keeps coords as whole
        // F2DOT14 values, so each value plus its deltas rounds to one,
        // halves up. It holds at most `MAX_COMPONENT_AXES` coords,
        // ignores an axis index past them, and starts from the font's
        // coords when the parent's hold more.
        let reset = record.flags & VC_RESET_UNSPECIFIED_AXES != 0;
        let base = if reset || coords.len() > MAX_COMPONENT_AXES {
            font_coords
        } else {
            coords
        };
        let mut child_coords = base.to_vec();
        for (&axis, &value) in record.axis_indices.iter().zip(&axis_values) {
            let axis = axis as usize;
            if axis >= MAX_COMPONENT_AXES {
                continue;
            }
            if child_coords.len() <= axis {
                child_coords.resize(axis + 1, 0.0);
            }
            child_coords[axis] = hb_roundf(value) / 16384.0;
        }

        // Transform fields in their stored units, plus their deltas,
        // then divided down: F4.12 for angles, F6.10 for scales.
        let mut values: Vec<f32> = record.fields.iter().map(|&(_, v)| f32::from(v)).collect();
        if let Some(index) = record.transform_var_index {
            self.add_deltas(index, coords, &mut values);
        }
        let mut t = Decomposed::default();
        for (&(field, _), &v) in record.fields.iter().zip(&values) {
            match field {
                TransformField::TranslateX => t.tx = v,
                TransformField::TranslateY => t.ty = v,
                TransformField::Rotation => t.rotation = v / 4096.0,
                TransformField::ScaleX => t.sx = v / 1024.0,
                TransformField::ScaleY => t.sy = v / 1024.0,
                TransformField::SkewX => t.skew_x = v / 4096.0,
                TransformField::SkewY => t.skew_y = v / 4096.0,
                TransformField::TCenterX => t.tcx = v,
                TransformField::TCenterY => t.tcy = v,
            }
        }
        // ScaleY defaults to ScaleX, not to 1.
        if record.flags & VC_HAVE_SCALE_Y == 0 {
            t.sy = t.sx;
        }

        Some(VarcComponent {
            gid,
            transform: t.to_affine(),
            coords: child_coords,
        })
    }

    /// Whether condition `index` of the ConditionList holds at `coords`.
    ///
    /// ```text
    ///   ConditionList: u32 count, Offset32 conditions[count]
    ///                  (from the start of the list)
    /// ```
    ///
    /// A missing list, an index past it, and a null or out-of-range
    /// offset all name HarfBuzz's Null condition, which does not hold.
    fn condition_holds(&self, index: u32, coords: &[f32], budget: &mut u32) -> bool {
        let Some(list) = self.condition_list else {
            return false;
        };
        let Some(count) = be_u32(list, 0) else {
            return false;
        };
        if index >= count {
            return false;
        }
        let slot = (index as usize)
            .checked_mul(4)
            .and_then(|at| at.checked_add(4));
        let Some(offset) = slot.and_then(|at| be_u32(list, at)) else {
            return false;
        };
        match list.get(offset as usize..) {
            Some(condition) if offset != 0 => self.evaluate_condition(condition, coords, 0, budget),
            _ => false,
        }
    }

    /// Evaluates the condition table at the start of `data`, as
    /// HarfBuzz's `Condition::evaluate` does:
    ///
    /// ```text
    ///   1 AxisRange: u16 format, u16 axisIndex, F2DOT14 min, F2DOT14 max
    ///   2 Value:     u16 format, i16 defaultValue, u32 varIndex
    ///   3 And:       u16 format, u8 count, Offset24 conditions[count]
    ///   4 Or:        u16 format, u8 count, Offset24 conditions[count]
    ///   5 Negate:    u16 format, Offset24 condition
    /// ```
    ///
    /// Offsets are from the start of the condition that holds them. A
    /// null offset names the Null condition, which does not hold, so
    /// its negation does. A table cut short, an unknown format, a table
    /// nested deeper than `MAX_CONDITION_DEPTH` (HarfBuzz's sanitizer
    /// drops those), and every table once `budget` runs out do not
    /// hold either.
    fn evaluate_condition(
        &self,
        data: &[u8],
        coords: &[f32],
        depth: usize,
        budget: &mut u32,
    ) -> bool {
        if depth >= MAX_CONDITION_DEPTH || *budget == 0 {
            return false;
        }
        *budget -= 1;
        let child = |offset: usize, budget: &mut u32| -> bool {
            let Some(at) = be_u24(data, offset) else {
                return false;
            };
            match data.get(at..) {
                Some(child) if at != 0 => self.evaluate_condition(child, coords, depth + 1, budget),
                _ => false,
            }
        };
        match be_u16(data, 0) {
            Some(1) => {
                let (Some(axis), Some(min), Some(max)) =
                    (be_u16(data, 2), be_u16(data, 4), be_u16(data, 6))
                else {
                    return false;
                };
                // HarfBuzz compares whole F2DOT14 values; an axis the
                // coords do not reach is at its default.
                let coord = coords
                    .get(usize::from(axis))
                    .map_or(0.0, |&c| hb_roundf(c * 16384.0));
                f32::from(min as i16) <= coord && coord <= f32::from(max as i16)
            }
            Some(2) => {
                let (Some(default), Some(index)) = (be_u16(data, 2), be_u32(data, 4)) else {
                    return false;
                };
                let mut value = [f32::from(default as i16)];
                self.add_deltas(index, coords, &mut value);
                value[0] > 0.0
            }
            Some(format @ (3 | 4)) => {
                let Some(&count) = data.get(2) else {
                    return false;
                };
                // The whole offset array has to be there, as for
                // HarfBuzz's sanitizer.
                if data.len() < 3 + 3 * usize::from(count) {
                    return false;
                }
                let mut offsets = (0..usize::from(count)).map(|i| 3 + 3 * i);
                if format == 3 {
                    offsets.all(|at| child(at, budget))
                } else {
                    offsets.any(|at| child(at, budget))
                }
            }
            Some(5) => data.len() >= 5 && !child(2, budget),
            _ => false,
        }
    }

    /// Adds the deltas of the variation index `index` at `coords` to
    /// `values`. Nothing is added at the default instance (empty
    /// coords), for `NO_VARIATION`, or without a store, as in HarfBuzz.
    fn add_deltas(&self, index: u32, coords: &[f32], values: &mut [f32]) {
        if coords.is_empty() || index == NO_VARIATION {
            return;
        }
        if let Some(store) = &self.var_store {
            store.add_deltas((index >> 16) as u16, index & 0xFFFF, coords, values);
        }
    }
}

/// One component record as stored, before it is evaluated at any
/// coords.
struct ComponentRecord<'t> {
    flags: u32,
    /// 16- or 24-bit glyph id.
    gid: u32,
    condition_index: Option<u32>,
    /// The axes the component sets, from the axis indices list.
    axis_indices: &'t [u32],
    /// One value per axis index, in F2DOT14 units.
    axis_values: Vec<i32>,
    axis_values_var_index: Option<u32>,
    transform_var_index: Option<u32>,
    /// The transform fields present, in spec order, as stored.
    fields: Vec<(TransformField, i16)>,
}

/// A component transform in its decomposed form, angles in half-turns.
struct Decomposed {
    tx: f32,
    ty: f32,
    rotation: f32,
    sx: f32,
    sy: f32,
    skew_x: f32,
    skew_y: f32,
    tcx: f32,
    tcy: f32,
}

impl Default for Decomposed {
    fn default() -> Self {
        Self {
            tx: 0.0,
            ty: 0.0,
            rotation: 0.0,
            sx: 1.0,
            sy: 1.0,
            skew_x: 0.0,
            skew_y: 0.0,
            tcx: 0.0,
            tcy: 0.0,
        }
    }
}

impl Decomposed {
    /// The affine the boring-expansion spec composes:
    ///
    /// ```text
    ///   T(tx + tcx, ty + tcy) * R(rotation * π) * S(sx, sy) *
    ///   Skew(-skewX * π, skewY * π) * T(-tcx, -tcy)
    /// ```
    fn to_affine(&self) -> [f32; 6] {
        compose_affine(
            self.tx,
            self.ty,
            self.rotation,
            self.sx,
            self.sy,
            self.skew_x,
            self.skew_y,
            self.tcx,
            self.tcy,
        )
    }
}

/// The transform fields in record order, with their flags.
const TRANSFORM_FIELDS: [(u32, TransformField); 9] = [
    (VC_HAVE_TRANSLATE_X, TransformField::TranslateX),
    (VC_HAVE_TRANSLATE_Y, TransformField::TranslateY),
    (VC_HAVE_ROTATION, TransformField::Rotation),
    (VC_HAVE_SCALE_X, TransformField::ScaleX),
    (VC_HAVE_SCALE_Y, TransformField::ScaleY),
    (VC_HAVE_SKEW_X, TransformField::SkewX),
    (VC_HAVE_SKEW_Y, TransformField::SkewY),
    (VC_HAVE_TCENTER_X, TransformField::TCenterX),
    (VC_HAVE_TCENTER_Y, TransformField::TCenterY),
];

/// One transform field, in spec order. Used to map variation deltas
/// back onto the right slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransformField {
    TranslateX,
    TranslateY,
    Rotation,
    ScaleX,
    ScaleY,
    SkewX,
    SkewY,
    TCenterX,
    TCenterY,
}

/// Big-endian `u16` at byte `at` of `data`.
fn be_u16(data: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(*data.get(at..)?.first_chunk::<2>()?))
}

/// Big-endian 24-bit offset at byte `at` of `data`.
fn be_u24(data: &[u8], at: usize) -> Option<usize> {
    let [a, b, c] = *data.get(at..)?.first_chunk::<3>()?;
    Some((usize::from(a) << 16) | (usize::from(b) << 8) | usize::from(c))
}

/// Big-endian `u32` at byte `at` of `data`.
fn be_u32(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(*data.get(at..)?.first_chunk::<4>()?))
}

/// Variable-length integer encoding used by VARC. 1-5 bytes
/// big-endian, sign-extension-free.
fn read_uint32var(r: &mut Reader<'_>) -> Result<u32> {
    let b0 = r.read_u8()?;
    Ok(match b0 {
        0x00..=0x7F => u32::from(b0),
        0x80..=0xBF => {
            let b1 = r.read_u8()?;
            ((u32::from(b0) - 0x80) << 8) | u32::from(b1)
        }
        0xC0..=0xDF => {
            let b1 = r.read_u8()?;
            let b2 = r.read_u8()?;
            ((u32::from(b0) - 0xC0) << 16) | (u32::from(b1) << 8) | u32::from(b2)
        }
        0xE0..=0xEF => {
            let b1 = r.read_u8()?;
            let b2 = r.read_u8()?;
            let b3 = r.read_u8()?;
            ((u32::from(b0) - 0xE0) << 24)
                | (u32::from(b1) << 16)
                | (u32::from(b2) << 8)
                | u32::from(b3)
        }
        0xF0..=0xFF => {
            // 5-byte form: first byte is the marker, payload is the
            // following u32 big-endian. (Per the spec the marker is
            // exactly 0xF0; consumers in the wild emit only that
            // byte, but we accept any 0xFn for robustness.)
            r.read_u32()?
        }
    })
}

/// Decodes `count` TupleValues from the reader, advancing it past
/// them. Returns `None` on truncation or when a run goes past `count`,
/// where HarfBuzz's `TupleValues::decompile` fails.
fn decode_tuple_values_in_reader(r: &mut Reader<'_>, count: usize) -> Option<Vec<i32>> {
    let buf = r.peek_bytes(r.remaining()).ok()?;
    let (values, used) = decode_tuple_values(buf, count)?;
    r.skip(used).ok()?;
    Some(values)
}

/// Decodes one axis indices tuple the way HarfBuzz's
/// `TupleValues::iter_t` walks it, malformed streams included: a run
/// that does not fit the bytes left yields one zero and the walk goes
/// on from the next byte. Each value is read as unsigned, as HarfBuzz
/// stores the indices, so a negative one names no axis.
fn decode_axis_indices(data: &[u8]) -> Vec<u32> {
    struct Walk<'d> {
        data: &'d [u8],
        p: usize,
        run: usize,
        width: usize,
        value: i32,
    }
    impl Walk<'_> {
        fn ensure_run(&mut self) -> bool {
            if self.run > 0 {
                return true;
            }
            let Some(&control) = self.data.get(self.p) else {
                self.value = 0;
                return false;
            };
            self.p += 1;
            self.run = usize::from(control & 0x3F) + 1;
            self.width = match control & 0xC0 {
                0x80 => 0,
                0x00 => 1,
                0x40 => 2,
                _ => 4,
            };
            if self.data.len() - self.p < self.run * self.width {
                self.run = 0;
                self.value = 0;
                return false;
            }
            true
        }
        fn read_value(&mut self) {
            let b = &self.data[self.p..self.p + self.width];
            self.value = match *b {
                [] => 0,
                [x] => i32::from(x as i8),
                [x, y] => i32::from(i16::from_be_bytes([x, y])),
                [a, b2, c, d] => i32::from_be_bytes([a, b2, c, d]),
                _ => 0,
            };
            self.p += self.width;
        }
    }
    let mut walk = Walk {
        data,
        p: 0,
        run: 0,
        width: 0,
        value: 0,
    };
    if walk.ensure_run() {
        walk.read_value();
    }
    let mut out = Vec::new();
    while walk.run > 0 || walk.p < data.len() {
        out.push(walk.value as u32);
        walk.run = walk.run.saturating_sub(1);
        if walk.ensure_run() {
            walk.read_value();
        }
    }
    out
}

/// Builds the affine matrix from VARC's transform fields. Matches the
/// boring-expansion-spec composition order:
///
/// ```text
///   T(tx + tcx, ty + tcy) * R(rotation * π) *
///   S(sx, sy) * Skew(-skewX * π, skewY * π) * T(-tcx, -tcy)
/// ```
///
/// Returned in `[xx, xy, yx, yy, tx_eff, ty_eff]` row-major form.
#[allow(clippy::too_many_arguments)]
fn compose_affine(
    tx: f32,
    ty: f32,
    rotation: f32,
    sx: f32,
    sy: f32,
    skew_x: f32,
    skew_y: f32,
    tcx: f32,
    tcy: f32,
) -> [f32; 6] {
    // core has no sin or cos without std, so `sincos_pi` evaluates a
    // Taylor polynomial instead of calling libm.
    let (cos_r, sin_r) = sincos_pi(rotation);
    let (cos_skx, sin_skx) = sincos_pi(-skew_x);
    let (cos_sky, sin_sky) = sincos_pi(skew_y);

    // Skew matrix:
    //   [ 1, tan(skewY*π) ]
    //   [ tan(-skewX*π), 1 ]
    // Implemented as cos/sin to avoid blowing up at ±π/2.
    // Practically, skews in fonts stay well clear of ±π/2 so the
    // tan form is fine; we compute via sin/cos for stability.
    let tan_skx = sin_skx / cos_skx;
    let tan_sky = sin_sky / cos_sky;

    // Build M = R * S * Skew. S applied to skew first:
    //   [ sx, 0 ]   [ 1, tan_skx ]   [ sx, sx*tan_skx ]
    //   [ 0, sy ] * [ tan_sky, 1 ] = [ sy*tan_sky, sy ]
    let m_xx = sx;
    let m_xy = sx * tan_skx;
    let m_yx = sy * tan_sky;
    let m_yy = sy;

    // Rotation * M:
    //   [ cos, -sin ]   [ m_xx, m_xy ]
    //   [ sin,  cos ] * [ m_yx, m_yy ]
    let r_xx = cos_r * m_xx - sin_r * m_yx;
    let r_xy = cos_r * m_xy - sin_r * m_yy;
    let r_yx = sin_r * m_xx + cos_r * m_yx;
    let r_yy = sin_r * m_xy + cos_r * m_yy;

    // Effective translation:
    //   p' = R*M*(p - tcenter) + (tcenter + translate)
    //   tx_eff = -(r_xx*tcx + r_xy*tcy) + tcx + tx
    //   ty_eff = -(r_yx*tcx + r_yy*tcy) + tcy + ty
    let tx_eff = -(r_xx * tcx + r_xy * tcy) + tcx + tx;
    let ty_eff = -(r_yx * tcx + r_yy * tcy) + tcy + ty;

    [r_xx, r_xy, r_yx, r_yy, tx_eff, ty_eff]
}

/// `sin(x*π)` and `cos(x*π)`. Tight enough for glyph composites.
/// VARC's F4.12 fields stay within `[-8, 8)`, but variation deltas can
/// push an angle anywhere, including infinity. Implemented via a
/// Taylor series for `no_std`-friendliness.
fn sincos_pi(x: f32) -> (f32, f32) {
    // Reduce to [-1, 1] (i.e. [-π, π]).
    let mut t = x;
    // For large or infinite angles the loops below would run for a
    // long time or forever, so take the remainder first. `%` is exact
    // and so is each loop step below 2^25, so both paths give the same
    // angle wherever the loops finish. Clearing the sign of a zero
    // remainder matches what the loops produce.
    // A range check instead of `abs`, which core lacks before Rust 1.85.
    if !(-16.0..=16.0).contains(&t) {
        t %= 2.0;
        if t == 0.0 {
            t = 0.0;
        }
    }
    while t > 1.0 {
        t -= 2.0;
    }
    while t < -1.0 {
        t += 2.0;
    }
    // y = t * π
    let y = t * core::f32::consts::PI;
    // 9-term Taylor expansion good to ~1e-6 over [-π, π].
    let y2 = y * y;
    let sin = y * (1.0 - y2 / 6.0 + y2 * y2 / 120.0 - y2 * y2 * y2 / 5040.0);
    let cos = 1.0 - y2 / 2.0 + y2 * y2 / 24.0 - y2 * y2 * y2 / 720.0 + y2 * y2 * y2 * y2 / 40320.0;
    (cos, sin)
}

#[cfg(test)]
mod tests;
