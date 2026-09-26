//! The FeatureList and ScriptList rewrites that close the drop cascade:
//! features that name no surviving lookup go, then language systems
//! and scripts whose features all went.

use alloc::vec::Vec;

use sigilbuzz::tables::layout::FeatureList;

use super::bytes::{read_tag, read_u16};
use super::GidMap;
use crate::offset16::Offset16Guard;
use crate::warnings::Diag;
use crate::SubsetError;

pub(super) struct RewrittenFeatures {
    pub(super) bytes: Vec<u8>,
    /// Old feature index -> new feature index (or None if dropped).
    pub(super) feature_renumber: Vec<Option<u16>>,
}

/// Rewrites the FeatureList. Drops any feature whose lookup-index list
/// becomes empty after the lookup renumber, unless `live_alternates`
/// marks it (a FeatureVariations alternate still gives it a lookup).
/// Returns the new bytes plus a feature-index renumber map.
///
/// A feature whose table cannot be read is dropped and reported
/// through `diag` at its FeatureRecord. `list_at` is where the
/// FeatureList starts in the table. Returns `Ok(None)` once the work
/// budget in `map` runs out.
pub(super) fn rewrite_features(
    feature_list: FeatureList<'_>,
    lookup_renumber: &[Option<u16>],
    live_alternates: &[bool],
    diag: &Diag<'_>,
    list_at: usize,
    map: &GidMap,
) -> Result<Option<RewrittenFeatures>, SubsetError> {
    let offsets = Offset16Guard::default();
    let mut surviving: Vec<([u8; 4], Vec<u16>)> = Vec::new();
    let mut feature_renumber: Vec<Option<u16>> = Vec::with_capacity(feature_list.len() as usize);
    for fi in 0..feature_list.len() {
        let Some((tag, feature)) = feature_list.get(fi) else {
            diag.at(
                list_at + 2 + usize::from(fi) * 6,
                "feature offset past the end, or feature table truncated",
                "a feature",
            );
            feature_renumber.push(None);
            continue;
        };
        if !map.spend(1 + usize::from(feature.len())) {
            return Ok(None);
        }
        let new_indices: Vec<u16> = feature
            .lookup_indices()
            .filter_map(|li| lookup_renumber.get(li as usize).copied().flatten())
            .collect();
        let live = live_alternates
            .get(usize::from(fi))
            .copied()
            .unwrap_or(false);
        if new_indices.is_empty() && !live {
            feature_renumber.push(None);
        } else {
            feature_renumber.push(Some(surviving.len() as u16));
            surviving.push((tag, new_indices));
        }
    }

    // Encode FeatureList:
    //   u16 featureCount
    //   FeatureRecord records[featureCount]: { tag(4) + Offset16 }
    //   Feature[] bodies
    let mut out = Vec::new();
    out.extend_from_slice(&(surviving.len() as u16).to_be_bytes());
    let records_start = out.len();
    for _ in 0..surviving.len() {
        out.extend_from_slice(&[0u8; 6]); // placeholder: tag + offset
    }
    for (i, (tag, indices)) in surviving.iter().enumerate() {
        let body_start = out.len();
        out.extend_from_slice(&0u16.to_be_bytes()); // featureParamsOffset
        out.extend_from_slice(&(indices.len() as u16).to_be_bytes());
        for idx in indices {
            out.extend_from_slice(&idx.to_be_bytes());
        }
        let rec_off = records_start + i * 6;
        out[rec_off..rec_off + 4].copy_from_slice(tag);
        let body_off = offsets.narrow(body_start);
        out[rec_off + 4..rec_off + 6].copy_from_slice(&body_off.to_be_bytes());
    }
    offsets.check("FeatureList rewrite: an offset exceeds 64 KiB")?;

    Ok(Some(RewrittenFeatures {
        bytes: out,
        feature_renumber,
    }))
}

/// Rewrites the ScriptList by walking the raw bytes (the parser
/// doesn't expose enumeration of named LangSys records, only binary
/// search by tag). Drops any LangSys whose feature indices all
/// dropped, drops any Script with no surviving default LangSys + no
/// surviving named LangSys, and returns the new bytes when at least
/// one script survives.
///
/// A script or language system that cannot be read is dropped and
/// reported through `diag`. `bytes` is the ScriptList, a sub-slice of
/// the table `diag` reports against. The walk charges the work budget
/// in `map` and returns `Ok(None)` once it runs out.
pub(super) fn rewrite_scripts_from_bytes(
    bytes: &[u8],
    feature_renumber: &[Option<u16>],
    diag: &Diag<'_>,
    map: &GidMap,
) -> Result<Option<Vec<u8>>, SubsetError> {
    // ScriptList:
    //   u16 scriptCount
    //   ScriptRecord records[scriptCount]: { tag(4) + Offset16 (relative to ScriptList start) }
    let script_count = match bytes.get(0..2) {
        Some(b) => usize::from(u16::from_be_bytes([b[0], b[1]])),
        None => {
            diag.in_part(bytes, 0, "ScriptList truncated", "the whole table");
            return Ok(None);
        }
    };
    if bytes.len() < 2 + script_count * 6 {
        diag.in_part(
            bytes,
            2,
            "ScriptList records shorter than scriptCount",
            "the whole table",
        );
        return Ok(None);
    }

    type ScriptEntry = (
        [u8; 4],
        Option<RewrittenLangSys>,
        Vec<([u8; 4], RewrittenLangSys)>,
    );
    let mut surviving_scripts: Vec<ScriptEntry> = Vec::new();

    for i in 0..script_count {
        let rec_off = 2 + i * 6;
        let (Some(tag), Some(script_off)) =
            (read_tag(bytes, rec_off), read_u16(bytes, rec_off + 4))
        else {
            continue;
        };
        let script_off = usize::from(script_off);
        let Some(script_body) = bytes.get(script_off..).filter(|b| b.len() >= 4) else {
            diag.in_part(
                bytes,
                rec_off + 4,
                "script offset past the end, or script table truncated",
                "a script",
            );
            continue;
        };
        // Script:
        //   Offset16 defaultLangSysOffset (Script-relative; 0 means none)
        //   u16      langSysCount
        //   LangSysRecord records[langSysCount]: { tag(4) + Offset16 (Script-relative) }
        let (Some(default_off), Some(langsys_count)) =
            (read_u16(script_body, 0), read_u16(script_body, 2))
        else {
            continue;
        };
        let default_off = usize::from(default_off);
        let langsys_count = usize::from(langsys_count);
        if !map.spend(1 + langsys_count) {
            return Ok(None);
        }
        let langsys_records_off = 4;
        let langsys_records_end = langsys_records_off + langsys_count * 6;
        if script_body.len() < langsys_records_end {
            diag.in_part(
                script_body,
                2,
                "LangSysRecords shorter than langSysCount",
                "a script",
            );
            continue;
        }

        let default = if default_off != 0 {
            read_langsys(script_body, 0, default_off, feature_renumber, diag, map)
        } else {
            None
        };

        let mut langsystems: Vec<([u8; 4], RewrittenLangSys)> = Vec::new();
        for j in 0..langsys_count {
            let lr = langsys_records_off + j * 6;
            let (Some(ls_tag), Some(ls_off)) =
                (read_tag(script_body, lr), read_u16(script_body, lr + 4))
            else {
                continue;
            };
            let ls_off = usize::from(ls_off);
            if let Some(rls) =
                read_langsys(script_body, lr + 4, ls_off, feature_renumber, diag, map)
            {
                langsystems.push((ls_tag, rls));
            }
        }
        if map.budget_spent() {
            return Ok(None);
        }

        if default.is_some() || !langsystems.is_empty() {
            surviving_scripts.push((tag, default, langsystems));
        }
    }

    if surviving_scripts.is_empty() {
        return Ok(None);
    }

    // Encode ScriptList:
    //   u16 scriptCount
    //   ScriptRecord records[scriptCount]: { tag(4) + Offset16 }
    //   Script[] bodies (each: defaultLangSysOffset + langSysCount + LangSysRecord[])
    //   LangSys[] bodies (each: lookupOrderOffset(0) + reqFeatureIndex + featureCount + indices[])
    let offsets = Offset16Guard::default();
    let mut out = Vec::new();
    out.extend_from_slice(&(surviving_scripts.len() as u16).to_be_bytes());
    let script_records_start = out.len();
    for _ in 0..surviving_scripts.len() {
        out.extend_from_slice(&[0u8; 6]);
    }
    for (i, (script_tag, default, langsystems)) in surviving_scripts.iter().enumerate() {
        let script_body_start = out.len();
        // Patch the ScriptRecord pointing at this body.
        let rec_off = script_records_start + i * 6;
        out[rec_off..rec_off + 4].copy_from_slice(script_tag);
        let body_off_u16 = offsets.narrow(script_body_start);
        out[rec_off + 4..rec_off + 6].copy_from_slice(&body_off_u16.to_be_bytes());

        let default_slot = out.len();
        out.extend_from_slice(&0u16.to_be_bytes()); // defaultLangSysOffset placeholder
        out.extend_from_slice(&(langsystems.len() as u16).to_be_bytes());
        let langsys_records_start = out.len();
        for _ in 0..langsystems.len() {
            out.extend_from_slice(&[0u8; 6]); // tag + offset placeholders
        }
        // Default LangSys body, if any.
        if let Some(d) = default.as_ref() {
            let langsys_body_pos = out.len() - script_body_start;
            out.extend_from_slice(&encode_langsys(d));
            out[default_slot..default_slot + 2]
                .copy_from_slice(&offsets.narrow(langsys_body_pos).to_be_bytes());
        }
        // Named LangSys bodies.
        for (j, (ls_tag, ls)) in langsystems.iter().enumerate() {
            let langsys_body_pos = out.len() - script_body_start;
            out.extend_from_slice(&encode_langsys(ls));
            let lr_off = langsys_records_start + j * 6;
            out[lr_off..lr_off + 4].copy_from_slice(ls_tag);
            out[lr_off + 4..lr_off + 6]
                .copy_from_slice(&offsets.narrow(langsys_body_pos).to_be_bytes());
        }
    }
    offsets.check("ScriptList rewrite: an offset exceeds 64 KiB")?;
    Ok(Some(out))
}

struct RewrittenLangSys {
    required_feature_index: u16,
    feature_indices: Vec<u16>,
}

/// Walks LangSys raw bytes:
///
/// ```text
///   Offset16 lookupOrderOffset (=0)
///   u16 requiredFeatureIndex
///   u16 featureIndexCount
///   u16 featureIndices[featureIndexCount]
/// ```
///
/// Returns `Ok(None)` when no feature survives, and an error, measured
/// from the start of `bytes`, when the LangSys is truncated.
fn rewrite_langsys_from_bytes(
    bytes: &[u8],
    feature_renumber: &[Option<u16>],
) -> Result<Option<RewrittenLangSys>, sigilbuzz::Error> {
    if bytes.len() < 6 {
        return Err(sigilbuzz::Error::Truncated {
            offset: 0,
            context: "LangSys header truncated",
        });
    }
    let _lookup_order = u16::from_be_bytes([bytes[0], bytes[1]]);
    let required = u16::from_be_bytes([bytes[2], bytes[3]]);
    let count = u16::from_be_bytes([bytes[4], bytes[5]]) as usize;
    let need = 6 + count * 2;
    if bytes.len() < need {
        return Err(sigilbuzz::Error::Truncated {
            offset: 6,
            context: "LangSys featureIndices shorter than featureIndexCount",
        });
    }
    let new_required = if required == 0xFFFF {
        0xFFFF
    } else {
        match feature_renumber.get(required as usize) {
            Some(Some(new)) => *new,
            _ => 0xFFFF,
        }
    };
    let mut new_indices: Vec<u16> = Vec::with_capacity(count);
    for fi in bytes
        .get(6..need)
        .unwrap_or_default()
        .chunks_exact(2)
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
    {
        if let Some(Some(new)) = feature_renumber.get(fi as usize) {
            new_indices.push(*new);
        }
    }
    if new_required == 0xFFFF && new_indices.is_empty() {
        return Ok(None);
    }
    Ok(Some(RewrittenLangSys {
        required_feature_index: new_required,
        feature_indices: new_indices,
    }))
}

/// Rewrites the LangSys at `off` inside `script`, whose Offset16 sits
/// at byte `slot` of `script`. A LangSys that cannot be read is dropped
/// and reported through `diag`. Its feature indices are charged to the
/// work budget in `map`, and nothing is read once it runs out.
fn read_langsys(
    script: &[u8],
    slot: usize,
    off: usize,
    feature_renumber: &[Option<u16>],
    diag: &Diag<'_>,
    map: &GidMap,
) -> Option<RewrittenLangSys> {
    let Some(body) = script.get(off..) else {
        diag.in_part(
            script,
            slot,
            "LangSys offset past the end of the table",
            "a language system",
        );
        return None;
    };
    let count = read_u16(body, 4).map_or(0, usize::from);
    if !map.spend(1 + count) {
        return None;
    }
    match rewrite_langsys_from_bytes(body, feature_renumber) {
        Ok(langsys) => langsys,
        Err(e) => {
            diag.part_error(body, &e, "a language system");
            None
        }
    }
}

fn encode_langsys(ls: &RewrittenLangSys) -> Vec<u8> {
    // LangSys:
    //   Offset16 lookupOrderOffset (0, reserved)
    //   u16 requiredFeatureIndex
    //   u16 featureIndexCount
    //   u16 featureIndices[featureIndexCount]
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&ls.required_feature_index.to_be_bytes());
    out.extend_from_slice(&(ls.feature_indices.len() as u16).to_be_bytes());
    for idx in &ls.feature_indices {
        out.extend_from_slice(&idx.to_be_bytes());
    }
    out
}
