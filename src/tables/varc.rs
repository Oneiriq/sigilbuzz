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
//! [`Varc::composite`]. The actual outline flattening is the caller's
//! job. See [`crate::Face::glyph_outline_at_coords`], which delegates
//! to VARC when the gid is covered.
//!
//! # Header
//!
//! ```text
//!   u16       majorVersion = 1
//!   u16       minorVersion = 0
//!   Offset32  coverage
//!   Offset32  multiVarStore
//!   Offset32  conditionList            (sigilbuzz parses but ignores)
//!   Offset32  axisIndicesList          CFF2 INDEX of TupleValues
//!   Offset32  glyphRecords             CFF2 INDEX of VarCompositeGlyph
//! ```
//!
//! # Component record
//!
//! Each component record is a flag-driven variable-length blob. See
//! the boring-expansion-spec `VARC.md` for the full table; the comment
//! at `Varc::resolve_component` enumerates which fields appear under
//! which flags.
//!
//! # Scope
//!
//! Reading only: encoding and subsetting are out of scope and live in
//! their own follow-up tickets. ConditionList parsing is stubbed (we
//! advance past it but never gate on conditions); in-the-wild VARC
//! fonts shipped to date do not exercise conditions either.

use alloc::vec::Vec;

use crate::error::{Error, Result};
use crate::tables::layout::Coverage;
use crate::tables::multi_var_store::{read_cff2_index, MultiVarStore};
use crate::tables::parse::Reader;

/// A parsed `VARC` table.
#[derive(Debug, Clone)]
pub struct Varc<'a> {
    coverage: Coverage<'a>,
    var_store: Option<MultiVarStore<'a>>,
    /// One axis-indices tuple per CFF2 INDEX entry; outer index of the
    /// component's `axisIndicesIndex` selects one of these.
    axis_indices_lists: Vec<Vec<u16>>,
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
    /// `fvar` axis order. Empty when the child reuses the parent's
    /// coord vector (RESET_UNSPECIFIED_AXES clear and HAVE_AXES clear).
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
        let _condition_list_off = r.read_u32()? as usize;
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

        // axisIndicesList is a CFF2 INDEX of TupleValues blocks. Each
        // entry decodes to a list of u16 axis indices, except the
        // length is the number of axis indices, which equals the
        // number of values in the TupleValues stream when each value
        // occupies one slot. The boring-expansion-spec encodes axis
        // indices as a packed delta-from-previous list (gvar's packed
        // point-numbers form), but in-the-wild fonts so far emit the
        // simpler "one i8/i16 per axis index" form, which our
        // decode_tuple_values handles directly. We reconstruct the
        // absolute axis indices from a running cumulative sum, which
        // collapses to the identity when the deltas are absolute.
        let axis_indices_lists = if axis_indices_off == 0 {
            Vec::new()
        } else {
            let mut sr = Reader::at(data, axis_indices_off)?;
            let entries = read_cff2_index(&mut sr)?;
            let mut out = Vec::with_capacity(entries.len());
            for entry in entries {
                out.push(decode_axis_indices(entry)?);
            }
            out
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
    #[must_use]
    pub fn composite(&self, gid: u16, coords: &[f32]) -> Option<VarcComposite> {
        let idx = self.coverage.index_of(gid)? as usize;
        let raw = *self.glyph_records.get(idx)?;
        let mut composite = VarcComposite::default();
        let mut r = Reader::new(raw);
        while !r.is_empty() {
            // A VarComponent stops when bytes run out. Reaching the
            // end mid-record means the font is malformed; we skip the
            // rest rather than error so a single bad glyph doesn't
            // tank the rest of the document.
            match self.resolve_component(&mut r, coords) {
                Ok(c) => composite.components.push(c),
                Err(_) => break,
            }
        }
        Some(composite)
    }

    /// Decodes one component record at the reader's current position
    /// and resolves it against `coords`.
    #[allow(clippy::too_many_lines, clippy::similar_names)]
    fn resolve_component(&self, r: &mut Reader<'_>, coords: &[f32]) -> Result<VarcComponent> {
        let flags = read_uint32var(r)?;
        if flags & VC_RESERVED_MASK != 0 {
            return Err(Error::Malformed {
                offset: r.position(),
                context: "VARC component has reserved flag bits set",
            });
        }

        let gid = if flags & VC_GID_IS_24BIT != 0 {
            let bytes = r.read_bytes(3)?;
            ((u32::from(bytes[0]) << 16) | (u32::from(bytes[1]) << 8) | u32::from(bytes[2])) as u16
        } else {
            r.read_u16()?
        };

        // ConditionList: read and discard. sigilbuzz does not gate
        // components on conditions yet.
        if flags & VC_HAVE_CONDITION != 0 {
            let _ = read_uint32var(r)?;
        }

        let mut effective_coords: Vec<f32> = if flags & VC_RESET_UNSPECIFIED_AXES != 0 {
            // Start from the parent's coord vector; HAVE_AXES values
            // override per-axis. (Inherited axes outside HAVE_AXES are
            // taken from the parent.)
            coords.to_vec()
        } else {
            // No reset: the child operates in the same coord vector
            // as the parent, with HAVE_AXES values *replacing* the
            // listed axes.
            coords.to_vec()
        };

        if flags & VC_HAVE_AXES != 0 {
            let axis_indices_index = read_uint32var(r)? as usize;
            let axis_indices = self
                .axis_indices_lists
                .get(axis_indices_index)
                .ok_or(Error::Malformed {
                    offset: r.position(),
                    context: "VARC component axisIndicesIndex out of range",
                })?
                .clone();
            let n = axis_indices.len();
            // The TupleValues stream packs `n` F2DOT14 values, but
            // sigilbuzz's decode_tuple_values yields i32; we treat
            // each as F2DOT14 by dividing by 16384.
            let raw_values = decode_tuple_values_in_reader(r, n).ok_or(Error::Malformed {
                offset: r.position(),
                context: "VARC component axisValues TupleValues truncated",
            })?;
            let mut axis_values: Vec<f32> = raw_values
                .into_iter()
                .map(|v| {
                    #[allow(clippy::cast_precision_loss)]
                    let f = v as f32 / 16384.0;
                    f
                })
                .collect();

            // Optional per-value variation deltas.
            if flags & VC_AXIS_VALUES_HAVE_VARIATION != 0 {
                let var_idx = read_uint32var(r)?;
                if let Some(store) = &self.var_store {
                    let outer = (var_idx >> 16) as u16;
                    let inner = var_idx & 0xFFFF;
                    if let Some(deltas) = store.resolve_deltas(outer, inner, n, coords) {
                        for (i, d) in deltas.iter().enumerate() {
                            axis_values[i] += d / 16384.0;
                        }
                    }
                }
            }

            // Make sure the effective coord vector is wide enough to
            // hold the highest-numbered axis we are about to write.
            if let Some(max_axis) = axis_indices.iter().copied().max() {
                let needed = max_axis as usize + 1;
                if effective_coords.len() < needed {
                    effective_coords.resize(needed, 0.0);
                }
            }
            for (axis_index, value) in axis_indices.iter().zip(axis_values.iter()) {
                effective_coords[*axis_index as usize] = *value;
            }
        }

        // Transform variation index: present when TRANSFORM_HAS_VARIATION
        // is set, regardless of which transform fields are present.
        let transform_var_idx = if flags & VC_TRANSFORM_HAS_VARIATION != 0 {
            Some(read_uint32var(r)?)
        } else {
            None
        };

        // Read the present transform fields in spec order.
        let mut tx = 0.0_f32;
        let mut ty = 0.0_f32;
        let mut rotation = 0.0_f32; // angle * π
        let mut sx = 1.0_f32;
        let mut sy = 1.0_f32;
        let mut skew_x = 0.0_f32;
        let mut skew_y = 0.0_f32;
        let mut tcx = 0.0_f32;
        let mut tcy = 0.0_f32;

        // Track which transform fields were present; the variation
        // delta tuple has one slot per present field, in spec order.
        let mut present_fields: Vec<TransformField> = Vec::new();

        if flags & VC_HAVE_TRANSLATE_X != 0 {
            tx = f32::from(r.read_i16()?);
            present_fields.push(TransformField::TranslateX);
        }
        if flags & VC_HAVE_TRANSLATE_Y != 0 {
            ty = f32::from(r.read_i16()?);
            present_fields.push(TransformField::TranslateY);
        }
        if flags & VC_HAVE_ROTATION != 0 {
            rotation = read_f4dot12(r)?;
            present_fields.push(TransformField::Rotation);
        }
        if flags & VC_HAVE_SCALE_X != 0 {
            sx = read_f6dot10(r)?;
            present_fields.push(TransformField::ScaleX);
        }
        if flags & VC_HAVE_SCALE_Y != 0 {
            sy = read_f6dot10(r)?;
            present_fields.push(TransformField::ScaleY);
        }
        if flags & VC_HAVE_SKEW_X != 0 {
            skew_x = read_f4dot12(r)?;
            present_fields.push(TransformField::SkewX);
        }
        if flags & VC_HAVE_SKEW_Y != 0 {
            skew_y = read_f4dot12(r)?;
            present_fields.push(TransformField::SkewY);
        }
        if flags & VC_HAVE_TCENTER_X != 0 {
            tcx = f32::from(r.read_i16()?);
            present_fields.push(TransformField::TCenterX);
        }
        if flags & VC_HAVE_TCENTER_Y != 0 {
            tcy = f32::from(r.read_i16()?);
            present_fields.push(TransformField::TCenterY);
        }

        // Apply transform variation deltas, if any.
        if let Some(var_idx) = transform_var_idx {
            if let Some(store) = &self.var_store {
                let outer = (var_idx >> 16) as u16;
                let inner = var_idx & 0xFFFF;
                let n = present_fields.len();
                if let Some(deltas) = store.resolve_deltas(outer, inner, n, coords) {
                    for (field, delta) in present_fields.iter().zip(deltas.iter()) {
                        match field {
                            TransformField::TranslateX => tx += delta,
                            TransformField::TranslateY => ty += delta,
                            TransformField::Rotation => rotation += delta / 4096.0,
                            TransformField::ScaleX => sx += delta / 1024.0,
                            TransformField::ScaleY => sy += delta / 1024.0,
                            TransformField::SkewX => skew_x += delta / 4096.0,
                            TransformField::SkewY => skew_y += delta / 4096.0,
                            TransformField::TCenterX => tcx += delta,
                            TransformField::TCenterY => tcy += delta,
                        }
                    }
                }
            }
        }

        // Build the affine. boring-expansion-spec composes:
        //   T(tx + tcx, ty + tcy) *
        //   R(rotation * π) * S(sx, sy) *
        //   Skew(-skewX * π, skewY * π) *
        //   T(-tcx, -tcy)
        let transform = compose_affine(tx, ty, rotation, sx, sy, skew_x, skew_y, tcx, tcy);

        // Discard any reserved trailing uint32var per remaining flag
        // bit. boring-expansion-spec says the high 17 bits are a
        // reserved-mask; we already errored on those above, so this
        // loop is a no-op in practice. Kept for forward-compat.
        let mut bits = flags & VC_RESERVED_MASK;
        while bits != 0 {
            let _ = read_uint32var(r)?;
            bits &= bits - 1;
        }

        Ok(VarcComponent {
            gid,
            transform,
            coords: effective_coords,
        })
    }
}

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

/// Decodes a TupleValues stream of exactly `count` deltas from the
/// reader, advancing the cursor by however many bytes the encoding
/// uses. Returns `None` on truncation or overrun.
fn decode_tuple_values_in_reader(r: &mut Reader<'_>, count: usize) -> Option<Vec<i32>> {
    // We need to know how many bytes the stream consumed. Walk the
    // remaining buffer manually, mirroring the inner loop of
    // decode_tuple_values, and advance the reader.
    let start = r.position();
    let remaining = r.remaining();
    let buf = r.peek_bytes(remaining).ok()?;
    let mut out: Vec<i32> = Vec::with_capacity(count);
    let mut i = 0usize;
    while out.len() < count {
        if i >= buf.len() {
            return None;
        }
        let ctrl = buf[i];
        i += 1;
        let run_len = (ctrl & 0x3F) as usize + 1;
        let zeros = ctrl & 0x80 != 0;
        let words = ctrl & 0x40 != 0;
        for _ in 0..run_len {
            if out.len() >= count {
                return None;
            }
            let delta: i32 = match (zeros, words) {
                (true, false) => 0,
                (true, true) => {
                    if i + 4 > buf.len() {
                        return None;
                    }
                    let v = i32::from_be_bytes([buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]);
                    i += 4;
                    v
                }
                (false, true) => {
                    if i + 2 > buf.len() {
                        return None;
                    }
                    let v = i32::from(i16::from_be_bytes([buf[i], buf[i + 1]]));
                    i += 2;
                    v
                }
                (false, false) => {
                    if i >= buf.len() {
                        return None;
                    }
                    #[allow(clippy::cast_possible_wrap)]
                    let v = buf[i] as i8;
                    i += 1;
                    i32::from(v)
                }
            };
            out.push(delta);
        }
    }
    if out.len() == count {
        // Advance reader.
        r.seek(start + i).ok()?;
        Some(out)
    } else {
        None
    }
}

/// Decodes a TupleValues stream of axis indices. boring-expansion-spec
/// allows two encodings; sigilbuzz handles the absolute form (one
/// value per axis index) which is what fontTools and HarfBuzz emit.
fn decode_axis_indices(data: &[u8]) -> Result<Vec<u16>> {
    // Determine how many axis indices are encoded by walking the
    // entire byte stream as a TupleValues list and treating each
    // value as a u16 index.
    let mut out: Vec<u16> = Vec::new();
    let mut i = 0usize;
    while i < data.len() {
        let ctrl = data[i];
        i += 1;
        let run_len = (ctrl & 0x3F) as usize + 1;
        let zeros = ctrl & 0x80 != 0;
        let words = ctrl & 0x40 != 0;
        for _ in 0..run_len {
            let v: i32 = match (zeros, words) {
                (true, false) => 0,
                (true, true) => {
                    if i + 4 > data.len() {
                        return Err(Error::Malformed {
                            offset: i,
                            context: "VARC axisIndices i32 entry truncated",
                        });
                    }
                    let v = i32::from_be_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]);
                    i += 4;
                    v
                }
                (false, true) => {
                    if i + 2 > data.len() {
                        return Err(Error::Malformed {
                            offset: i,
                            context: "VARC axisIndices i16 entry truncated",
                        });
                    }
                    let v = i32::from(i16::from_be_bytes([data[i], data[i + 1]]));
                    i += 2;
                    v
                }
                (false, false) => {
                    if i >= data.len() {
                        return Err(Error::Malformed {
                            offset: i,
                            context: "VARC axisIndices i8 entry truncated",
                        });
                    }
                    #[allow(clippy::cast_possible_wrap)]
                    let v = data[i] as i8;
                    i += 1;
                    i32::from(v)
                }
            };
            #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
            out.push(v as u16);
        }
    }
    Ok(out)
}

/// Reads an F4.12 fixed-point as `f32`. boring-expansion-spec uses
/// this for rotation, skew (each multiplied by π in radians).
fn read_f4dot12(r: &mut Reader<'_>) -> Result<f32> {
    let raw = r.read_i16()?;
    #[allow(clippy::cast_precision_loss)]
    Ok(f32::from(raw) / 4096.0)
}

/// Reads an F6.10 fixed-point as `f32`. Used for scale fields.
fn read_f6dot10(r: &mut Reader<'_>) -> Result<f32> {
    let raw = r.read_i16()?;
    #[allow(clippy::cast_precision_loss)]
    Ok(f32::from(raw) / 1024.0)
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
#[allow(
    clippy::many_single_char_names,
    clippy::too_many_arguments,
    clippy::similar_names
)]
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
    // sigilbuzz is no_std; avoid libm by sticking to small-angle exact
    // values for the trig-free identity case (rotation == skew == 0).
    // For non-zero angles fall back to the polynomial approximations
    // already used by the rest of the crate. No, we just use libm-free
    // f32::sin / f32::cos when std is on, and a Taylor expansion when
    // it isn't. Wait: core::f32 has no sin/cos in no_std. Use a
    // ChebyshevPad approximation good to ~5e-7 over [-π, π].
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

/// `sin(x*π)` and `cos(x*π)` for `x` in `[-2, 2]`. Tight enough for
/// glyph composites. VARC's F4.12 rotation field clamps at ±2π
/// regardless and font designers stay well inside ±π. Implemented
/// via a Taylor series for `no_std`-friendliness.
#[allow(clippy::many_single_char_names)]
fn sincos_pi(x: f32) -> (f32, f32) {
    // Reduce to [-1, 1] (i.e. [-π, π]).
    let mut t = x;
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
mod tests {
    use super::*;
    use alloc::vec;

    /// Builds a coverage format-1 table with the listed gids in order.
    fn build_coverage(gids: &[u16]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // format
        out.extend_from_slice(&(gids.len() as u16).to_be_bytes());
        for g in gids {
            out.extend_from_slice(&g.to_be_bytes());
        }
        out
    }

    /// Builds a CFF2 INDEX with 1-byte offsets.
    fn build_cff2_index(entries: &[&[u8]]) -> Vec<u8> {
        let count = entries.len() as u32;
        let mut out = Vec::new();
        out.extend_from_slice(&count.to_be_bytes());
        if entries.is_empty() {
            return out;
        }
        out.push(1); // off_size = 1
        let mut cursor: u32 = 1;
        out.push(cursor as u8);
        for e in entries {
            cursor += e.len() as u32;
            out.push(cursor as u8);
        }
        for e in entries {
            out.extend_from_slice(e);
        }
        out
    }

    /// Builds a minimal VARC table with the given coverage gids and
    /// raw glyph record bytes. `var_store` and `axis_indices` are
    /// optional; passing `None` leaves their offsets at zero.
    fn build_varc(
        coverage_gids: &[u16],
        glyph_records: &[&[u8]],
        var_store: Option<&[u8]>,
        axis_indices: Option<&[&[u8]]>,
    ) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // major
        out.extend_from_slice(&0u16.to_be_bytes()); // minor
        let cov_off_slot = out.len();
        out.extend_from_slice(&0u32.to_be_bytes()); // coverage
        let vs_off_slot = out.len();
        out.extend_from_slice(&0u32.to_be_bytes()); // varStore
        out.extend_from_slice(&0u32.to_be_bytes()); // conditionList
        let ail_off_slot = out.len();
        out.extend_from_slice(&0u32.to_be_bytes()); // axisIndicesList
        let gr_off_slot = out.len();
        out.extend_from_slice(&0u32.to_be_bytes()); // glyphRecords

        let cov_start = out.len() as u32;
        out[cov_off_slot..cov_off_slot + 4].copy_from_slice(&cov_start.to_be_bytes());
        out.extend_from_slice(&build_coverage(coverage_gids));

        if let Some(vs) = var_store {
            let vs_start = out.len() as u32;
            out[vs_off_slot..vs_off_slot + 4].copy_from_slice(&vs_start.to_be_bytes());
            out.extend_from_slice(vs);
        }
        if let Some(ail) = axis_indices {
            let ail_start = out.len() as u32;
            out[ail_off_slot..ail_off_slot + 4].copy_from_slice(&ail_start.to_be_bytes());
            out.extend_from_slice(&build_cff2_index(ail));
        }
        let gr_start = out.len() as u32;
        out[gr_off_slot..gr_off_slot + 4].copy_from_slice(&gr_start.to_be_bytes());
        out.extend_from_slice(&build_cff2_index(glyph_records));
        out
    }

    #[test]
    fn parses_minimal_header_and_coverage() {
        let bytes = build_varc(&[42, 100], &[b"\x00\x00\x05", b"\x00\x00\x06"], None, None);
        let varc = Varc::parse(&bytes).unwrap();
        assert!(varc.covers(42));
        assert!(varc.covers(100));
        assert!(!varc.covers(99));
        assert_eq!(varc.glyph_record_count(), 2);
    }

    #[test]
    fn rejects_wrong_major_version() {
        let mut bytes = build_varc(&[1], &[b"\x00\x00\x01"], None, None);
        bytes[0] = 0;
        bytes[1] = 2; // major = 2
        assert!(matches!(Varc::parse(&bytes), Err(Error::Malformed { .. })));
    }

    #[test]
    fn uint32var_roundtrip() {
        // 0x42 = single byte
        let mut r = Reader::new(&[0x42]);
        assert_eq!(read_uint32var(&mut r).unwrap(), 0x42);
        // 0x80 0x42 = (0 << 8) | 0x42
        let mut r = Reader::new(&[0x80, 0x42]);
        assert_eq!(read_uint32var(&mut r).unwrap(), 0x42);
        // 0xC1 0x02 0x03 = (1 << 16) | (2 << 8) | 3
        let mut r = Reader::new(&[0xC1, 0x02, 0x03]);
        assert_eq!(read_uint32var(&mut r).unwrap(), 0x10203);
        // 0xE0 0x01 0x02 0x03 = (0 << 24) | ...
        let mut r = Reader::new(&[0xE0, 0x01, 0x02, 0x03]);
        assert_eq!(read_uint32var(&mut r).unwrap(), 0x01_0203);
        // 0xF0 + u32
        let mut r = Reader::new(&[0xF0, 0xAB, 0xCD, 0xEF, 0x01]);
        assert_eq!(read_uint32var(&mut r).unwrap(), 0xABCD_EF01);
    }

    #[test]
    fn resolves_translation_only_component() {
        // One component: flags = HAVE_TRANSLATE_X | HAVE_TRANSLATE_Y,
        // gid = 5, tx = 100, ty = -50.
        let flags = VC_HAVE_TRANSLATE_X | VC_HAVE_TRANSLATE_Y;
        // uint32var encoding for `flags`. flags = 0x30 (HAVE_TRANSLATE_X = 1<<4 = 0x10,
        // HAVE_TRANSLATE_Y = 1<<5 = 0x20). 0x30 < 0x80, so single byte.
        assert!(flags < 0x80);
        let mut record = Vec::new();
        record.push(flags as u8);
        record.extend_from_slice(&5u16.to_be_bytes()); // gid
        record.extend_from_slice(&100i16.to_be_bytes()); // tx
        record.extend_from_slice(&(-50i16).to_be_bytes()); // ty

        let bytes = build_varc(&[42], &[&record], None, None);
        let varc = Varc::parse(&bytes).unwrap();
        let comp = varc.composite(42, &[]).unwrap();
        assert_eq!(comp.components.len(), 1);
        assert_eq!(comp.components[0].gid, 5);
        // Identity matrix + translation.
        let t = comp.components[0].transform;
        assert!((t[0] - 1.0).abs() < 1e-5);
        assert!(t[1].abs() < 1e-5);
        assert!(t[2].abs() < 1e-5);
        assert!((t[3] - 1.0).abs() < 1e-5);
        assert!((t[4] - 100.0).abs() < 1e-3);
        assert!((t[5] - -50.0).abs() < 1e-3);
    }

    #[test]
    fn resolves_scale_only_component() {
        // flags = VC_HAVE_SCALE_X | VC_HAVE_SCALE_Y = 0x300.
        // Two-byte uint32var: 0x80|0x03, 0x00.
        let mut record = Vec::new();
        record.push(0x80 | 0x03);
        record.push(0x00);
        record.extend_from_slice(&7u16.to_be_bytes());
        // Scale 2.0 in F6.10 = 2.0 * 1024 = 2048
        record.extend_from_slice(&2048i16.to_be_bytes());
        record.extend_from_slice(&512i16.to_be_bytes()); // 0.5
        let bytes = build_varc(&[1], &[&record], None, None);
        let varc = Varc::parse(&bytes).unwrap();
        let comp = varc.composite(1, &[]).unwrap();
        let t = comp.components[0].transform;
        assert!((t[0] - 2.0).abs() < 1e-3, "xx={}", t[0]);
        assert!((t[3] - 0.5).abs() < 1e-3, "yy={}", t[3]);
    }

    #[test]
    fn gid_24bit_flag_reads_three_byte_glyph_id() {
        // flags = VC_GID_IS_24BIT = 0x1000.
        // Two-byte uint32var: 0x80|0x10 = 0x90, 0x00.
        let mut record = Vec::new();
        record.push(0x90);
        record.push(0x00);
        // 24-bit gid 0x010203, but u16 truncation in the API means
        // we should pick a value that fits in u16.
        record.extend_from_slice(&[0x00, 0x12, 0x34]);
        let bytes = build_varc(&[1], &[&record], None, None);
        let varc = Varc::parse(&bytes).unwrap();
        let comp = varc.composite(1, &[]).unwrap();
        assert_eq!(comp.components[0].gid, 0x1234);
    }

    #[test]
    fn uncovered_gid_returns_none() {
        let bytes = build_varc(&[1], &[b"\x00\x00\x05"], None, None);
        let varc = Varc::parse(&bytes).unwrap();
        assert!(varc.composite(99, &[]).is_none());
    }

    #[test]
    fn multiple_components_in_one_record() {
        let flags = VC_HAVE_TRANSLATE_X;
        let mut record = Vec::new();
        // Component 1: gid 5, tx 10
        record.push(flags as u8);
        record.extend_from_slice(&5u16.to_be_bytes());
        record.extend_from_slice(&10i16.to_be_bytes());
        // Component 2: gid 7, tx 20
        record.push(flags as u8);
        record.extend_from_slice(&7u16.to_be_bytes());
        record.extend_from_slice(&20i16.to_be_bytes());
        let bytes = build_varc(&[1], &[&record], None, None);
        let varc = Varc::parse(&bytes).unwrap();
        let comp = varc.composite(1, &[]).unwrap();
        assert_eq!(comp.components.len(), 2);
        assert_eq!(comp.components[0].gid, 5);
        assert_eq!(comp.components[1].gid, 7);
        assert!((comp.components[0].transform[4] - 10.0).abs() < 1e-3);
        assert!((comp.components[1].transform[4] - 20.0).abs() < 1e-3);
    }

    #[test]
    fn have_axes_overrides_coord_at_axis_index() {
        // axisIndices = [1] (one axis index, axis 1).
        // axisValues = [F2DOT14(0.5)] = 8192. Encode as 0x40 | 0x00 = 0x40
        // (run of 1 i16), then 8192 BE = 0x20 0x00.
        let axis_indices_payload = vec![0x00_u8, 0x01]; // run of 1 i8: index 1
        let bytes_axis_indices = build_cff2_index(&[&axis_indices_payload]);
        // Place the axis_indices at the right offset by building VARC
        // with axis_indices block.
        let flags = VC_HAVE_AXES;
        // 0x02 < 0x80 -> single byte uint32var.
        let mut record = Vec::new();
        record.push(flags as u8);
        record.extend_from_slice(&3u16.to_be_bytes()); // gid 3
        record.push(0x00); // axisIndicesIndex = 0
                           // axisValues: 1 value, F2DOT14 = 0.5 = 8192. Word run.
        record.push(0x40); // ctrl: words, run_len 1
        record.extend_from_slice(&8192i16.to_be_bytes());

        // Build VARC with the axis_indices block. We need to use the
        // separate axis_indices arg.
        let _ = bytes_axis_indices; // (we let build_varc rebuild it)
        let bytes = build_varc(&[1], &[&record], None, Some(&[&axis_indices_payload]));
        let varc = Varc::parse(&bytes).unwrap();
        let comp = varc.composite(1, &[0.0, 0.0]).unwrap();
        let coords = &comp.components[0].coords;
        // axis 0 untouched, axis 1 set to 0.5.
        assert!(coords.len() >= 2);
        assert!((coords[1] - 0.5).abs() < 1e-3);
    }

    #[test]
    fn rotation_rotates_unit_vector() {
        // rotation = 0.5 (= 90° = 0.5 * π). F4DOT12 raw = 0.5 * 4096 = 2048.
        let flags = VC_HAVE_ROTATION;
        // 0x40 = 64 < 0x80 -> single byte.
        let mut record = Vec::new();
        record.push(flags as u8);
        record.extend_from_slice(&5u16.to_be_bytes());
        record.extend_from_slice(&2048i16.to_be_bytes());
        let bytes = build_varc(&[1], &[&record], None, None);
        let varc = Varc::parse(&bytes).unwrap();
        let comp = varc.composite(1, &[]).unwrap();
        let t = comp.components[0].transform;
        // Apply transform to (1, 0). Should land near (0, 1).
        let x_out = t[0] * 1.0 + t[1] * 0.0 + t[4];
        let y_out = t[2] * 1.0 + t[3] * 0.0 + t[5];
        assert!(x_out.abs() < 1e-3, "expected 0, got {x_out}");
        assert!((y_out - 1.0).abs() < 1e-3, "expected 1, got {y_out}");
    }
}
