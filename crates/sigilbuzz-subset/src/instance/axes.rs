//! The fvar and avar trims of a partial instance: only the kept axes,
//! their segment maps and the named instances they still tell apart.

use alloc::vec::Vec;

use super::AxisPin;

// ---------------------------------------------------------------------------
// Partial-instancing fvar trim
// ---------------------------------------------------------------------------

/// Re-emits an `fvar` table with every Pin-axis dimension dropped.
///
/// `pins` carries one entry per source axis; only axes whose pin is
/// `AxisPin::Keep` survive. Instance records keep the same flags /
/// nameIDs but drop their Pin-axis coord slots; instances whose
/// surviving coord vector is now identical to the trimmed default-
/// instance vector are removed (they would shadow the implicit default).
///
/// Returns `None` when every axis pins (the all-pin case is the
/// existing full-instancing behavior and the caller drops fvar
/// outright when `drop_var_tables` is true).
#[allow(dead_code)] // wired in by the partial-instancing integration commit
pub(super) fn bake_fvar_partial(fvar_bytes: &[u8], pins: &[AxisPin]) -> Option<Vec<u8>> {
    if pins.iter().all(|p| matches!(p, AxisPin::Pin)) {
        return None;
    }
    if fvar_bytes.len() < 16 {
        return None;
    }
    let major = u16::from_be_bytes([fvar_bytes[0], fvar_bytes[1]]);
    if major != 1 {
        return None;
    }
    let axes_array_off = u16::from_be_bytes([fvar_bytes[4], fvar_bytes[5]]) as usize;
    let axis_count = u16::from_be_bytes([fvar_bytes[8], fvar_bytes[9]]) as usize;
    let axis_size = u16::from_be_bytes([fvar_bytes[10], fvar_bytes[11]]) as usize;
    let instance_count = u16::from_be_bytes([fvar_bytes[12], fvar_bytes[13]]) as usize;
    let instance_size = u16::from_be_bytes([fvar_bytes[14], fvar_bytes[15]]) as usize;
    if axis_size < 20 || pins.len() != axis_count {
        return None;
    }
    let need_axes = axes_array_off.checked_add(axis_count.checked_mul(axis_size)?)?;
    if fvar_bytes.len() < need_axes {
        return None;
    }

    // Surviving axis indices (in source order).
    let kept: Vec<usize> = pins
        .iter()
        .enumerate()
        .filter_map(|(i, p)| matches!(p, AxisPin::Keep).then_some(i))
        .collect();
    let new_axis_count = kept.len();

    // Collect the source's per-axis default values (for instance
    // dedup). Each axis record's defaultValue lives at +8 in the 20-
    // byte axis record (tag[4] + min[4] + default[4]).
    let mut axis_defaults: Vec<u32> = Vec::with_capacity(axis_count);
    for i in 0..axis_count {
        let off = axes_array_off + i * axis_size;
        let raw = u32::from_be_bytes([
            fvar_bytes[off + 8],
            fvar_bytes[off + 9],
            fvar_bytes[off + 10],
            fvar_bytes[off + 11],
        ]);
        axis_defaults.push(raw);
    }

    // Decide the new instanceSize. Fixed-format: 20 (axis records) but
    // for instances it's 4 (subfamilyNameID + flags) + 4 * axisCount
    // + optional 2 (postScriptNameID). We detect "with PS name" by
    // checking source instance_size against 4 + 4 * axis_count.
    let base_inst = 4usize + 4 * axis_count;
    let with_ps = instance_size == base_inst + 2;
    let new_instance_size = if with_ps {
        4usize + 4 * new_axis_count + 2
    } else {
        4usize + 4 * new_axis_count
    };

    // Filter instances: read each, drop Pin-axis slots, then drop the
    // record entirely if its surviving coord vector matches the
    // trimmed default-instance vector.
    let mut new_instance_records: Vec<Vec<u8>> = Vec::with_capacity(instance_count);
    let instances_off = axes_array_off + axis_count * axis_size;
    if instance_count > 0 {
        if instance_size < base_inst {
            return None;
        }
        let need_inst = instances_off.checked_add(instance_count.checked_mul(instance_size)?)?;
        if fvar_bytes.len() < need_inst {
            return None;
        }
        for i in 0..instance_count {
            let off = instances_off + i * instance_size;
            let mut rec = Vec::with_capacity(new_instance_size);
            // subfamilyNameID + flags.
            rec.extend_from_slice(&fvar_bytes[off..off + 4]);
            let mut all_default = true;
            for &k in &kept {
                let coord_off = off + 4 + k * 4;
                let raw = &fvar_bytes[coord_off..coord_off + 4];
                rec.extend_from_slice(raw);
                let raw_u32 = u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]);
                if raw_u32 != axis_defaults[k] {
                    all_default = false;
                }
            }
            if with_ps {
                let ps_off = off + 4 + axis_count * 4;
                rec.extend_from_slice(&fvar_bytes[ps_off..ps_off + 2]);
            }
            // Drop instances that collapse to the default once the Pin
            // axes are removed. Keeping them would create duplicates of
            // the implicit default instance.
            if all_default && new_axis_count > 0 {
                continue;
            }
            new_instance_records.push(rec);
        }
    }

    // Assemble the new fvar.
    let mut out = Vec::with_capacity(
        16 + new_axis_count * 20 + new_instance_records.len() * new_instance_size,
    );
    out.extend_from_slice(&1u16.to_be_bytes()); // major
    out.extend_from_slice(&0u16.to_be_bytes()); // minor
    out.extend_from_slice(&16u16.to_be_bytes()); // axesArrayOffset (header is 16 bytes)
    out.extend_from_slice(&2u16.to_be_bytes()); // reserved
    out.extend_from_slice(&(new_axis_count as u16).to_be_bytes());
    out.extend_from_slice(&20u16.to_be_bytes()); // axisSize
    out.extend_from_slice(&(new_instance_records.len() as u16).to_be_bytes());
    out.extend_from_slice(&(new_instance_size as u16).to_be_bytes());
    for &k in &kept {
        let off = axes_array_off + k * axis_size;
        // Each axis record is 20 bytes; emit verbatim from source.
        out.extend_from_slice(&fvar_bytes[off..off + 20]);
    }
    for rec in &new_instance_records {
        out.extend_from_slice(rec);
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// Partial-instancing avar trim
// ---------------------------------------------------------------------------

/// Re-emits an `avar` table with every Pin-axis segment map dropped.
/// Returns `None` when every axis pins.
#[allow(dead_code)] // wired in by the partial-instancing integration commit
pub(super) fn bake_avar_partial(avar_bytes: &[u8], pins: &[AxisPin]) -> Option<Vec<u8>> {
    if pins.iter().all(|p| matches!(p, AxisPin::Pin)) {
        return None;
    }
    if avar_bytes.len() < 8 {
        return None;
    }
    let major = u16::from_be_bytes([avar_bytes[0], avar_bytes[1]]);
    if major != 1 {
        return None;
    }
    let axis_count = u16::from_be_bytes([avar_bytes[6], avar_bytes[7]]) as usize;
    if pins.len() != axis_count {
        return None;
    }

    // Walk the segment maps, slicing each into its byte range so we
    // can emit the kept ones verbatim.
    let mut cursor = 8usize;
    let mut map_ranges: Vec<(usize, usize)> = Vec::with_capacity(axis_count);
    for _ in 0..axis_count {
        if avar_bytes.len() < cursor + 2 {
            return None;
        }
        let count = u16::from_be_bytes([avar_bytes[cursor], avar_bytes[cursor + 1]]) as usize;
        let start = cursor;
        // Each AxisValueMap is 4 bytes (2 x F2DOT14).
        let map_size = 2 + count * 4;
        if avar_bytes.len() < start + map_size {
            return None;
        }
        cursor = start + map_size;
        map_ranges.push((start, cursor));
    }

    // Count surviving axes.
    let new_axis_count = pins.iter().filter(|p| matches!(p, AxisPin::Keep)).count();

    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // major
    out.extend_from_slice(&0u16.to_be_bytes()); // minor
    out.extend_from_slice(&0u16.to_be_bytes()); // reserved
    out.extend_from_slice(&(new_axis_count as u16).to_be_bytes());
    for (i, &pin) in pins.iter().enumerate() {
        if matches!(pin, AxisPin::Keep) {
            let (s, e) = map_ranges[i];
            out.extend_from_slice(&avar_bytes[s..e]);
        }
    }
    Some(out)
}
