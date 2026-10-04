//! The metric bakes of an instance: hmtx and vmtx from the baked
//! glyphs' phantom points (or, without `gvar`, through HVAR and VVAR),
//! VORG through VVAR, the head bounding box and the hhea and vhea
//! extremes, and the OS/2, hhea, vhea and post fields MVAR varies.

use alloc::collections::BTreeSet;
use alloc::vec::Vec;

use sigilbuzz::tables::tag;
use sigilbuzz::Face;

use super::glyf::{clamp_i16, GlyphMetrics};
use super::ivs::{project_ivs_with, Projection};
use super::AxisPin;
use crate::hmtx::emit_long_metrics;
use crate::hvar::{read_index_map, STORE_SLOT, VVAR_VORG_SLOT};
use crate::util::{round_half_up, StoreDeltas};
use crate::warnings::Warnings;
use crate::SubsetError;

// ---------------------------------------------------------------------------
// hmtx bake
// ---------------------------------------------------------------------------

pub(super) struct HmtxBake {
    pub(super) bytes: Vec<u8>,
    pub(super) number_of_h_metrics: u16,
    /// Every glyph's advance, in glyph order.
    pub(super) advances: Vec<u16>,
    /// Every glyph's left side bearing, in glyph order.
    pub(super) lsbs: Vec<i16>,
}

impl HmtxBake {
    /// The `OS/2` `xAvgCharWidth` of these metrics, as HarfBuzz's
    /// instancer works it out: the mean of the advances that are not
    /// zero, rounded, or 0 when every advance is.
    pub(super) fn avg_char_width(&self) -> u16 {
        let (total, count) = self
            .advances
            .iter()
            .filter(|&&a| a != 0)
            .fold((0u64, 0u64), |(t, c), &a| (t + u64::from(a), c + 1));
        if count == 0 {
            return 0;
        }
        // HarfBuzz's `roundf` of the quotient in double precision,
        // `floor(x + 0.5)`. At most 65,535 advances of at most 65,535
        // each, so the sum fits and so does the mean.
        (total as f64 / count as f64 + 0.5).floor() as u16
    }
}

/// Builds `hmtx` from the metrics of the baked glyphs.
pub(super) fn hmtx_from_metrics(metrics: &[GlyphMetrics]) -> HmtxBake {
    let advances: Vec<u16> = metrics.iter().map(|m| m.advance).collect();
    let lsbs: Vec<i16> = metrics.iter().map(|m| m.lsb).collect();
    let (bytes, number_of_h_metrics) = emit_long_metrics(&advances, &lsbs);
    HmtxBake {
        bytes,
        number_of_h_metrics,
        advances,
        lsbs,
    }
}

// ---------------------------------------------------------------------------
// head bounding box and hhea / vhea extremes
// ---------------------------------------------------------------------------

/// Writes the union of the baked glyphs' bounding boxes into the `head`
/// bytes (`xMin`, `yMin`, `xMax`, `yMax` at bytes 36 to 43). Glyphs
/// with no outline do not count; when none has one, `head` keeps its
/// box.
pub(super) fn patch_head_bounds(head: &mut [u8], metrics: &[GlyphMetrics]) {
    let mut boxes = metrics.iter().filter_map(|m| m.bounds);
    let Some(first) = boxes.next() else {
        return;
    };
    let union = boxes.fold(first, |u, b| {
        [
            u[0].min(b[0]),
            u[1].min(b[1]),
            u[2].max(b[2]),
            u[3].max(b[3]),
        ]
    });
    write_head_box(head, union);
}

/// Writes `(xMin, yMin, xMax, yMax)` into the `head` bytes (bytes 36
/// to 43).
pub(super) fn write_head_box(head: &mut [u8], bounds: [i16; 4]) {
    for (i, v) in bounds.iter().enumerate() {
        write_i16(head, 36 + 2 * i, *v);
    }
}

/// Writes the extremes of the baked metrics into `hhea` (`vertical`
/// false) or `vhea` (`vertical` true), as HarfBuzz's instancer does:
/// the largest advance (byte 10), then, over the glyphs with an
/// outline, the smallest leading bearing (12), the smallest trailing
/// bearing (14), and the largest extent, the leading bearing plus the
/// outline's size (16). Without a glyph with an outline only the
/// largest advance changes.
pub(super) fn patch_line_extremes(table: &mut [u8], metrics: &[GlyphMetrics], vertical: bool) {
    let advance = |m: &GlyphMetrics| if vertical { m.v_advance } else { m.advance };
    let max_advance = metrics.iter().map(advance).max().unwrap_or(0);
    write_u16(table, 10, max_advance);
    let mut extremes: Option<(i32, i32, i32)> = None;
    for m in metrics {
        let Some([x_min, y_min, x_max, y_max]) = m.bounds else {
            continue;
        };
        let (lead, size) = if vertical {
            (i32::from(m.tsb), i32::from(y_max) - i32::from(y_min))
        } else {
            (i32::from(m.lsb), i32::from(x_max) - i32::from(x_min))
        };
        let trail = i32::from(advance(m)) - lead - size;
        let extent = lead + size;
        let e = extremes.get_or_insert((lead, trail, extent));
        *e = (e.0.min(lead), e.1.min(trail), e.2.max(extent));
    }
    if let Some((lead, trail, extent)) = extremes {
        write_i16(table, 12, clamp_i16(lead));
        write_i16(table, 14, clamp_i16(trail));
        write_i16(table, 16, clamp_i16(extent));
    }
}

/// Writes the largest advance (byte 10) and, when given, the smallest
/// leading and trailing bearings and the largest extent (bytes 12, 14
/// and 16) into `hhea` or `vhea`.
pub(super) fn write_line_extremes(table: &mut [u8], max_advance: u16, extremes: Option<[i16; 3]>) {
    write_u16(table, 10, max_advance);
    if let Some([lead, trail, extent]) = extremes {
        write_i16(table, 12, lead);
        write_i16(table, 14, trail);
        write_i16(table, 16, extent);
    }
}

/// Writes a big-endian `i16` at `off` when the table is long enough.
fn write_i16(buf: &mut [u8], off: usize, v: i16) {
    if let Some(field) = buf.get_mut(off..).and_then(<[u8]>::first_chunk_mut::<2>) {
        *field = v.to_be_bytes();
    }
}

/// Writes a big-endian `u16` at `off` when the table is long enough.
fn write_u16(buf: &mut [u8], off: usize, v: u16) {
    if let Some(field) = buf.get_mut(off..).and_then(<[u8]>::first_chunk_mut::<2>) {
        *field = v.to_be_bytes();
    }
}

/// The deltas at `coords` of the store of the HVAR or VVAR `table` (the
/// store offset sits at byte 4 of both), reporting a spent budget
/// against `tag`; `None` when the store cannot be read.
fn metrics_deltas<'s, 'a>(
    table: &'a [u8],
    coords: &'s [f32],
    warnings: &'s Warnings,
    tag: [u8; 4],
) -> Option<StoreDeltas<'s, 'a>> {
    let off = table.get(STORE_SLOT..).and_then(<[u8]>::first_chunk::<4>)?;
    let store = table.get(u32::from_be_bytes(*off) as usize..)?;
    StoreDeltas::new(store, coords).map(|d| d.reporting(warnings, tag))
}

/// The offset of the index map whose slot is at byte `slot` of `table`;
/// 0 when there is none.
fn map_offset(table: &[u8], slot: usize) -> usize {
    table
        .get(slot..)
        .and_then(<[u8]>::first_chunk::<4>)
        .map_or(0, |b| u32::from_be_bytes(*b) as usize)
}

/// The delta `deltas` gives glyph `gid` through the index map at
/// `map_off` of `table`, as the core `HVAR` and `VVAR` readers give it:
/// without a map, an advance reads row `(0, gid)` (`implicit`) and other
/// metrics have none; a glyph the map does not name has none.
fn glyph_delta(
    deltas: &StoreDeltas<'_, '_>,
    table: &[u8],
    map_off: usize,
    gid: u16,
    implicit: bool,
) -> f32 {
    if map_off == 0 {
        return if implicit { deltas.get(0, gid) } else { 0.0 };
    }
    read_index_map(table, map_off, gid).map_or(0.0, |(outer, inner)| deltas.get(outer, inner))
}

pub(super) fn bake_hmtx(
    face: &Face<'_>,
    coords: &[f32],
    num_glyphs: u16,
    warnings: &Warnings,
) -> Result<HmtxBake, SubsetError> {
    let hmtx = face.hmtx().map_err(SubsetError::from)?;
    let hvar = face.hvar().map_err(SubsetError::from)?;
    // Many glyphs can map to one row; each row is resolved once.
    let hvar_bytes = hvar
        .as_ref()
        .and_then(|_| face.table_bytes(tag::HVAR).ok())
        .filter(|_| !coords.is_empty());
    let deltas = hvar_bytes.and_then(|b| metrics_deltas(b, coords, warnings, tag::HVAR));
    let advance_map = hvar_bytes.map_or(0, |b| map_offset(b, 8));

    let mut advances: Vec<u16> = Vec::with_capacity(num_glyphs as usize);
    let mut lsbs: Vec<i16> = Vec::with_capacity(num_glyphs as usize);
    for gid in 0..num_glyphs {
        let base_adv = hmtx.advance(gid).unwrap_or(0);
        let base_lsb = hmtx.lsb(gid).unwrap_or(0);
        let adv_delta = match (&deltas, hvar_bytes) {
            (Some(d), Some(b)) => glyph_delta(d, b, advance_map, gid, true),
            _ => 0.0,
        };
        // hmtx advances are unsigned; clamp at 0 if a delta would
        // underflow. In practice this only happens with malformed
        // HVAR data.
        // The delta rounds halves up before it is added, as in HarfBuzz.
        let new_adv = i32::from(base_adv).saturating_add(round_half_up(adv_delta));
        advances.push(new_adv.clamp(0, i32::from(u16::MAX)) as u16);
        lsbs.push(base_lsb);
    }

    // Compress trailing identical advances into the LSB-only tail.
    let (bytes, number_of_h_metrics) = emit_long_metrics(&advances, &lsbs);
    Ok(HmtxBake {
        bytes,
        number_of_h_metrics,
        advances,
        lsbs,
    })
}

// ---------------------------------------------------------------------------
// vmtx bake (VVAR-aware)
// ---------------------------------------------------------------------------

/// What a warning about a vertical metrics table left out.
const VERTICAL_DROPPED: &str = "the vhea and vmtx tables";

pub(super) struct VmtxBake {
    /// New `vmtx` bytes, or `None` when the source has no `vmtx` or
    /// it is left out (see `left_out`).
    pub(super) vmtx_bytes: Option<Vec<u8>>,
    /// Recomputed `numberOfLongVerMetrics` for the rebuilt table. The
    /// caller must patch `vhea` with this value when it differs from
    /// the source's count. Holds zero when no vmtx was emitted.
    pub(super) number_of_long_ver_metrics: u16,
    /// Source tables the bake could not read, which the instance
    /// leaves out: `vhea` and `vmtx` together, and a `VVAR` whose
    /// deltas could not be folded in. Each is reported in the warnings.
    pub(super) left_out: Vec<[u8; 4]>,
}

impl VmtxBake {
    /// No `vmtx` to emit, leaving out `left_out`.
    fn without_vmtx(left_out: Vec<[u8; 4]>) -> Self {
        Self {
            vmtx_bytes: None,
            number_of_long_ver_metrics: 0,
            left_out,
        }
    }
}

/// Rebuilds `vmtx` with the `VVAR` advance height and top side bearing
/// deltas at `coords` folded in, or, when `baked` holds the metrics of
/// the baked glyphs, from their vertical phantom points instead.
///
/// A malformed `vhea` or `vmtx` is left out with its partner, and a
/// malformed `VVAR` is left out and its deltas not applied, as the
/// subsetter does; each is reported in `warnings` and named in
/// [`VmtxBake::left_out`]. A `vmtx` without a `vhea` cannot be sliced,
/// so it is not rebuilt and rides through as it is.
pub(super) fn bake_vmtx(
    face: &Face<'_>,
    coords: &[f32],
    num_glyphs: u16,
    baked: Option<&[GlyphMetrics]>,
    warnings: &Warnings,
) -> VmtxBake {
    // Parse `vhea` on its own first, so a problem there is reported
    // against `vhea` rather than the `vmtx` that depends on it. The
    // long count it holds is recomputed below from the post-VVAR
    // advances.
    if let Err(e) = face.vhea() {
        warnings.parse_error(tag::VHEA, 0, &e, VERTICAL_DROPPED);
        return VmtxBake::without_vmtx(alloc::vec![tag::VHEA, tag::VMTX]);
    }
    let vmtx = match face.vmtx() {
        Ok(Some(vmtx)) => vmtx,
        Ok(None) => return VmtxBake::without_vmtx(Vec::new()),
        Err(e) => {
            warnings.parse_error(tag::VMTX, 0, &e, VERTICAL_DROPPED);
            return VmtxBake::without_vmtx(alloc::vec![tag::VHEA, tag::VMTX]);
        }
    };

    // The baked glyphs' phantom points give the metrics directly.
    if let Some(baked) = baked {
        let advances: Vec<u16> = baked.iter().map(|m| m.v_advance).collect();
        let tsbs: Vec<i16> = baked.iter().map(|m| m.tsb).collect();
        let (out, long_count) = emit_long_metrics(&advances, &tsbs);
        return VmtxBake {
            vmtx_bytes: Some(out),
            number_of_long_ver_metrics: long_count,
            left_out: Vec::new(),
        };
    }

    let mut left_out = Vec::new();
    let vvar = face.vvar().unwrap_or_else(|e| {
        warnings.parse_error(tag::VVAR, 0, &e, "the whole table");
        left_out.push(tag::VVAR);
        None
    });

    // Compute the new (advance, tsb) per gid. Every glyph that ends
    // up in the long range carries its own advance; trailing glyphs
    // share the last advance. We resolve VVAR deltas for *every* gid
    // (including those originally past the source's long count) so
    // that a trailing glyph whose advance now diverges from the
    // shared one extends the long range below.
    let mut advances: Vec<u16> = Vec::with_capacity(num_glyphs as usize);
    let mut tsbs: Vec<i16> = Vec::with_capacity(num_glyphs as usize);
    // Many glyphs can map to one row; each row is resolved once.
    let vvar_bytes = vvar
        .as_ref()
        .and_then(|_| face.table_bytes(tag::VVAR).ok())
        .filter(|_| !coords.is_empty());
    let deltas = vvar_bytes.and_then(|b| metrics_deltas(b, coords, warnings, tag::VVAR));
    let (advance_map, tsb_map) =
        vvar_bytes.map_or((0, 0), |b| (map_offset(b, 8), map_offset(b, 12)));
    for gid in 0..num_glyphs {
        let base_adv = vmtx.advance(gid).unwrap_or(0);
        let base_tsb = vmtx.tsb(gid).unwrap_or(0);
        let (adv_delta, tsb_delta) = match (&deltas, vvar_bytes) {
            (Some(d), Some(b)) => (
                glyph_delta(d, b, advance_map, gid, true),
                glyph_delta(d, b, tsb_map, gid, false),
            ),
            _ => (0.0, 0.0),
        };
        let new_adv = i32::from(base_adv).saturating_add(round_half_up(adv_delta));
        advances.push(new_adv.clamp(0, i32::from(u16::MAX)) as u16);
        let new_tsb = i32::from(base_tsb).saturating_add(round_half_up(tsb_delta));
        tsbs.push(clamp_i16(new_tsb));
    }

    // The long count is recomputed, so trailing glyphs that now share
    // an advance fold into the tsb-only tail, and a VVAR delta that
    // sets a trailing glyph's advance apart extends the long range.
    let (out, long_count) = emit_long_metrics(&advances, &tsbs);

    VmtxBake {
        vmtx_bytes: Some(out),
        number_of_long_ver_metrics: long_count,
        left_out,
    }
}

// ---------------------------------------------------------------------------
// VORG bake (VVAR vertical origin deltas)
// ---------------------------------------------------------------------------

/// What a full instance does with `VORG`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum VorgBake {
    /// Nothing to fold in: the source table, if any, passes through.
    Unchanged,
    /// The table with the `VVAR` vertical origin deltas folded in.
    Rebuilt(Vec<u8>),
    /// The `VORG` could not be read; it is left out and reported.
    Dropped,
}

/// Folds the `VVAR` vertical origin deltas at `coords` into `VORG`, as
/// [`bake_vmtx`] folds the advance and top side bearing deltas into
/// `vmtx`. HarfBuzz adds the same delta to every `VORG` lookup of a
/// variable font, so a glyph with no `VORG` entry moves too, and gains
/// one when its origin leaves the default.
///
/// The core `VVAR` parser does not read `vorgMappingOffset`, so the
/// map is read here, from the raw table. A `VVAR` store that cannot be
/// read leaves `VORG` unchanged, and a malformed `VORG` is dropped;
/// both are reported in `warnings`.
pub(super) fn bake_vorg(
    face: &Face<'_>,
    coords: &[f32],
    num_glyphs: u16,
    warnings: &Warnings,
) -> VorgBake {
    if coords.is_empty() {
        return VorgBake::Unchanged;
    }
    let (Ok(vorg_bytes), Ok(vvar_bytes)) =
        (face.table_bytes(tag::VORG), face.table_bytes(tag::VVAR))
    else {
        return VorgBake::Unchanged;
    };
    let offset_at = |slot: usize| {
        vvar_bytes
            .get(slot..)
            .and_then(<[u8]>::first_chunk::<4>)
            .map_or(0, |b| u32::from_be_bytes(*b) as usize)
    };
    let map_off = offset_at(VVAR_VORG_SLOT);
    if map_off == 0 {
        return VorgBake::Unchanged;
    }
    let store_off = offset_at(STORE_SLOT);
    let store = match vvar_bytes
        .get(store_off..)
        .map(|b| sigilbuzz::tables::variation_store::ItemVariationStore::parse(b).map(|_| b))
    {
        Some(Ok(store)) => store,
        Some(Err(e)) => {
            warnings.parse_error(tag::VVAR, store_off, &e, "the vertical origin deltas");
            return VorgBake::Unchanged;
        }
        None => {
            warnings.push(
                tag::VVAR,
                STORE_SLOT,
                "VVAR store offset past end",
                "the vertical origin deltas",
            );
            return VorgBake::Unchanged;
        }
    };
    let Some(deltas) = StoreDeltas::new(store, coords).map(|d| d.reporting(warnings, tag::VORG))
    else {
        return VorgBake::Unchanged;
    };
    let delta = |gid: u16| glyph_delta(&deltas, vvar_bytes, map_off, gid, false);
    match crate::vorg::bake_vorg(vorg_bytes, num_glyphs, delta) {
        Ok(bytes) => VorgBake::Rebuilt(bytes),
        Err(e) => {
            warnings.parse_error(tag::VORG, 0, &e, "the whole table");
            VorgBake::Dropped
        }
    }
}

// ---------------------------------------------------------------------------
// MVAR bake (OS/2 + hhea + vhea + post)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub(super) struct MvarBake {
    pub(super) os2: Option<Vec<u8>>,
    pub(super) hhea: Option<Vec<u8>>,
    pub(super) vhea: Option<Vec<u8>>,
    pub(super) post: Option<Vec<u8>>,
}

/// Walks the source's `MVAR` records, applies each delta to its target
/// field in OS/2 / hhea / vhea / post, and returns the patched table
/// bytes. Tables that don't exist in the source, or whose fields no
/// MVAR record references, return `None` (caller passes through the
/// source bytes).
pub(super) fn bake_mvar_metrics(face: &Face<'_>, coords: &[f32]) -> Result<MvarBake, SubsetError> {
    let mvar = face.mvar().map_err(SubsetError::from)?;
    let Some(mvar) = mvar else {
        return Ok(MvarBake::default());
    };
    if coords.is_empty() {
        return Ok(MvarBake::default());
    }

    let os2 = face.table_bytes(*b"OS/2").ok().map(<[u8]>::to_vec);
    let hhea = face.table_bytes(tag::HHEA).ok().map(<[u8]>::to_vec);
    let vhea = face.table_bytes(tag::VHEA).ok().map(<[u8]>::to_vec);
    let post = face.table_bytes(tag::POST).ok().map(<[u8]>::to_vec);

    apply_mvar_records(&mvar, coords, os2, hhea, vhea, post)
}

/// Walks `mvar.entries()` and patches the rebuilt OS/2 / hhea / vhea /
/// post buffers in place. Splits out from [`bake_mvar_metrics`] so the
/// duplicate-tag dedup policy is unit-testable without spinning up a
/// full Face.
pub(super) fn apply_mvar_records(
    mvar: &sigilbuzz::tables::Mvar<'_>,
    coords: &[f32],
    os2: Option<Vec<u8>>,
    hhea: Option<Vec<u8>>,
    vhea: Option<Vec<u8>>,
    post: Option<Vec<u8>>,
) -> Result<MvarBake, SubsetError> {
    let Some(store) = mvar.variation_store() else {
        return Ok(MvarBake {
            os2,
            hhea,
            vhea,
            post,
        });
    };
    let delta = |outer: u16, inner: u16| store.delta(outer, inner, coords);
    apply_mvar_deltas(mvar, &delta, os2, hhea, vhea, post)
}

/// The `OS/2`, `hhea`, `vhea` and `post` of a partial instance, with the
/// fields `MVAR` varies moved to the new default: each takes its row's
/// deltas from the regions on the pinned axes only, which the projected
/// `MVAR` drops (see [`super::ivs::RegionRemap::folded`]). Without an
/// `MVAR` the projection can read, nothing moves (the `MVAR` bake
/// reports and drops a malformed one).
pub(super) fn mvar_defaults(
    face: &Face<'_>,
    coords: &[f32],
    pins: &[AxisPin],
) -> Result<MvarBake, SubsetError> {
    let Some(mvar) = face.mvar().map_err(SubsetError::from)? else {
        return Ok(MvarBake::default());
    };
    let Ok(bytes) = face.table_bytes(tag::MVAR) else {
        return Ok(MvarBake::default());
    };
    let store_off = bytes
        .get(10..)
        .and_then(<[u8]>::first_chunk::<2>)
        .map_or(0, |b| usize::from(u16::from_be_bytes(*b)));
    if store_off == 0 {
        return Ok(MvarBake::default());
    }
    let Some(Ok((_, remap))) = bytes
        .get(store_off..)
        .map(|store| project_ivs_with(store, coords, pins, Projection::MERGED))
    else {
        return Ok(MvarBake::default());
    };
    let os2 = face.table_bytes(*b"OS/2").ok().map(<[u8]>::to_vec);
    let hhea = face.table_bytes(tag::HHEA).ok().map(<[u8]>::to_vec);
    let vhea = face.table_bytes(tag::VHEA).ok().map(<[u8]>::to_vec);
    let post = face.table_bytes(tag::POST).ok().map(<[u8]>::to_vec);
    let delta = |outer: u16, inner: u16| remap.folded(outer, inner);
    apply_mvar_deltas(&mvar, &delta, os2, hhea, vhea, post)
}

/// `hcrs`: hhea caretSlopeRise.
const HORIZ_CARET_RISE: [u8; 4] = *b"hcrs";
/// `hcrn`: hhea caretSlopeRun.
const HORIZ_CARET_RUN: [u8; 4] = *b"hcrn";
/// `hcof`: hhea caretOffset.
const HORIZ_CARET_OFFSET: [u8; 4] = *b"hcof";
/// `vcrs`: vhea caretSlopeRise.
const VERT_CARET_RISE: [u8; 4] = *b"vcrs";
/// `vcrn`: vhea caretSlopeRun.
const VERT_CARET_RUN: [u8; 4] = *b"vcrn";
/// `vcof`: vhea caretOffset.
const VERT_CARET_OFFSET: [u8; 4] = *b"vcof";

/// [`apply_mvar_records`] with each record's delta from `delta`, by its
/// `(outer, inner)` row.
fn apply_mvar_deltas(
    mvar: &sigilbuzz::tables::Mvar<'_>,
    delta: &dyn Fn(u16, u16) -> f32,
    mut os2: Option<Vec<u8>>,
    mut hhea: Option<Vec<u8>>,
    mut vhea: Option<Vec<u8>>,
    mut post: Option<Vec<u8>>,
) -> Result<MvarBake, SubsetError> {
    use sigilbuzz::tables::mvar::tag as mvar_tag;
    // OS/2 v0 is 78 bytes; v1+ goes through 96/100. Field offsets
    // (per OpenType OS/2 spec):
    //   sxHeight        (s i16) at v2+ offset 0x56 (86)
    //   sCapHeight      (s i16) at v2+ offset 0x58 (88)
    //   ySubscriptXSize (s i16) 0x0A (10)
    //   ySubscriptYSize          0x0C (12)
    //   ySubscriptXOffset        0x0E (14)
    //   ySubscriptYOffset        0x10 (16)
    //   ySuperscriptXSize        0x12 (18)
    //   ySuperscriptYSize        0x14 (20)
    //   ySuperscriptXOffset      0x16 (22)
    //   ySuperscriptYOffset      0x18 (24)
    //   yStrikeoutSize           0x1A (26)
    //   yStrikeoutPosition       0x1C (28)
    //   sTypoAscender   (i16)    0x44 (68)
    //   sTypoDescender           0x46 (70)
    //   sTypoLineGap             0x48 (72)
    //   usWinAscent     (u16)    0x4A (74)
    //   usWinDescent             0x4C (76)
    //
    // post: italicAngle is offset 4 (Fixed16.16). underlineThickness
    // and underlinePosition are i16 at offsets 10 and 8 respectively.
    //
    // hhea and vhea share a layout (all i16):
    //   ascent / vertTypoAscender 4 (vhea only: vasc)
    //   descent                   6 (vhea only: vdsc)
    //   lineGap                   8 (vhea only: vlgp)
    //   caretSlopeRise           18 (hcrs, vcrs)
    //   caretSlopeRun            20 (hcrn, vcrn)
    //   caretOffset              22 (hcof, vcof)
    //
    // The gasp tags (gsp0 to gsp9) are left alone, as HarfBuzz's
    // instancer leaves them: gasp passes through unchanged.

    // Per OpenType MVAR spec each tag appears at most once in a
    // well-formed `valueRecords` array. Malformed fonts can ship the
    // same tag twice; without dedup the patch path applies the delta
    // once per record, doubling its effect on the rebuilt OS/2 / hhea
    // / vhea / post fields. Dedup with first-wins so the rebuild
    // matches the spec-conforming case bit-for-bit.
    //
    // Only the tags below patch a field, so every other record is
    // skipped before its delta is evaluated. The first record for a
    // tag carries the `(outer, inner)` pair `Mvar::metric_delta` would
    // look up, so the delta is read from it directly. Both keep the
    // walk linear in the record count.
    let mut seen: BTreeSet<[u8; 4]> = BTreeSet::new();
    for (rec_tag, (outer, inner)) in mvar.entries() {
        let (buf, off, signed) = match rec_tag {
            t if t == mvar_tag::HORIZ_ASCENDER => (&mut os2, 68, true),
            t if t == mvar_tag::HORIZ_DESCENDER => (&mut os2, 70, true),
            t if t == mvar_tag::HORIZ_LINE_GAP => (&mut os2, 72, true),
            t if t == mvar_tag::HORIZ_CLIPPING_ASCENT => (&mut os2, 74, false),
            t if t == mvar_tag::HORIZ_CLIPPING_DESCENT => (&mut os2, 76, false),
            t if t == mvar_tag::X_HEIGHT => (&mut os2, 86, true),
            t if t == mvar_tag::CAP_HEIGHT => (&mut os2, 88, true),
            t if t == mvar_tag::SUBSCRIPT_X_SIZE => (&mut os2, 10, true),
            t if t == mvar_tag::SUBSCRIPT_Y_SIZE => (&mut os2, 12, true),
            t if t == mvar_tag::SUBSCRIPT_X_OFFSET => (&mut os2, 14, true),
            t if t == mvar_tag::SUBSCRIPT_Y_OFFSET => (&mut os2, 16, true),
            t if t == mvar_tag::SUPERSCRIPT_X_SIZE => (&mut os2, 18, true),
            t if t == mvar_tag::SUPERSCRIPT_Y_SIZE => (&mut os2, 20, true),
            t if t == mvar_tag::SUPERSCRIPT_X_OFFSET => (&mut os2, 22, true),
            t if t == mvar_tag::SUPERSCRIPT_Y_OFFSET => (&mut os2, 24, true),
            t if t == mvar_tag::STRIKEOUT_SIZE => (&mut os2, 26, true),
            t if t == mvar_tag::STRIKEOUT_OFFSET => (&mut os2, 28, true),
            t if t == mvar_tag::VERT_ASCENDER => (&mut vhea, 4, true),
            t if t == mvar_tag::VERT_DESCENDER => (&mut vhea, 6, true),
            t if t == mvar_tag::VERT_LINE_GAP => (&mut vhea, 8, true),
            t if t == HORIZ_CARET_RISE => (&mut hhea, 18, true),
            t if t == HORIZ_CARET_RUN => (&mut hhea, 20, true),
            t if t == HORIZ_CARET_OFFSET => (&mut hhea, 22, true),
            t if t == VERT_CARET_RISE => (&mut vhea, 18, true),
            t if t == VERT_CARET_RUN => (&mut vhea, 20, true),
            t if t == VERT_CARET_OFFSET => (&mut vhea, 22, true),
            t if t == mvar_tag::UNDERLINE_SIZE => (&mut post, 10, true),
            t if t == mvar_tag::UNDERLINE_OFFSET => (&mut post, 8, true),
            _ => continue, // unrecognized tag: silently ignore
        };
        if !seen.insert(rec_tag) {
            continue;
        }
        let delta = round_half_up(delta(outer, inner));
        if delta == 0 {
            continue;
        }
        if signed {
            patch_i16(buf, off, delta);
        } else {
            patch_u16(buf, off, delta);
        }
    }

    Ok(MvarBake {
        os2,
        hhea,
        vhea,
        post,
    })
}

/// Returns the two bytes at `buf[off..off + 2]`, or `None` when the
/// table is absent or too short.
fn field_bytes(buf: &mut Option<Vec<u8>>, off: usize) -> Option<&mut [u8; 2]> {
    buf.as_mut()?.get_mut(off..)?.first_chunk_mut::<2>()
}

/// Adds `delta` to the big-endian `i16` at `off`, clamping to the field
/// range. A delta from a long-word variation store can reach
/// `i32::MAX`, so the sum saturates before the clamp.
pub(super) fn patch_i16(buf: &mut Option<Vec<u8>>, off: usize, delta: i32) {
    let Some(field) = field_bytes(buf, off) else {
        return;
    };
    let cur = i16::from_be_bytes(*field);
    let new = i32::from(cur)
        .saturating_add(delta)
        .clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
    *field = new.to_be_bytes();
}

/// Adds `delta` to the big-endian `u16` at `off`, clamping to the field
/// range.
pub(super) fn patch_u16(buf: &mut Option<Vec<u8>>, off: usize, delta: i32) {
    let Some(field) = field_bytes(buf, off) else {
        return;
    };
    let cur = u16::from_be_bytes(*field);
    let new = i32::from(cur)
        .saturating_add(delta)
        .clamp(0, i32::from(u16::MAX)) as u16;
    *field = new.to_be_bytes();
}
