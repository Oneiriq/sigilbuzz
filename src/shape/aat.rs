//! AAT glue: the `morx` substitution pass for fonts without GSUB and
//! the `kerx` format 4 attachment resolution.

use alloc::vec::Vec;

use crate::buffer::Glyph;
use crate::error::Result;
use crate::face::Face;
use crate::tables::{Kerx, Morx};

/// AAT `morx` substitution pass: runs only when the font has no
/// GSUB. The morx parser returns a new glyph id stream plus an
/// origin vector; each output index carries the input index it was
/// derived from (or the smallest input index for a ligature). We
/// rebuild the `Glyph` vector by copying metadata from that origin
/// so clusters survive ligation: the surviving glyph inherits the
/// first component's cluster, matching HarfBuzz's "merge clusters
/// to earliest" policy.
///
/// Returns `None` when the stream did not change. Otherwise returns
/// one origin per output glyph (`usize::MAX` for a glyph with no
/// single origin) so the caller can remap its segment ranges.
pub(super) fn apply_morx(morx: &Morx<'_>, glyphs: &mut Vec<Glyph>) -> Option<Vec<usize>> {
    if glyphs.is_empty() {
        return None;
    }
    let input_ids: Vec<u16> = glyphs.iter().map(|g| g.glyph_id as u16).collect();
    let (out_ids, origins) = morx.apply(&input_ids);
    if out_ids.len() == glyphs.len() && out_ids == input_ids {
        return None; // no change: avoid needless allocation
    }
    let mut rebuilt: Vec<Glyph> = Vec::with_capacity(out_ids.len());
    let mut out_origins: Vec<usize> = Vec::with_capacity(out_ids.len());
    for (out_idx, &gid) in out_ids.iter().enumerate() {
        let origin = origins.get(out_idx).copied().unwrap_or(usize::MAX);
        if let Some(src) = glyphs.get(origin) {
            let mut g = *src;
            g.glyph_id = u32::from(gid);
            rebuilt.push(g);
            out_origins.push(origin);
        } else {
            // Synthesized output with no single origin: rare; fall
            // back to the lowest available cluster so layout does
            // not confuse renderer-side grapheme tracking.
            let cluster = glyphs.first().map_or(0, |g| g.cluster);
            rebuilt.push(Glyph::new(u32::from(gid), cluster));
            out_origins.push(usize::MAX);
        }
    }
    *glyphs = rebuilt;
    Some(out_origins)
}

/// Resolves every fmt-4 event the state machine emits across `glyphs`
/// and applies the resulting offset to the current glyph.
///
/// Three event types appear:
///
/// - **Coordinates** (action type 2): inline FUnit deltas; resolves
///   directly to `(mark - current)` without external lookups.
/// - **ControlPoints** (action type 0): pairs of glyf-point indices.
///   [`Face::glyph_points`] returns the points in glyf-natural order
///   (contour points + 4 phantoms); the offset comes from
///   `mark[mpi] - current[cpi]`.
/// - **AnchorPoints** (action type 1): pairs of `ankr` indices.
///   [`crate::tables::Ankr::anchor_for`] maps `(gid, idx)` to
///   `(x, y)`; same `mark - current` math.
///
/// Per #166's contract, an out-of-range index, a missing `ankr`
/// table, or a CFF-only font (no glyf) drops the kern silently,
/// matching the type-2 path's conservative posture for malformed
/// records. Errors only bubble when a *parsed* table turns out to
/// be malformed mid-walk.
pub(super) fn apply_kerx_format4(
    face: &Face<'_>,
    kerx: &Kerx<'_>,
    glyphs: &mut [Glyph],
) -> Result<()> {
    let ids: alloc::vec::Vec<u16> = glyphs.iter().map(|g| g.glyph_id as u16).collect();
    let ankr = face.ankr()?;
    // Cache `Face::glyph_points` lookups across events. A single run
    // can fire the same glyph as the mark or current many times: a
    // long "ABABAB" pattern would otherwise re-flatten A twice per
    // pair. `None` (no points / no glyf / out of range) is a real
    // result and worth caching too.
    let mut points_cache: alloc::collections::BTreeMap<u16, Option<alloc::vec::Vec<(i16, i16)>>> =
        alloc::collections::BTreeMap::new();
    let mut events: alloc::vec::Vec<crate::tables::kerx::Kerx4Action> = alloc::vec::Vec::new();
    kerx.apply_format4(&ids, |evt| events.push(evt));
    for evt in events {
        let Some((current_index, dx, dy)) =
            resolve_kerx4_event(evt, face, glyphs, ankr.as_ref(), &mut points_cache)?
        else {
            continue;
        };
        if let Some(g) = glyphs.get_mut(current_index) {
            g.x_offset = g.x_offset.saturating_add(dx);
            g.y_offset = g.y_offset.saturating_add(dy);
        }
    }
    Ok(())
}

/// Resolves one [`Kerx4Action`] event to `(current_glyph_index, dx,
/// dy)`. Returns `Ok(None)` when the event references an unavailable
/// glyph point / anchor: silent drop, mirroring the type-2 path's
/// behavior for malformed records.
fn resolve_kerx4_event(
    evt: crate::tables::kerx::Kerx4Action,
    face: &Face<'_>,
    glyphs: &[Glyph],
    ankr: Option<&crate::tables::Ankr<'_>>,
    points_cache: &mut alloc::collections::BTreeMap<u16, Option<alloc::vec::Vec<(i16, i16)>>>,
) -> Result<Option<(usize, i32, i32)>> {
    use crate::tables::kerx::Kerx4Action;
    match evt {
        Kerx4Action::Coordinates {
            mark_index: _,
            current_index,
            mark_x,
            mark_y,
            current_x,
            current_y,
        } => {
            let dx = i32::from(mark_x) - i32::from(current_x);
            let dy = i32::from(mark_y) - i32::from(current_y);
            Ok(Some((current_index, dx, dy)))
        }
        Kerx4Action::ControlPoints {
            mark_index,
            current_index,
            mark_point,
            current_point,
        } => {
            let mark_gid = glyphs.get(mark_index).map(|g| g.glyph_id as u16);
            let cur_gid = glyphs.get(current_index).map(|g| g.glyph_id as u16);
            let (Some(mark_gid), Some(cur_gid)) = (mark_gid, cur_gid) else {
                return Ok(None);
            };
            let mark_pts = cached_glyph_points(face, mark_gid, points_cache)?;
            let cur_pts = cached_glyph_points(face, cur_gid, points_cache)?;
            let (Some(mark_pts), Some(cur_pts)) = (mark_pts, cur_pts) else {
                return Ok(None);
            };
            let Some(&(mx, my)) = mark_pts.get(mark_point as usize) else {
                return Ok(None);
            };
            let Some(&(cx, cy)) = cur_pts.get(current_point as usize) else {
                return Ok(None);
            };
            Ok(Some((
                current_index,
                i32::from(mx) - i32::from(cx),
                i32::from(my) - i32::from(cy),
            )))
        }
        Kerx4Action::AnchorPoints {
            mark_index,
            current_index,
            mark_anchor,
            current_anchor,
        } => {
            let Some(ankr) = ankr else { return Ok(None) };
            let mark_gid = glyphs.get(mark_index).map(|g| g.glyph_id as u16);
            let cur_gid = glyphs.get(current_index).map(|g| g.glyph_id as u16);
            let (Some(mark_gid), Some(cur_gid)) = (mark_gid, cur_gid) else {
                return Ok(None);
            };
            let Some((mx, my)) = ankr.anchor_for(mark_gid, mark_anchor) else {
                return Ok(None);
            };
            let Some((cx, cy)) = ankr.anchor_for(cur_gid, current_anchor) else {
                return Ok(None);
            };
            Ok(Some((
                current_index,
                i32::from(mx) - i32::from(cx),
                i32::from(my) - i32::from(cy),
            )))
        }
    }
}

/// Reads `Face::glyph_points(gid)` once per `gid`, memoizing the
/// result. Cloning the cached Vec is cheaper than re-flattening a
/// composite glyph; callers that want the raw slice should refactor
/// the caller chain to take a reference, but the current cache shape
/// keeps `apply_kerx_format4` short and obvious.
fn cached_glyph_points(
    face: &Face<'_>,
    gid: u16,
    cache: &mut alloc::collections::BTreeMap<u16, Option<alloc::vec::Vec<(i16, i16)>>>,
) -> Result<Option<alloc::vec::Vec<(i16, i16)>>> {
    if let Some(v) = cache.get(&gid) {
        return Ok(v.clone());
    }
    let v = face.glyph_points(gid)?;
    cache.insert(gid, v.clone());
    Ok(v)
}
