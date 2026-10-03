//! Builders for hand-made `gvar` tables, shared by the `gvar` and
//! `glyf` tests.

use alloc::vec;
use alloc::vec::Vec;

/// One tuple variation of a test glyph, with an embedded peak and its
/// own point numbers.
pub(crate) struct Tuple {
    /// Peak coordinate per axis.
    pub(crate) peak: Vec<f32>,
    /// Listed point numbers, ascending, or `None` for every point (for
    /// the glyph's shared point numbers, in [`build_gvar_shared`]).
    pub(crate) points: Option<Vec<u16>>,
    /// One `(dx, dy)` per listed point, or per point.
    pub(crate) deltas: Vec<(i16, i16)>,
}

/// Builds a `gvar` table with long offsets, one `GlyphVariationData`
/// per entry of `glyphs` (none for an empty entry).
pub(crate) fn build_gvar(axis_count: u16, glyphs: &[Vec<Tuple>]) -> Vec<u8> {
    let glyphs: Vec<(Option<&[u16]>, &[Tuple])> =
        glyphs.iter().map(|t| (None, t.as_slice())).collect();
    build(axis_count, &glyphs)
}

/// [`build_gvar`] with shared point numbers: each glyph's
/// `GlyphVariationData` carries its list of shared point numbers
/// (ascending; empty for every point), and its tuples whose `points` is
/// `None` use them.
pub(crate) fn build_gvar_shared(axis_count: u16, glyphs: &[(Vec<u16>, Vec<Tuple>)]) -> Vec<u8> {
    let glyphs: Vec<(Option<&[u16]>, &[Tuple])> = glyphs
        .iter()
        .map(|(shared, t)| (Some(shared.as_slice()), t.as_slice()))
        .collect();
    build(axis_count, &glyphs)
}

fn build(axis_count: u16, glyphs: &[(Option<&[u16]>, &[Tuple])]) -> Vec<u8> {
    let mut data_array = Vec::new();
    let mut offsets = vec![0u32];
    for &(shared, tuples) in glyphs {
        if !tuples.is_empty() {
            data_array.extend(glyph_variation_data(axis_count, shared, tuples));
        }
        offsets.push(data_array.len() as u32);
    }
    let header_len = (20 + 4 * offsets.len()) as u32;
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // major
    out.extend_from_slice(&0u16.to_be_bytes()); // minor
    out.extend_from_slice(&axis_count.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes()); // shared tuple count
    out.extend_from_slice(&header_len.to_be_bytes()); // shared tuples offset
    out.extend_from_slice(&(glyphs.len() as u16).to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes()); // long offsets
    out.extend_from_slice(&header_len.to_be_bytes()); // data array offset
    for o in offsets {
        out.extend_from_slice(&o.to_be_bytes());
    }
    out.extend(data_array);
    out
}

/// One glyph's variation data. With `shared` point numbers, a tuple
/// whose `points` is `None` stores no point numbers of its own.
fn glyph_variation_data(axis_count: u16, shared: Option<&[u16]>, tuples: &[Tuple]) -> Vec<u8> {
    let mut headers = Vec::new();
    let mut data = Vec::new();
    let mut count = tuples.len() as u16;
    if let Some(shared) = shared {
        count |= 0x8000; // SHARED_POINT_NUMBERS
        packed_points(&mut data, Some(shared));
    }
    for t in tuples {
        assert_eq!(t.peak.len(), usize::from(axis_count));
        let private = shared.is_none() || t.points.is_some();
        let mut serialized = Vec::new();
        if private {
            packed_points(&mut serialized, t.points.as_deref());
        }
        packed_deltas(&mut serialized, t.deltas.iter().map(|d| d.0));
        packed_deltas(&mut serialized, t.deltas.iter().map(|d| d.1));
        headers.extend_from_slice(&(serialized.len() as u16).to_be_bytes());
        // Embedded peak, and private point numbers when the tuple has
        // its own.
        let flags: u16 = if private { 0xA000 } else { 0x8000 };
        headers.extend_from_slice(&flags.to_be_bytes());
        for &p in &t.peak {
            headers.extend_from_slice(&((p * 16384.0) as i16).to_be_bytes());
        }
        data.extend(serialized);
    }
    let mut out = Vec::new();
    out.extend_from_slice(&count.to_be_bytes());
    out.extend_from_slice(&((4 + headers.len()) as u16).to_be_bytes());
    out.extend(headers);
    out.extend(data);
    out
}

/// Packed point numbers, in runs of 16-bit increments; an empty or
/// missing list means every point.
fn packed_points(out: &mut Vec<u8>, points: Option<&[u16]>) {
    let Some(points) = points.filter(|p| !p.is_empty()) else {
        out.push(0); // every point
        return;
    };
    let n = points.len();
    if n < 128 {
        out.push(n as u8);
    } else {
        out.push(0x80 | (n >> 8) as u8);
        out.push(n as u8);
    }
    let mut last = 0u16;
    for run in points.chunks(128) {
        out.push(0x80 | (run.len() - 1) as u8);
        for &p in run {
            out.extend_from_slice(&(p - last).to_be_bytes());
            last = p;
        }
    }
}

/// Packed deltas, in runs of 16-bit values.
fn packed_deltas(out: &mut Vec<u8>, deltas: impl Iterator<Item = i16>) {
    let deltas: Vec<i16> = deltas.collect();
    for run in deltas.chunks(64) {
        out.push(0x40 | (run.len() - 1) as u8);
        for &d in run {
            out.extend_from_slice(&d.to_be_bytes());
        }
    }
}
