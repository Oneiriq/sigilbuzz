//! GPOS variation fold tests: every device offset cleared, and the
//! PairPos deltas of the var_kern fixture folded at the instance.

use super::*;

/// Walks every GPOS lookup and returns true if any ValueRecord
/// or Anchor (format 3) device offset slot is non-zero. Used by
/// the post-bake assertions to confirm no orphan VariationIndex
/// offsets survived the fold. Covers SinglePos / PairPos formats
/// 1 and 2, CursivePos, Mark{Base,Lig,Mark}Pos, and Type 9
/// Extension wrappers around any of the above, the same set we
/// explicitly bake.
fn any_value_record_device_offset_nonzero(face: &Face<'_>) -> bool {
    let Ok(Some(gpos)) = face.gpos() else {
        return false;
    };
    let lookups = gpos.lookup_list();
    for li in 0..lookups.len() {
        let Some(lookup) = lookups.get(li) else {
            continue;
        };
        let lt = lookup.lookup_type();
        for si in 0..lookup.subtable_count() {
            let Some(sub) = lookup.subtable_bytes(si) else {
                continue;
            };
            let (effective_lt, effective_sub) = if lt == 9 {
                if sub.len() < 8 {
                    continue;
                }
                let ext_type = u16::from_be_bytes([sub[2], sub[3]]);
                let ext_off = u32::from_be_bytes([sub[4], sub[5], sub[6], sub[7]]) as usize;
                let Some(inner) = sub.get(ext_off..) else {
                    continue;
                };
                (ext_type, inner)
            } else {
                (lt, sub)
            };
            if check_subtable_for_device_offsets(effective_lt, effective_sub) {
                return true;
            }
        }
    }
    false
}

fn check_subtable_for_device_offsets(lt: u16, sub: &[u8]) -> bool {
    match lt {
        1 => {
            // SinglePos.
            if sub.len() < 6 {
                return false;
            }
            let format = u16::from_be_bytes([sub[0], sub[1]]);
            let value_format = u16::from_be_bytes([sub[4], sub[5]]);
            if value_format & 0x00F0 == 0 {
                return false;
            }
            let stride = (value_format & 0x00FF).count_ones() as usize * 2;
            let value_count = if format == 2 {
                u16::from_be_bytes([sub[6], sub[7]]) as usize
            } else {
                1
            };
            let header_len = if format == 2 { 8 } else { 6 };
            for i in 0..value_count {
                let vr = header_len + i * stride;
                if vr_has_nonzero_device_offset(&sub[vr..vr + stride], value_format) {
                    return true;
                }
            }
            false
        }
        2 => {
            // PairPos.
            if sub.len() < 4 {
                return false;
            }
            let format = u16::from_be_bytes([sub[0], sub[1]]);
            let vf1 = u16::from_be_bytes([sub[4], sub[5]]);
            let vf2 = u16::from_be_bytes([sub[6], sub[7]]);
            let v1 = (vf1 & 0x00FF).count_ones() as usize * 2;
            let v2 = (vf2 & 0x00FF).count_ones() as usize * 2;
            if (vf1 | vf2) & 0x00F0 == 0 {
                return false;
            }
            if format == 1 {
                let pair_set_count = u16::from_be_bytes([sub[8], sub[9]]) as usize;
                let pvr_size = 2 + v1 + v2;
                for i in 0..pair_set_count {
                    let off_off = 10 + i * 2;
                    if off_off + 2 > sub.len() {
                        continue;
                    }
                    let set_off = u16::from_be_bytes([sub[off_off], sub[off_off + 1]]) as usize;
                    if set_off + 2 > sub.len() {
                        continue;
                    }
                    let pair_value_count =
                        u16::from_be_bytes([sub[set_off], sub[set_off + 1]]) as usize;
                    for j in 0..pair_value_count {
                        let pvr = set_off + 2 + j * pvr_size;
                        if pvr + pvr_size > sub.len() {
                            continue;
                        }
                        if vr_has_nonzero_device_offset(&sub[pvr + 2..pvr + 2 + v1], vf1)
                            || vr_has_nonzero_device_offset(
                                &sub[pvr + 2 + v1..pvr + 2 + v1 + v2],
                                vf2,
                            )
                        {
                            return true;
                        }
                    }
                }
                false
            } else if format == 2 {
                if sub.len() < 16 {
                    return false;
                }
                let class1 = u16::from_be_bytes([sub[12], sub[13]]) as usize;
                let class2 = u16::from_be_bytes([sub[14], sub[15]]) as usize;
                let cell = v1 + v2;
                let row = class2 * cell;
                for i in 0..class1 {
                    for j in 0..class2 {
                        let off = 16 + i * row + j * cell;
                        if off + cell > sub.len() {
                            continue;
                        }
                        if vr_has_nonzero_device_offset(&sub[off..off + v1], vf1)
                            || vr_has_nonzero_device_offset(&sub[off + v1..off + cell], vf2)
                        {
                            return true;
                        }
                    }
                }
                false
            } else {
                false
            }
        }
        // CursivePos.
        3 => {
            if sub.len() < 6 {
                return false;
            }
            let format = u16::from_be_bytes([sub[0], sub[1]]);
            if format != 1 {
                return false;
            }
            let count = u16::from_be_bytes([sub[4], sub[5]]) as usize;
            let recs = 6usize;
            if recs + count * 4 > sub.len() {
                return false;
            }
            for i in 0..count {
                let r = recs + i * 4;
                let entry = u16::from_be_bytes([sub[r], sub[r + 1]]) as usize;
                let exit = u16::from_be_bytes([sub[r + 2], sub[r + 3]]) as usize;
                if anchor_has_nonzero_device_offset(sub, entry)
                    || anchor_has_nonzero_device_offset(sub, exit)
                {
                    return true;
                }
            }
            false
        }
        // MarkBasePos / MarkMarkPos: same shape (mark + base/mark2 array).
        4 | 6 => mark_pair_has_nonzero_device_offset(sub),
        // MarkLigPos.
        5 => mark_lig_has_nonzero_device_offset(sub),
        _ => false,
    }
}

/// Returns true when the Anchor at `anchor_off` (relative to
/// `subtable_buf`) is AnchorFormat 3 with a non-zero xDevice or
/// yDevice slot. Format 1 / 2 have no device slots; an anchor_off
/// of 0 (the spec's "absent" sentinel) returns false.
fn anchor_has_nonzero_device_offset(subtable_buf: &[u8], anchor_off: usize) -> bool {
    if anchor_off == 0 || anchor_off + 10 > subtable_buf.len() {
        return false;
    }
    let format = u16::from_be_bytes([subtable_buf[anchor_off], subtable_buf[anchor_off + 1]]);
    if format != 3 {
        return false;
    }
    let x_dev = u16::from_be_bytes([subtable_buf[anchor_off + 6], subtable_buf[anchor_off + 7]]);
    let y_dev = u16::from_be_bytes([subtable_buf[anchor_off + 8], subtable_buf[anchor_off + 9]]);
    x_dev != 0 || y_dev != 0
}

/// Walks the MarkArray + BaseArray / Mark2Array of a MarkBasePos /
/// MarkMarkPos subtable. Returns true if any anchor has a surviving
/// device offset.
fn mark_pair_has_nonzero_device_offset(sub: &[u8]) -> bool {
    if sub.len() < 12 {
        return false;
    }
    let format = u16::from_be_bytes([sub[0], sub[1]]);
    if format != 1 {
        return false;
    }
    let mark_class_count = u16::from_be_bytes([sub[6], sub[7]]) as usize;
    let mark_array_off = u16::from_be_bytes([sub[8], sub[9]]) as usize;
    let other_array_off = u16::from_be_bytes([sub[10], sub[11]]) as usize;
    if mark_array_check(sub, mark_array_off) {
        return true;
    }
    if other_array_off + 2 > sub.len() {
        return false;
    }
    let count = u16::from_be_bytes([sub[other_array_off], sub[other_array_off + 1]]) as usize;
    let recs = other_array_off + 2;
    let total = count * mark_class_count;
    if recs + total * 2 > sub.len() {
        return false;
    }
    for i in 0..total {
        let pos = recs + i * 2;
        let rel = u16::from_be_bytes([sub[pos], sub[pos + 1]]) as usize;
        if rel != 0 && anchor_has_nonzero_device_offset(sub, other_array_off + rel) {
            return true;
        }
    }
    false
}

fn mark_array_check(sub: &[u8], mark_array_off: usize) -> bool {
    if mark_array_off + 2 > sub.len() {
        return false;
    }
    let count = u16::from_be_bytes([sub[mark_array_off], sub[mark_array_off + 1]]) as usize;
    let recs = mark_array_off + 2;
    if recs + count * 4 > sub.len() {
        return false;
    }
    for i in 0..count {
        let pos = recs + i * 4;
        let rel = u16::from_be_bytes([sub[pos + 2], sub[pos + 3]]) as usize;
        if rel != 0 && anchor_has_nonzero_device_offset(sub, mark_array_off + rel) {
            return true;
        }
    }
    false
}

/// Walks the MarkArray + LigatureArray of a MarkLigPos subtable.
/// Returns true if any anchor has a surviving device offset.
fn mark_lig_has_nonzero_device_offset(sub: &[u8]) -> bool {
    if sub.len() < 12 {
        return false;
    }
    let format = u16::from_be_bytes([sub[0], sub[1]]);
    if format != 1 {
        return false;
    }
    let mark_class_count = u16::from_be_bytes([sub[6], sub[7]]) as usize;
    let mark_array_off = u16::from_be_bytes([sub[8], sub[9]]) as usize;
    let lig_array_off = u16::from_be_bytes([sub[10], sub[11]]) as usize;
    if mark_array_check(sub, mark_array_off) {
        return true;
    }
    if lig_array_off + 2 > sub.len() {
        return false;
    }
    let lig_count = u16::from_be_bytes([sub[lig_array_off], sub[lig_array_off + 1]]) as usize;
    let lig_attach_offs = lig_array_off + 2;
    if lig_attach_offs + lig_count * 2 > sub.len() {
        return false;
    }
    for i in 0..lig_count {
        let pos = lig_attach_offs + i * 2;
        let rel = u16::from_be_bytes([sub[pos], sub[pos + 1]]) as usize;
        if rel == 0 {
            continue;
        }
        let la_off = lig_array_off + rel;
        if la_off + 2 > sub.len() {
            continue;
        }
        let comp_count = u16::from_be_bytes([sub[la_off], sub[la_off + 1]]) as usize;
        let comps_off = la_off + 2;
        let row = mark_class_count * 2;
        if comps_off + comp_count * row > sub.len() {
            continue;
        }
        for c in 0..comp_count {
            for k in 0..mark_class_count {
                let p = comps_off + c * row + k * 2;
                let arel = u16::from_be_bytes([sub[p], sub[p + 1]]) as usize;
                if arel != 0 && anchor_has_nonzero_device_offset(sub, la_off + arel) {
                    return true;
                }
            }
        }
    }
    false
}

fn vr_has_nonzero_device_offset(vr: &[u8], format: u16) -> bool {
    // Skip the four static i16 fields (each present iff its bit
    // is set) and inspect the four device-offset slots.
    let mut cursor = 0usize;
    for bit in [0x0001u16, 0x0002, 0x0004, 0x0008] {
        if format & bit != 0 {
            cursor += 2;
        }
    }
    for bit in [0x0010u16, 0x0020, 0x0040, 0x0080] {
        if format & bit != 0 {
            if cursor + 2 > vr.len() {
                return false;
            }
            let off = u16::from_be_bytes([vr[cursor], vr[cursor + 1]]);
            if off != 0 {
                return true;
            }
            cursor += 2;
        }
    }
    false
}

/// `var_kern.ttf` measures its PairValueRecord device offset from
/// the PairSet, as the spec says (the base the bake uses). Its
/// earlier Python-built version measured from the PairPos subtable
/// and needed rebasing here; the Rust-built fixture does not.
fn var_kern_with_pair_set_relative_device() -> Vec<u8> {
    VAR_KERN.to_vec()
}

#[test]
fn var_kern_fixture_bake_at_wght_900_folds_pair_pos_advance() {
    // The synthetic var_kern fixture carries a single PairPos
    // format 1 lookup. At wght=900 the source GPOS has x_advance=0
    // on the AV pair plus a VariationIndex that resolves to -100.
    // After the bake, the baked GPOS must carry x_advance=-100
    // statically and the device offset slot must be zero.
    let bytes = var_kern_with_pair_set_relative_device();
    let face = Face::parse_bytes(&bytes, 0).unwrap();
    let coords = face
        .fvar()
        .unwrap()
        .unwrap()
        .axes()
        .iter()
        .enumerate()
        .map(|(i, a)| a.normalize([900.0].get(i).copied().unwrap_or(a.default_value)))
        .collect::<Vec<f32>>();
    let input = InstanceInput {
        coords: coords.clone(),
        drop_var_tables: true,
        axis_pins: Vec::new(),
    };
    let out = instance(&face, &input).expect("bake");
    let baked = Face::parse_bytes(&out.bytes, 0).expect("baked face parses");
    // Confirm no GPOS device offset survived the fold.
    assert!(
        !any_value_record_device_offset_nonzero(&baked),
        "baked GPOS must have zero device offsets"
    );
    // Confirm GDEF.IVS was pruned.
    if let Some(gdef) = baked.gdef().unwrap() {
        assert!(
            gdef.item_variation_store().is_none(),
            "baked GDEF.IVS must be pruned"
        );
    }
    // Confirm the static field carries the resolved delta. Walk
    // GPOS by hand to read the AV pair's value.
    let gpos_bytes = baked.table_bytes(tag::GPOS).expect("baked GPOS");
    let lookup_list_off = u16::from_be_bytes([gpos_bytes[8], gpos_bytes[9]]) as usize;
    let lookup_off = u16::from_be_bytes([
        gpos_bytes[lookup_list_off + 2],
        gpos_bytes[lookup_list_off + 3],
    ]) as usize;
    let lookup_base = lookup_list_off + lookup_off;
    let sub_off =
        u16::from_be_bytes([gpos_bytes[lookup_base + 6], gpos_bytes[lookup_base + 7]]) as usize;
    let sub_abs = lookup_base + sub_off;
    let sub = &gpos_bytes[sub_abs..];
    // PairPos fmt 1: first PairSet at the first set offset.
    let pair_set_rel = u16::from_be_bytes([sub[10], sub[11]]) as usize;
    // PairValueRecord 0 starts at +2 inside the PairSet, AV pair
    // bytes are: u16 secondGlyph (V), i16 x_advance, u16 device.
    let pvr_off = pair_set_rel + 2;
    let x_advance = i16::from_be_bytes([sub[pvr_off + 2], sub[pvr_off + 3]]);
    assert_eq!(x_advance, -100, "AV x_advance baked at wght=900");
}

#[test]
fn var_kern_fixture_bake_at_default_coords_leaves_static_field_at_source() {
    // At wght=400 the variation region peaks at zero scalar ->
    // delta is zero. The static x_advance must stay at the
    // source's 0 and the device offset must still be zeroed (the
    // bake unconditionally severs the offset to keep GDEF.IVS
    // safe to drop).
    let face = Face::parse_bytes(VAR_KERN, 0).unwrap();
    let coords = face
        .fvar()
        .unwrap()
        .unwrap()
        .axes()
        .iter()
        .enumerate()
        .map(|(i, a)| a.normalize([400.0].get(i).copied().unwrap_or(a.default_value)))
        .collect::<Vec<f32>>();
    let input = InstanceInput {
        coords,
        drop_var_tables: true,
        axis_pins: Vec::new(),
    };
    let out = instance(&face, &input).expect("bake");
    let baked = Face::parse_bytes(&out.bytes, 0).expect("baked face parses");
    assert!(!any_value_record_device_offset_nonzero(&baked));
    let gpos_bytes = baked.table_bytes(tag::GPOS).expect("baked GPOS");
    let lookup_list_off = u16::from_be_bytes([gpos_bytes[8], gpos_bytes[9]]) as usize;
    let lookup_off = u16::from_be_bytes([
        gpos_bytes[lookup_list_off + 2],
        gpos_bytes[lookup_list_off + 3],
    ]) as usize;
    let lookup_base = lookup_list_off + lookup_off;
    let sub_off =
        u16::from_be_bytes([gpos_bytes[lookup_base + 6], gpos_bytes[lookup_base + 7]]) as usize;
    let sub_abs = lookup_base + sub_off;
    let sub = &gpos_bytes[sub_abs..];
    let pair_set_rel = u16::from_be_bytes([sub[10], sub[11]]) as usize;
    let pvr_off = pair_set_rel + 2;
    let x_advance = i16::from_be_bytes([sub[pvr_off + 2], sub[pvr_off + 3]]);
    assert_eq!(x_advance, 0, "AV x_advance unchanged at default wght");
}

#[test]
fn source_sans_vf_subset_bake_clears_all_gpos_variation_offsets() {
    // Source Sans 3 VF Latin Subset is the real-world fixture #173
    // already covered with the IVS-prune path. After the variation
    // fold, no PairPos / SinglePos ValueRecord must carry a
    // surviving device offset, and GDEF.IVS must be pruned.
    let face = Face::parse_bytes(SOURCE_SANS, 0).unwrap();
    let fvar = face.fvar().unwrap().unwrap();
    let mut user = alloc::vec![0.0_f32; fvar.axes().len()];
    if let Some(idx) = fvar.axis_index(*b"wght") {
        user[idx] = fvar.axes()[idx].max_value;
    }
    let coords = fvar
        .axes()
        .iter()
        .enumerate()
        .map(|(i, a)| a.normalize(user.get(i).copied().unwrap_or(a.default_value)))
        .collect::<Vec<f32>>();
    let input = InstanceInput {
        coords,
        drop_var_tables: true,
        axis_pins: Vec::new(),
    };
    let out = instance(&face, &input).expect("bake");
    let baked = Face::parse_bytes(&out.bytes, 0).expect("baked face parses");
    if let Some(gdef) = baked.gdef().unwrap() {
        assert!(
            gdef.item_variation_store().is_none(),
            "baked GDEF.IVS must be pruned"
        );
    }
    assert!(
        !any_value_record_device_offset_nonzero(&baked),
        "baked GPOS must have no surviving device offsets on PairPos/SinglePos"
    );
}
