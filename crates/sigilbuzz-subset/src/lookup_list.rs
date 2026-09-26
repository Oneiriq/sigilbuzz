//! LookupList serialization for the rebuilt GSUB and GPOS tables.
//!
//! ```text
//!   LookupList:
//!     u16      lookupCount
//!     Offset16 lookupOffsets[lookupCount]        (from LookupList)
//!   Lookup:
//!     u16      lookupType
//!     u16      lookupFlag
//!     u16      subTableCount
//!     Offset16 subtableOffsets[subTableCount]    (from Lookup)
//!     u16      markFilteringSet                  (if the flag asks for it)
//! ```
//!
//! Both offset levels are 16-bit. The compact layout writes each Lookup
//! followed by its own subtables, which keeps small tables small and is
//! what [`emit`] produces whenever every offset fits. Once the rebuilt
//! lookups outgrow what an Offset16 can reach (carrying Device and
//! VariationIndex tables makes mark lookups of variable fonts grow
//! quickly), a compact layout would silently wrap its offsets and
//! corrupt every lookup past the limit.
//!
//! In that case every lookup becomes an Extension lookup (GSUB type 7,
//! GPOS type 9): all Lookup headers come first, then one 8-byte
//! extension subtable per subtable, then the real subtable bodies,
//! which the extension subtables reach through an Offset32:
//!
//! ```text
//!   ExtensionSubst / ExtensionPos:
//!     u16      format = 1
//!     u16      extensionLookupType
//!     Offset32 extensionOffset                   (from this subtable)
//! ```
//!
//! Only the headers and the 8-byte extension subtables then need 16-bit
//! offsets, and they stay small no matter how large the bodies grow.

use alloc::vec::Vec;

use crate::layout::RewrittenLookup;

/// Serializes `lookups` as a LookupList. `extension_type` is the
/// table's Extension lookup type (7 for GSUB, 9 for GPOS). Returns
/// `None` only when even the extension layout cannot address its
/// headers, which takes tens of thousands of subtables.
pub(crate) fn emit(lookups: &[RewrittenLookup], extension_type: u16) -> Option<Vec<u8>> {
    compact(lookups).or_else(|| extension(lookups, extension_type))
}

/// Writes `value` at `pos`. Callers only write slots they reserved.
fn put_u16(out: &mut [u8], pos: usize, value: u16) {
    if let Some(slot) = out.get_mut(pos..pos + 2) {
        slot.copy_from_slice(&value.to_be_bytes());
    }
}

/// Appends a Lookup header with zeroed subtable offsets and returns the
/// position of the first offset slot. `None` when the lookup has more
/// subtables than its 16-bit count can hold, which splitting a hostile
/// subtable into many pieces could produce.
fn push_header(out: &mut Vec<u8>, lookup: &RewrittenLookup, lookup_type: u16) -> Option<usize> {
    let count = u16::try_from(lookup.subtables.len()).ok()?;
    out.extend_from_slice(&lookup_type.to_be_bytes());
    out.extend_from_slice(&lookup.lookup_flag.to_be_bytes());
    out.extend_from_slice(&count.to_be_bytes());
    let slots = out.len();
    out.resize(slots + lookup.subtables.len() * 2, 0);
    if let Some(set) = lookup.mark_filtering_set {
        out.extend_from_slice(&set.to_be_bytes());
    }
    Some(slots)
}

/// Each Lookup followed by its subtables. `None` when an offset would
/// not fit in 16 bits.
fn compact(lookups: &[RewrittenLookup]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    out.extend_from_slice(&u16::try_from(lookups.len()).ok()?.to_be_bytes());
    out.resize(2 + lookups.len() * 2, 0);
    for (i, lookup) in lookups.iter().enumerate() {
        let lookup_start = out.len();
        put_u16(&mut out, 2 + i * 2, u16::try_from(lookup_start).ok()?);
        let slots = push_header(&mut out, lookup, lookup.lookup_type)?;
        for (j, sub) in lookup.subtables.iter().enumerate() {
            let rel = u16::try_from(out.len() - lookup_start).ok()?;
            put_u16(&mut out, slots + j * 2, rel);
            out.extend_from_slice(&sub.bytes);
        }
    }
    Some(out)
}

/// Splits one rewritten subtable into the lookup type its extension
/// subtable names and the body the extension points at. Subtables of a
/// lookup that already was an Extension lookup carry their own 8-byte
/// extension header, which is unwrapped so extensions never nest.
fn inner_of(lookup: &RewrittenLookup, bytes: &[u8], extension_type: u16) -> (u16, Vec<u8>) {
    if lookup.lookup_type == extension_type && bytes.len() >= 8 {
        let inner_type = u16::from_be_bytes([bytes[2], bytes[3]]);
        let off = u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
        if let Some(body) = bytes.get(off..) {
            return (inner_type, body.to_vec());
        }
    }
    (lookup.lookup_type, bytes.to_vec())
}

/// Every lookup as an Extension lookup: headers, then the 8-byte
/// extension subtables, then the bodies.
fn extension(lookups: &[RewrittenLookup], extension_type: u16) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    out.extend_from_slice(&u16::try_from(lookups.len()).ok()?.to_be_bytes());
    out.resize(2 + lookups.len() * 2, 0);
    // (header start, first offset slot) per lookup.
    let mut headers = Vec::with_capacity(lookups.len());
    for (i, lookup) in lookups.iter().enumerate() {
        let lookup_start = out.len();
        put_u16(&mut out, 2 + i * 2, u16::try_from(lookup_start).ok()?);
        let slots = push_header(&mut out, lookup, extension_type)?;
        headers.push((lookup_start, slots));
    }
    // (extension subtable position, body) in lookup order.
    let mut bodies = Vec::new();
    for (lookup, &(lookup_start, slots)) in lookups.iter().zip(&headers) {
        for (j, sub) in lookup.subtables.iter().enumerate() {
            let at = out.len();
            put_u16(
                &mut out,
                slots + j * 2,
                u16::try_from(at - lookup_start).ok()?,
            );
            let (inner_type, body) = inner_of(lookup, &sub.bytes, extension_type);
            out.extend_from_slice(&1u16.to_be_bytes());
            out.extend_from_slice(&inner_type.to_be_bytes());
            out.extend_from_slice(&0u32.to_be_bytes());
            bodies.push((at, body));
        }
    }
    for (at, body) in bodies {
        let rel = u32::try_from(out.len() - at).ok()?;
        out.get_mut(at + 4..at + 8)?
            .copy_from_slice(&rel.to_be_bytes());
        out.extend_from_slice(&body);
    }
    Some(out)
}

#[cfg(test)]
mod tests;
