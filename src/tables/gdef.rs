//! `GDEF`: Glyph Definition table.
//!
//! Tells the shaper what each glyph *is*: a plain base glyph, the
//! output of a ligature substitution, a combining mark, or a
//! component that makes up a ligature. Downstream passes use this
//! to decide whether a glyph participates in positioning, and if so
//! how it attaches to its neighbors.
//!
//! `GDEF` is technically optional: plenty of simple fonts do not
//! carry one. Callers that hit [`crate::Face::table_bytes`] for `GDEF` get
//! [`crate::Error::MissingTable`] and must be prepared to fall back
//! to "treat every glyph as a base," which is what [`GlyphClass`]
//! returns by default.
//!
//! # Scope
//!
//! sigilbuzz consumes:
//!
//! - `GlyphClassDef`: per-glyph base/ligature/mark/component class.
//! - `MarkAttachClassDef`: per-mark attachment class, consulted by
//!   the `LookupFlag` skip-iterator when the high byte of the flag is
//!   non-zero.
//! - `MarkGlyphSetsDef` (v1.2+): a list of Coverage tables indexed
//!   by `LookupFlag`'s `markFilteringSet` slot. Used by the skip-
//!   iterator to restrict the set of marks that participate in a
//!   match.
//!
//! `AttachList` and `LigCaretList` are not parsed. The shaper does
//! not use them.

use crate::error::{Error, Result};
use crate::tables::layout::{ClassDef, Coverage};
use crate::tables::parse::Reader;
use crate::tables::variation_store::ItemVariationStore;

/// Glyph role as declared by the font's `GDEF` table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlyphClass {
    /// Plain base glyph. The default when no `GDEF` is present.
    Base,
    /// Output of a ligature substitution (spans multiple clusters).
    Ligature,
    /// Combining mark glyph (attaches to a preceding base).
    Mark,
    /// Component of a ligature. Rarely emitted; usually only seen
    /// in source fonts before feature compilation.
    Component,
    /// Class the font carries but sigilbuzz does not model yet. The
    /// raw class value is preserved so callers can inspect it without
    /// re-parsing.
    Other(u16),
}

impl GlyphClass {
    /// Converts a raw spec class value to a [`GlyphClass`].
    #[must_use]
    pub const fn from_raw(class: u16) -> Self {
        match class {
            0 | 1 => Self::Base,
            2 => Self::Ligature,
            3 => Self::Mark,
            4 => Self::Component,
            other => Self::Other(other),
        }
    }

    /// True if this is a combining mark glyph.
    #[must_use]
    pub const fn is_mark(self) -> bool {
        matches!(self, Self::Mark)
    }

    /// True if this is a base or ligature glyph, the kinds that
    /// accept mark attachments.
    #[must_use]
    pub const fn is_base_or_ligature(self) -> bool {
        matches!(self, Self::Base | Self::Ligature)
    }
}

/// Parsed `GDEF`. Holds a borrowed view of the underlying table so
/// class lookups are allocation-free.
#[derive(Debug, Clone)]
pub struct Gdef<'a> {
    glyph_class_def: Option<ClassDef<'a>>,
    mark_attach_class_def: Option<ClassDef<'a>>,
    /// Parsed mark-glyph-set coverages, one per entry of
    /// `MarkGlyphSetsDef.coverage[]`. Only populated when the table
    /// is v1.2+ *and* carries a mark-glyph-sets subtable.
    mark_glyph_sets: alloc::vec::Vec<Coverage<'a>>,
    /// Shared `ItemVariationStore` (GDEF v1.3+). Every
    /// `VariationIndex` referenced from a GPOS value record or
    /// anchor resolves its outer/inner pair against this store.
    /// Fonts without a v1.3 header leave it `None`.
    item_variation_store: Option<ItemVariationStore<'a>>,
}

impl<'a> Gdef<'a> {
    /// Parses a `GDEF` table. A missing optional subtable (offset 0)
    /// is silently tolerated; the corresponding accessor simply
    /// returns the default.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let major = r.read_u16()?;
        let minor = r.read_u16()?;
        if major != 1 {
            return Err(Error::Malformed {
                offset: 0,
                context: "unsupported GDEF major version",
            });
        }

        // Header order (all Offset16 relative to table start; 0 means
        // absent). Present in every subversion >= 1.0:
        //   glyphClassDefOff, attachListOff, ligCaretListOff,
        //   markAttachClassDefOff
        // Added in 1.2:
        //   markGlyphSetsDefOff
        // Added in 1.3:
        //   itemVarStoreOffset (Offset32): the shared variation store
        //   that every GPOS VariationIndex sub-offset indirects into.
        let glyph_class_def_off = r.read_u16()?;
        let _attach_list_off = r.read_u16()?;
        let _lig_caret_list_off = r.read_u16()?;
        let mark_attach_class_def_off = r.read_u16()?;
        let mark_glyph_sets_def_off = if minor >= 2 { r.read_u16()? } else { 0 };
        let item_var_store_off = if minor >= 3 { r.read_u32()? } else { 0 };

        let glyph_class_def = parse_optional_class_def(
            data,
            glyph_class_def_off,
            "GDEF glyphClassDef offset points outside table",
        )?;
        let mark_attach_class_def = parse_optional_class_def(
            data,
            mark_attach_class_def_off,
            "GDEF markAttachClassDef offset points outside table",
        )?;
        let mark_glyph_sets = if mark_glyph_sets_def_off == 0 {
            alloc::vec::Vec::new()
        } else {
            parse_mark_glyph_sets(data, mark_glyph_sets_def_off as usize)?
        };
        let item_variation_store = if item_var_store_off == 0 {
            None
        } else {
            let start = item_var_store_off as usize;
            let sub = data.get(start..).ok_or(Error::Malformed {
                offset: start,
                context: "GDEF itemVarStore offset points outside table",
            })?;
            Some(ItemVariationStore::parse(sub)?)
        };

        Ok(Self {
            glyph_class_def,
            mark_attach_class_def,
            mark_glyph_sets,
            item_variation_store,
        })
    }

    /// Resolves the glyph class for `glyph_id`. Returns
    /// [`GlyphClass::Base`] when the font omits `GlyphClassDef` or
    /// does not list this glyph, the same default the OpenType
    /// spec prescribes.
    #[must_use]
    pub fn glyph_class(&self, glyph_id: u16) -> GlyphClass {
        match &self.glyph_class_def {
            Some(cd) => GlyphClass::from_raw(cd.class_of(glyph_id)),
            None => GlyphClass::Base,
        }
    }

    /// Mark-attachment class for `glyph_id`. Returns 0 when the font
    /// carries no `MarkAttachClassDef` or the glyph is unlisted.
    /// `LookupFlag`'s high byte is compared against this number; a
    /// zero attachment class matches "any mark" in the spec.
    #[must_use]
    pub fn mark_attach_class(&self, glyph_id: u16) -> u16 {
        match &self.mark_attach_class_def {
            Some(cd) => cd.class_of(glyph_id),
            None => 0,
        }
    }

    /// Coverage for the `index`-th mark-glyph-set in the
    /// `MarkGlyphSetsDef` subtable, or `None` when the font lacks
    /// the subtable or the index is out of range. Referenced by
    /// `LookupFlag & USE_MARK_FILTERING_SET` lookups.
    #[must_use]
    pub fn mark_filtering_set(&self, index: u16) -> Option<&Coverage<'a>> {
        self.mark_glyph_sets.get(index as usize)
    }

    /// Shared `ItemVariationStore` (GDEF v1.3+). Every
    /// `VariationIndex` sub-offset in the font's GPOS value records
    /// or anchors resolves its `(outer, inner)` pair against this
    /// store under the active variation coords. Fonts without a v1.3
    /// header (or a zero `itemVarStoreOffset`) return `None`, and
    /// the shaper treats every VariationIndex as a zero delta.
    #[must_use]
    pub const fn item_variation_store(&self) -> Option<&ItemVariationStore<'a>> {
        self.item_variation_store.as_ref()
    }
}

fn parse_optional_class_def<'a>(
    data: &'a [u8],
    off: u16,
    context: &'static str,
) -> Result<Option<ClassDef<'a>>> {
    if off == 0 {
        return Ok(None);
    }
    let start = off as usize;
    let sub = data.get(start..).ok_or(Error::Malformed {
        offset: start,
        context,
    })?;
    ClassDef::parse(sub).map(Some)
}

fn parse_mark_glyph_sets(data: &[u8], sub_off: usize) -> Result<alloc::vec::Vec<Coverage<'_>>> {
    // Layout:
    //   u16 format (= 1)
    //   u16 markGlyphSetCount
    //   Offset32 coverage[markGlyphSetCount]   -- relative to sub_off
    let sub = data.get(sub_off..).ok_or(Error::Malformed {
        offset: sub_off,
        context: "GDEF markGlyphSetsDef offset points outside table",
    })?;
    let mut r = Reader::new(sub);
    let format = r.read_u16()?;
    if format != 1 {
        return Err(Error::Malformed {
            offset: sub_off,
            context: "unsupported GDEF markGlyphSetsDef format",
        });
    }
    let count = r.read_u16()? as usize;
    // Each coverage offset takes 4 bytes, so the remaining bytes bound
    // how many sets the table can back.
    let mut out = alloc::vec::Vec::with_capacity(count.min(r.remaining() / 4));
    for _ in 0..count {
        let rel = r.read_u32()? as usize;
        if rel == 0 {
            // Spec permits a NULL coverage slot; treat as empty.
            // Build an empty format-1 coverage so the skip-iterator
            // never matches this set.
            out.push(Coverage::parse(&[0, 1, 0, 0])?);
            continue;
        }
        let abs = sub_off.checked_add(rel).ok_or(Error::Malformed {
            offset: sub_off,
            context: "GDEF markGlyphSetsDef coverage offset overflow",
        })?;
        let cov_bytes = data.get(abs..).ok_or(Error::Malformed {
            offset: abs,
            context: "GDEF markGlyphSetsDef coverage offset past end",
        })?;
        out.push(Coverage::parse(cov_bytes)?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    fn build_class_def_format1(start: u16, values: &[u16]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&start.to_be_bytes());
        out.extend_from_slice(&(values.len() as u16).to_be_bytes());
        for v in values {
            out.extend_from_slice(&v.to_be_bytes());
        }
        out
    }

    // Minimal GDEF header with only glyphClassDef populated. Header
    // is 12 bytes for v1.0: u16 major + u16 minor + four u16
    // subtable offsets, each two bytes. The class def body sits
    // immediately after the header at offset 12.
    fn build_gdef_with_class_def(class_def: &[u8]) -> Vec<u8> {
        let header_len = 12u16;
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // major
        out.extend_from_slice(&0u16.to_be_bytes()); // minor
        out.extend_from_slice(&header_len.to_be_bytes()); // glyphClassDefOff
        out.extend_from_slice(&0u16.to_be_bytes()); // attachListOff
        out.extend_from_slice(&0u16.to_be_bytes()); // ligCaretListOff
        out.extend_from_slice(&0u16.to_be_bytes()); // markAttachClassDefOff
        out.extend_from_slice(class_def);
        out
    }

    /// Builds a v1.2 GDEF header with optional markAttachClassDef and
    /// markGlyphSetsDef subtables. Fields set to `None` get a zero
    /// offset in the header.
    fn build_gdef_v12(
        glyph_class_def: Option<&[u8]>,
        mark_attach_class_def: Option<&[u8]>,
        mark_glyph_sets: Option<&[Vec<u8>]>,
    ) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // major
        out.extend_from_slice(&2u16.to_be_bytes()); // minor
        let header_len = 14u16; // v1.2 header is 14 bytes
        out.extend_from_slice(&[0u8; 10]); // placeholders for 5 offset16s
        let gc_slot = 4usize;
        let mac_slot = 10usize;
        let mgs_slot = 12usize;
        debug_assert_eq!(out.len(), header_len as usize);

        if let Some(gc) = glyph_class_def {
            let off = out.len() as u16;
            out[gc_slot..gc_slot + 2].copy_from_slice(&off.to_be_bytes());
            out.extend_from_slice(gc);
        }
        if let Some(mac) = mark_attach_class_def {
            let off = out.len() as u16;
            out[mac_slot..mac_slot + 2].copy_from_slice(&off.to_be_bytes());
            out.extend_from_slice(mac);
        }
        if let Some(sets) = mark_glyph_sets {
            // markGlyphSetsDef: u16 format=1, u16 count, Offset32[count].
            let sub_off = out.len();
            out[mgs_slot..mgs_slot + 2].copy_from_slice(&(sub_off as u16).to_be_bytes());
            out.extend_from_slice(&1u16.to_be_bytes()); // format
            out.extend_from_slice(&(sets.len() as u16).to_be_bytes());
            let off32_slots = out.len();
            for _ in 0..sets.len() {
                out.extend_from_slice(&0u32.to_be_bytes());
            }
            for (i, cov) in sets.iter().enumerate() {
                let rel = (out.len() - sub_off) as u32;
                let slot = off32_slots + i * 4;
                out[slot..slot + 4].copy_from_slice(&rel.to_be_bytes());
                out.extend_from_slice(cov);
            }
        }
        out
    }

    fn build_coverage_format1(glyphs: &[u16]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&(glyphs.len() as u16).to_be_bytes());
        for g in glyphs {
            out.extend_from_slice(&g.to_be_bytes());
        }
        out
    }

    fn build_gdef_without_class_def() -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // major
        out.extend_from_slice(&0u16.to_be_bytes()); // minor
        out.extend_from_slice(&0u16.to_be_bytes()); // glyphClassDefOff = 0
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        out
    }

    #[test]
    fn glyph_class_from_raw_maps_standard_values() {
        assert_eq!(GlyphClass::from_raw(0), GlyphClass::Base);
        assert_eq!(GlyphClass::from_raw(1), GlyphClass::Base);
        assert_eq!(GlyphClass::from_raw(2), GlyphClass::Ligature);
        assert_eq!(GlyphClass::from_raw(3), GlyphClass::Mark);
        assert_eq!(GlyphClass::from_raw(4), GlyphClass::Component);
        assert_eq!(GlyphClass::from_raw(7), GlyphClass::Other(7));
    }

    #[test]
    fn is_mark_reports_only_marks() {
        assert!(GlyphClass::Mark.is_mark());
        assert!(!GlyphClass::Base.is_mark());
        assert!(!GlyphClass::Ligature.is_mark());
    }

    #[test]
    fn is_base_or_ligature_accepts_both() {
        assert!(GlyphClass::Base.is_base_or_ligature());
        assert!(GlyphClass::Ligature.is_base_or_ligature());
        assert!(!GlyphClass::Mark.is_base_or_ligature());
        assert!(!GlyphClass::Component.is_base_or_ligature());
    }

    #[test]
    fn parses_gdef_and_resolves_classes() {
        // Glyphs 10..=13 with classes 1 (base), 2 (ligature), 3 (mark), 4 (component).
        let class_def = build_class_def_format1(10, &[1, 2, 3, 4]);
        let gdef_bytes = build_gdef_with_class_def(&class_def);
        let gdef = Gdef::parse(&gdef_bytes).unwrap();
        assert_eq!(gdef.glyph_class(10), GlyphClass::Base);
        assert_eq!(gdef.glyph_class(11), GlyphClass::Ligature);
        assert_eq!(gdef.glyph_class(12), GlyphClass::Mark);
        assert_eq!(gdef.glyph_class(13), GlyphClass::Component);
    }

    #[test]
    fn unclassified_glyphs_are_treated_as_base() {
        let class_def = build_class_def_format1(10, &[3]);
        let gdef_bytes = build_gdef_with_class_def(&class_def);
        let gdef = Gdef::parse(&gdef_bytes).unwrap();
        // Glyph 10 is declared a Mark.
        assert_eq!(gdef.glyph_class(10), GlyphClass::Mark);
        // Glyph 100 is not listed. ClassDef returns 0, which we map
        // to Base (the spec default).
        assert_eq!(gdef.glyph_class(100), GlyphClass::Base);
    }

    #[test]
    fn gdef_without_glyph_class_def_defaults_every_glyph_to_base() {
        let bytes = build_gdef_without_class_def();
        let gdef = Gdef::parse(&bytes).unwrap();
        assert_eq!(gdef.glyph_class(0), GlyphClass::Base);
        assert_eq!(gdef.glyph_class(50000), GlyphClass::Base);
    }

    #[test]
    fn rejects_unsupported_major_version() {
        let mut bytes = build_gdef_without_class_def();
        bytes[0..2].copy_from_slice(&2u16.to_be_bytes());
        assert!(matches!(Gdef::parse(&bytes), Err(Error::Malformed { .. })));
    }

    #[test]
    fn rejects_glyph_class_def_offset_past_end() {
        let mut bytes = build_gdef_without_class_def();
        // Point glyphClassDefOff past the end of the table.
        let len = bytes.len() as u16;
        let bogus = len + 10;
        bytes[4..6].copy_from_slice(&bogus.to_be_bytes());
        assert!(matches!(Gdef::parse(&bytes), Err(Error::Malformed { .. })));
    }

    #[test]
    fn rejects_truncated_header() {
        let short = [0u8; 3];
        assert!(Gdef::parse(&short).is_err());
    }

    #[test]
    fn parses_mark_attach_class_def() {
        let class_def = build_class_def_format1(10, &[1, 2, 3]);
        let mac = build_class_def_format1(10, &[5, 6, 7]);
        let bytes = build_gdef_v12(Some(&class_def), Some(&mac), None);
        let gdef = Gdef::parse(&bytes).unwrap();
        assert_eq!(gdef.mark_attach_class(10), 5);
        assert_eq!(gdef.mark_attach_class(11), 6);
        assert_eq!(gdef.mark_attach_class(12), 7);
        // Unlisted glyph -> class 0.
        assert_eq!(gdef.mark_attach_class(99), 0);
    }

    #[test]
    fn mark_attach_class_defaults_to_zero_without_subtable() {
        let class_def = build_class_def_format1(10, &[3]);
        let bytes = build_gdef_with_class_def(&class_def);
        let gdef = Gdef::parse(&bytes).unwrap();
        assert_eq!(gdef.mark_attach_class(10), 0);
        assert_eq!(gdef.mark_attach_class(999), 0);
    }

    #[test]
    fn parses_mark_glyph_sets_and_resolves_coverage() {
        // Two mark glyph sets: set 0 = {20, 21}, set 1 = {30}.
        let sets = [
            build_coverage_format1(&[20, 21]),
            build_coverage_format1(&[30]),
        ];
        let class_def = build_class_def_format1(20, &[3, 3]);
        let bytes = build_gdef_v12(Some(&class_def), None, Some(&sets));
        let gdef = Gdef::parse(&bytes).unwrap();

        let set0 = gdef.mark_filtering_set(0).expect("set 0");
        assert!(set0.contains(20));
        assert!(set0.contains(21));
        assert!(!set0.contains(30));

        let set1 = gdef.mark_filtering_set(1).expect("set 1");
        assert!(set1.contains(30));
        assert!(!set1.contains(20));

        // Out-of-range index returns None.
        assert!(gdef.mark_filtering_set(2).is_none());
    }

    #[test]
    fn gdef_without_mark_glyph_sets_returns_none_for_every_index() {
        let class_def = build_class_def_format1(10, &[3]);
        let bytes = build_gdef_with_class_def(&class_def);
        let gdef = Gdef::parse(&bytes).unwrap();
        assert!(gdef.mark_filtering_set(0).is_none());
    }

    fn write_f2dot14(out: &mut Vec<u8>, v: f32) {
        #[allow(clippy::cast_possible_truncation)]
        let raw = (v * 16384.0).round() as i16;
        out.extend_from_slice(&raw.to_be_bytes());
    }

    /// Minimal ItemVariationStore with one axis, one region (0..=1..=1),
    /// one item carrying `delta` at inner index 0, outer index 0.
    fn build_minimal_ivs(delta: i16) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // format
        let region_off_slot = out.len();
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes()); // subtable count
        let subtable_slot = out.len();
        out.extend_from_slice(&0u32.to_be_bytes());

        let region_start = out.len() as u32;
        out[region_off_slot..region_off_slot + 4].copy_from_slice(&region_start.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes()); // axisCount
        out.extend_from_slice(&1u16.to_be_bytes()); // regionCount
        write_f2dot14(&mut out, 0.0);
        write_f2dot14(&mut out, 1.0);
        write_f2dot14(&mut out, 1.0);

        let sub_start = out.len() as u32;
        out[subtable_slot..subtable_slot + 4].copy_from_slice(&sub_start.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes()); // itemCount
        out.extend_from_slice(&1u16.to_be_bytes()); // wordDeltaCount (all wide, short)
        out.extend_from_slice(&1u16.to_be_bytes()); // regionIndexCount
        out.extend_from_slice(&0u16.to_be_bytes()); // region index
        out.extend_from_slice(&delta.to_be_bytes()); // one delta
        out
    }

    /// Builds a v1.3 GDEF with only the itemVarStore slot populated.
    /// Header layout for v1.3: u16 major + u16 minor + u16 * 4 (v1.0)
    /// + u16 mgs + u32 ivs = 18 bytes.
    fn build_gdef_v13_with_ivs_only(ivs: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes()); // major
        out.extend_from_slice(&3u16.to_be_bytes()); // minor
        out.extend_from_slice(&0u16.to_be_bytes()); // glyphClassDefOff
        out.extend_from_slice(&0u16.to_be_bytes()); // attachListOff
        out.extend_from_slice(&0u16.to_be_bytes()); // ligCaretListOff
        out.extend_from_slice(&0u16.to_be_bytes()); // markAttachClassDefOff
        out.extend_from_slice(&0u16.to_be_bytes()); // markGlyphSetsDefOff
        let ivs_off_slot = out.len();
        out.extend_from_slice(&0u32.to_be_bytes()); // itemVarStoreOffset placeholder
        let ivs_start = out.len() as u32;
        out[ivs_off_slot..ivs_off_slot + 4].copy_from_slice(&ivs_start.to_be_bytes());
        out.extend_from_slice(ivs);
        out
    }

    #[test]
    fn parses_item_variation_store_from_v13_header() {
        let ivs = build_minimal_ivs(75);
        let bytes = build_gdef_v13_with_ivs_only(&ivs);
        let gdef = Gdef::parse(&bytes).unwrap();
        let store = gdef.item_variation_store().expect("v1.3 IVS");
        // At coord 1.0 the single region peaks: delta = 75.
        let d = store.delta(0, 0, &[1.0]);
        assert!((d - 75.0).abs() < 1e-3);
    }

    #[test]
    fn v13_with_zero_itemvarstore_offset_yields_none() {
        // Build a v1.3 header whose itemVarStoreOffset stays 0, the
        // spec's "this table omits the optional IVS" sentinel. The
        // parser must not chase the zero offset or the IVS accessor
        // would return Some pointing at garbage.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1u16.to_be_bytes()); // major
        bytes.extend_from_slice(&3u16.to_be_bytes()); // minor
        bytes.extend_from_slice(&[0u8; 10]); // four u16 + u16 mgs = 0
        bytes.extend_from_slice(&0u32.to_be_bytes()); // ivs off = 0
        let gdef = Gdef::parse(&bytes).unwrap();
        assert!(gdef.item_variation_store().is_none());
    }

    #[test]
    fn pre_v13_header_has_no_item_variation_store() {
        // Every test above that builds a v1.0/v1.2 header should also
        // leave the IVS slot empty. Guard against a future refactor
        // that accidentally creates a default store.
        let bytes = build_gdef_without_class_def();
        let gdef = Gdef::parse(&bytes).unwrap();
        assert!(gdef.item_variation_store().is_none());

        let class_def = build_class_def_format1(10, &[3]);
        let bytes = build_gdef_v12(Some(&class_def), None, None);
        let gdef = Gdef::parse(&bytes).unwrap();
        assert!(gdef.item_variation_store().is_none());
    }
}
