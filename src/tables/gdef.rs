//! `GDEF` — Glyph Definition table.
//!
//! Tells the shaper what each glyph *is*: a plain base glyph, the
//! output of a ligature substitution, a combining mark, or a
//! component that makes up a ligature. Downstream passes use this
//! to decide whether a glyph participates in positioning, and if so
//! how it attaches to its neighbours.
//!
//! `GDEF` is technically optional — plenty of simple fonts do not
//! carry one. Callers that hit [`Face::table_bytes`] for `GDEF` get
//! [`crate::Error::MissingTable`] and must be prepared to fall back
//! to "treat every glyph as a base," which is what [`GlyphClass`]
//! returns by default.
//!
//! # Scope
//!
//! M2 only consumes the `GlyphClassDef` subtable. `AttachList`,
//! `LigCaretList`, `MarkAttachClassDef`, and `MarkGlyphSetsDef` are
//! left as byte slices for later milestones — their parsers land
//! when the shaper needs them.

use crate::error::{Error, Result};
use crate::tables::layout::ClassDef;
use crate::tables::parse::Reader;

/// Glyph role as declared by the font's `GDEF` table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlyphClass {
    /// Plain base glyph. The default when no `GDEF` is present.
    Base,
    /// Output of a ligature substitution (spans multiple clusters).
    Ligature,
    /// Combining mark glyph (attaches to a preceding base).
    Mark,
    /// Component of a ligature — rarely emitted; usually only seen
    /// in source fonts before feature compilation.
    Component,
    /// Class the font carries but sigilbuzz does not model yet. The
    /// raw class value is preserved so future milestones can inspect
    /// it without re-parsing.
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

    /// True if this is a base or ligature glyph — the kinds that
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
}

impl<'a> Gdef<'a> {
    /// Parses a `GDEF` table. A missing optional subtable (offset 0)
    /// is silently tolerated; the corresponding accessor simply
    /// returns the default.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let major = r.read_u16()?;
        let _minor = r.read_u16()?;
        if major != 1 {
            return Err(Error::Malformed {
                offset: 0,
                context: "unsupported GDEF major version",
            });
        }

        let glyph_class_def_off = r.read_u16()?;
        // Remaining header fields (attachListOffset, ligCaretListOffset,
        // markAttachClassDefOffset, and in later sub-versions the mark
        // glyph sets and item variation store offsets) are read-skip:
        // we record their presence but do not parse their subtables.
        // The exact count depends on the sub-version but the fields
        // we do consume are all at fixed offsets, so no validation is
        // lost.

        let glyph_class_def = if glyph_class_def_off == 0 {
            None
        } else {
            let start = glyph_class_def_off as usize;
            let sub = data.get(start..).ok_or(Error::Malformed {
                offset: start,
                context: "GDEF glyphClassDef offset points outside table",
            })?;
            Some(ClassDef::parse(sub)?)
        };

        Ok(Self { glyph_class_def })
    }

    /// Resolves the glyph class for `glyph_id`. Returns
    /// [`GlyphClass::Base`] when the font omits `GlyphClassDef` or
    /// does not list this glyph — the same default the OpenType
    /// spec prescribes.
    #[must_use]
    pub fn glyph_class(&self, glyph_id: u16) -> GlyphClass {
        match &self.glyph_class_def {
            Some(cd) => GlyphClass::from_raw(cd.class_of(glyph_id)),
            None => GlyphClass::Base,
        }
    }
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
        // Glyph 100 is not listed — ClassDef returns 0, which we map
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
}
