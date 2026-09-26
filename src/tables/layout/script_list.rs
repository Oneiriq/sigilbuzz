//! OpenType `ScriptList`: outermost layer of the GSUB / GPOS
//! feature-selection tree.
//!
//! ```text
//!   ScriptList
//!     [ script_tag -> Script ]
//!   Script
//!     default_lang_sys    `LangSys`?
//!     [ lang_sys_tag -> `LangSys` ]
//!   `LangSys`
//!     required_feature_index    u16   (0xFFFF = none)
//!     feature_indices           [u16] (indices into FeatureList)
//! ```
//!
//! Tags are four-byte ASCII arrays. The special tag `b"DFLT"` is the
//! default script; `b"dflt"` is the default language system inside
//! a script.

use crate::error::{Error, Result};
use crate::tables::parse::Reader;

/// `ScriptList`: the top-level directory of scripts carried by a
/// GSUB or GPOS table.
#[derive(Debug, Clone, Copy)]
pub struct ScriptList<'a> {
    data: &'a [u8],
    records_off: usize,
    script_count: u16,
}

impl<'a> ScriptList<'a> {
    /// Parses a `ScriptList` from its raw bytes.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let script_count = r.read_u16()?;
        let records_off = r.position();
        let need = records_off + script_count as usize * 6;
        if data.len() < need {
            return Err(Error::Truncated {
                offset: records_off,
                context: "scriptList records shorter than scriptCount",
            });
        }
        Ok(Self {
            data,
            records_off,
            script_count,
        })
    }

    /// Number of script records.
    #[must_use]
    pub const fn len(&self) -> u16 {
        self.script_count
    }

    /// True when the list is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.script_count == 0
    }

    /// Iterates `(tag, Script)` pairs in record order.
    #[allow(clippy::iter_without_into_iter)]
    pub fn iter(&self) -> ScriptIter<'a> {
        ScriptIter {
            list: *self,
            idx: 0,
        }
    }

    /// Looks a script up by tag. Binary search: records are sorted
    /// by tag in every valid OpenType file.
    #[must_use]
    pub fn find(&self, tag: [u8; 4]) -> Option<Script<'a>> {
        let mut lo: u16 = 0;
        let mut hi: u16 = self.script_count;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let (t, off) = self.record_at(mid);
            match t.cmp(&tag) {
                core::cmp::Ordering::Less => lo = mid + 1,
                core::cmp::Ordering::Greater => hi = mid,
                core::cmp::Ordering::Equal => {
                    return Script::parse_at(self.data, off).ok();
                }
            }
        }
        None
    }

    fn record_at(&self, i: u16) -> ([u8; 4], u16) {
        let off = self.records_off + i as usize * 6;
        let tag = [
            self.data[off],
            self.data[off + 1],
            self.data[off + 2],
            self.data[off + 3],
        ];
        let sub_off = u16::from_be_bytes([self.data[off + 4], self.data[off + 5]]);
        (tag, sub_off)
    }
}

/// Iterator yielding `(tag, Script)` for each script record.
pub struct ScriptIter<'a> {
    list: ScriptList<'a>,
    idx: u16,
}

impl<'a> Iterator for ScriptIter<'a> {
    type Item = ([u8; 4], Script<'a>);

    fn next(&mut self) -> Option<Self::Item> {
        while self.idx < self.list.script_count {
            let (tag, off) = self.list.record_at(self.idx);
            self.idx += 1;
            if let Ok(script) = Script::parse_at(self.list.data, off) {
                return Some((tag, script));
            }
        }
        None
    }
}

/// A single `Script` within a `ScriptList`.
#[derive(Debug, Clone, Copy)]
pub struct Script<'a> {
    data: &'a [u8],
    /// Absolute offset to this script's default `LangSys` (0 means none).
    default_lang_sys_off: u16,
    /// Byte offset of the language-system record array.
    lang_sys_records_off: usize,
    lang_sys_count: u16,
    /// Absolute base offset the script's sub-offsets are relative to.
    base: usize,
}

impl<'a> Script<'a> {
    fn parse_at(data: &'a [u8], script_off: u16) -> Result<Self> {
        let base = script_off as usize;
        if base >= data.len() {
            return Err(Error::Malformed {
                offset: base,
                context: "script offset past end of ScriptList",
            });
        }
        let mut r = Reader::at(data, base)?;
        let default_lang_sys_off_rel = r.read_u16()?;
        let lang_sys_count = r.read_u16()?;
        let lang_sys_records_off = r.position();

        let need = lang_sys_records_off + lang_sys_count as usize * 6;
        if data.len() < need {
            return Err(Error::Truncated {
                offset: lang_sys_records_off,
                context: "script langSysRecords shorter than langSysCount",
            });
        }

        // default_lang_sys_off is relative to the Script table's start.
        let default_lang_sys_off = if default_lang_sys_off_rel == 0 {
            0
        } else {
            // Store as absolute file offset for consistent lookups.
            let abs = base + default_lang_sys_off_rel as usize;
            if abs > u16::MAX as usize {
                return Err(Error::Malformed {
                    offset: base,
                    context: "script default LangSys offset overflows u16",
                });
            }
            abs as u16
        };

        Ok(Self {
            data,
            default_lang_sys_off,
            lang_sys_records_off,
            lang_sys_count,
            base,
        })
    }

    /// Returns the default language system if this script defines one.
    #[must_use]
    pub fn default_lang_sys(&self) -> Option<LangSys<'a>> {
        if self.default_lang_sys_off == 0 {
            return None;
        }
        LangSys::parse_at(self.data, self.default_lang_sys_off).ok()
    }

    /// Looks a language system up by tag.
    #[must_use]
    pub fn find_lang_sys(&self, tag: [u8; 4]) -> Option<LangSys<'a>> {
        let mut lo: u16 = 0;
        let mut hi: u16 = self.lang_sys_count;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let (t, rel_off) = self.lang_sys_record_at(mid);
            match t.cmp(&tag) {
                core::cmp::Ordering::Less => lo = mid + 1,
                core::cmp::Ordering::Greater => hi = mid,
                core::cmp::Ordering::Equal => {
                    // `rel_off` is relative to the Script table
                    // start; `LangSys::parse_at` takes an absolute
                    // u16 offset into `data`. The naïve `(self.base
                    // + rel_off) as u16` silently wraps when the sum
                    // exceeds `u16::MAX`: a Script placed past
                    // offset 0x8000 combined with a large rel_off is
                    // enough to do this. Surface overflow as a miss
                    // so a crafted font cannot redirect the parse to
                    // a wrapped-around location inside the blob.
                    let abs_off = u16::try_from(self.base + rel_off as usize).ok()?;
                    return LangSys::parse_at(self.data, abs_off).ok();
                }
            }
        }
        None
    }

    /// Picks the language system for a list of candidate language tags,
    /// the way HarfBuzz's `hb_ot_layout_script_select_language` does:
    /// the first tag with a record wins, then a record tagged `dflt`
    /// (some fonts ship one instead of a default offset), then the
    /// script's default language system. `None` only when the script
    /// has none of these.
    ///
    /// # Examples
    ///
    /// ```
    /// use sigilbuzz::Face;
    ///
    /// let data = include_bytes!("../../../tests/fixtures/opensans_regular.ttf");
    /// let face = Face::parse_bytes(data, 0)?;
    /// let gsub = face.gsub()?.expect("Open Sans has GSUB");
    /// let latn = gsub.script_list().find(*b"latn").expect("latn script");
    /// let romanian = latn.select_lang_sys(&[*b"XYZ ", *b"ROM "]).expect("ROM LangSys");
    /// let default = latn.select_lang_sys(&[]).expect("default LangSys");
    /// assert_ne!(
    ///     romanian.feature_indices().collect::<Vec<_>>(),
    ///     default.feature_indices().collect::<Vec<_>>()
    /// );
    /// # Ok::<(), sigilbuzz::Error>(())
    /// ```
    #[must_use]
    pub fn select_lang_sys(&self, language_tags: &[[u8; 4]]) -> Option<LangSys<'a>> {
        language_tags
            .iter()
            .find_map(|tag| self.find_lang_sys(*tag))
            .or_else(|| self.find_lang_sys(*b"dflt"))
            .or_else(|| self.default_lang_sys())
    }

    /// Number of language-system records (not counting the default).
    #[must_use]
    pub const fn lang_sys_count(&self) -> u16 {
        self.lang_sys_count
    }

    fn lang_sys_record_at(&self, i: u16) -> ([u8; 4], u16) {
        let off = self.lang_sys_records_off + i as usize * 6;
        let tag = [
            self.data[off],
            self.data[off + 1],
            self.data[off + 2],
            self.data[off + 3],
        ];
        let sub_off = u16::from_be_bytes([self.data[off + 4], self.data[off + 5]]);
        (tag, sub_off)
    }
}

/// A single language system: the leaf of the script tree that
/// actually names the features in effect.
#[derive(Debug, Clone, Copy)]
pub struct LangSys<'a> {
    data: &'a [u8],
    required_feature_index: u16,
    feature_indices_off: usize,
    feature_index_count: u16,
}

impl<'a> LangSys<'a> {
    fn parse_at(data: &'a [u8], off: u16) -> Result<Self> {
        let base = off as usize;
        if base >= data.len() {
            return Err(Error::Malformed {
                offset: base,
                context: "LangSys offset past end",
            });
        }
        let mut r = Reader::at(data, base)?;
        let lookup_order = r.read_u16()?;
        if lookup_order != 0 {
            return Err(Error::Malformed {
                offset: base,
                context: "LangSys lookupOrder must be 0 (reserved)",
            });
        }
        let required_feature_index = r.read_u16()?;
        let feature_index_count = r.read_u16()?;
        let feature_indices_off = r.position();
        let need = feature_indices_off + feature_index_count as usize * 2;
        if data.len() < need {
            return Err(Error::Truncated {
                offset: feature_indices_off,
                context: "LangSys featureIndices shorter than count",
            });
        }
        Ok(Self {
            data,
            required_feature_index,
            feature_indices_off,
            feature_index_count,
        })
    }

    /// Index of the required feature, or `None` (spec value 0xFFFF).
    #[must_use]
    pub const fn required_feature_index(&self) -> Option<u16> {
        if self.required_feature_index == 0xFFFF {
            None
        } else {
            Some(self.required_feature_index)
        }
    }

    /// Number of non-required feature indices attached.
    #[must_use]
    pub const fn len(&self) -> u16 {
        self.feature_index_count
    }

    /// True if no features are attached.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.feature_index_count == 0
    }

    /// Iterates feature indices in order.
    pub fn feature_indices(&self) -> FeatureIndexIter<'a> {
        FeatureIndexIter {
            data: self.data,
            off: self.feature_indices_off,
            remaining: self.feature_index_count,
        }
    }
}

/// Iterator over `LangSys` feature indices.
pub struct FeatureIndexIter<'a> {
    data: &'a [u8],
    off: usize,
    remaining: u16,
}

impl Iterator for FeatureIndexIter<'_> {
    type Item = u16;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let val = u16::from_be_bytes([self.data[self.off], self.data[self.off + 1]]);
        self.off += 2;
        self.remaining -= 1;
        Some(val)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    /// Builds a `LangSys` body: lookupOrder(0) + requiredFI + count + indices.
    fn build_lang_sys(required: u16, indices: &[u16]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&0u16.to_be_bytes()); // lookupOrder
        out.extend_from_slice(&required.to_be_bytes());
        out.extend_from_slice(&(indices.len() as u16).to_be_bytes());
        for i in indices {
            out.extend_from_slice(&i.to_be_bytes());
        }
        out
    }

    /// (tag, defaultLangSysBody?, langSysRecordBodies): a
    /// flattened view of one script.
    type ScriptFixture<'a> = ([u8; 4], Option<Vec<u8>>, &'a [([u8; 4], Vec<u8>)]);

    /// Assembles a `ScriptList` + `Script` + `LangSys` triad in a
    /// single blob.
    fn build_minimal_scriptlist(scripts: &[ScriptFixture<'_>]) -> Vec<u8> {
        // Layout plan:
        //   ScriptList header (2 + 6 * nScripts)
        //   for each script: Script header + LangSysRecord[*] + bodies
        // Simpler: walk once to compute sizes, then write.
        let mut out = Vec::new();
        out.extend_from_slice(&(scripts.len() as u16).to_be_bytes());
        // Reserve space for script records: tag(4) + offset(2) * N.
        let script_records_start = out.len();
        for _ in 0..scripts.len() {
            out.extend_from_slice(&[0u8; 6]);
        }

        for (i, (tag, default_ls, lang_sys_records)) in scripts.iter().enumerate() {
            let script_start = out.len();
            // Record layout: tag (4 bytes) + offset (2 bytes) = 6
            // bytes per record, packed after the scriptCount u16.
            let rec_off = script_records_start + i * 6;
            out[rec_off..rec_off + 4].copy_from_slice(tag);
            out[rec_off + 4..rec_off + 6].copy_from_slice(&(script_start as u16).to_be_bytes());

            // Script header: defaultLangSysOffset + langSysCount +
            // langSysRecords[*]. Offsets are Script-relative.
            let default_off_slot = out.len();
            out.extend_from_slice(&[0u8; 2]); // defaultLangSysOffset placeholder
            out.extend_from_slice(&(lang_sys_records.len() as u16).to_be_bytes());
            let lang_sys_records_start = out.len();
            for _ in 0..lang_sys_records.len() {
                out.extend_from_slice(&[0u8; 6]);
            }

            if let Some(body) = default_ls {
                let body_start = out.len();
                out.extend_from_slice(body);
                let rel = (body_start - script_start) as u16;
                out[default_off_slot..default_off_slot + 2].copy_from_slice(&rel.to_be_bytes());
            }

            for (j, (ls_tag, ls_body)) in lang_sys_records.iter().enumerate() {
                let body_start = out.len();
                out.extend_from_slice(ls_body);
                let rec_slot = lang_sys_records_start + j * 6;
                out[rec_slot..rec_slot + 4].copy_from_slice(ls_tag);
                let rel = (body_start - script_start) as u16;
                out[rec_slot + 4..rec_slot + 6].copy_from_slice(&rel.to_be_bytes());
            }
        }

        out
    }

    #[test]
    fn empty_script_list_parses_and_iterates_to_none() {
        let bytes = build_minimal_scriptlist(&[]);
        let list = ScriptList::parse(&bytes).unwrap();
        assert!(list.is_empty());
        assert_eq!(list.len(), 0);
        assert!(list.iter().next().is_none());
    }

    #[test]
    fn finds_script_by_tag() {
        let latn_default = build_lang_sys(0xFFFF, &[0, 1]);
        let bytes = build_minimal_scriptlist(&[
            (*b"DFLT", Some(build_lang_sys(0xFFFF, &[])), &[]),
            (*b"latn", Some(latn_default), &[]),
        ]);
        let list = ScriptList::parse(&bytes).unwrap();
        let latn = list.find(*b"latn").expect("latn present");
        let default = latn.default_lang_sys().expect("default LangSys");
        let indices: Vec<_> = default.feature_indices().collect();
        assert_eq!(indices, alloc::vec![0, 1]);
    }

    #[test]
    fn missing_script_returns_none() {
        let bytes = build_minimal_scriptlist(&[(*b"DFLT", None, &[])]);
        let list = ScriptList::parse(&bytes).unwrap();
        assert!(list.find(*b"arab").is_none());
    }

    #[test]
    fn script_default_lang_sys_optional() {
        let bytes = build_minimal_scriptlist(&[(*b"DFLT", None, &[])]);
        let list = ScriptList::parse(&bytes).unwrap();
        let script = list.find(*b"DFLT").unwrap();
        assert!(script.default_lang_sys().is_none());
    }

    #[test]
    fn lang_sys_records_are_searchable() {
        let latn_default = build_lang_sys(0xFFFF, &[0]);
        let latn_eng = build_lang_sys(0xFFFF, &[5, 6, 7]);
        let latn_deu = build_lang_sys(0xFFFF, &[9]);
        // Records must be sorted by tag: DEU < ENG.
        let bytes = build_minimal_scriptlist(&[(
            *b"latn",
            Some(latn_default),
            &[(*b"DEU ", latn_deu), (*b"ENG ", latn_eng)],
        )]);
        let list = ScriptList::parse(&bytes).unwrap();
        let latn = list.find(*b"latn").unwrap();
        let eng = latn.find_lang_sys(*b"ENG ").expect("ENG LangSys");
        let idx: Vec<_> = eng.feature_indices().collect();
        assert_eq!(idx, alloc::vec![5, 6, 7]);
        let deu = latn.find_lang_sys(*b"DEU ").expect("DEU LangSys");
        assert_eq!(deu.feature_indices().collect::<Vec<_>>(), alloc::vec![9]);
    }

    #[test]
    fn select_lang_sys_prefers_candidates_in_order() {
        let bytes = build_minimal_scriptlist(&[(
            *b"cyrl",
            Some(build_lang_sys(0xFFFF, &[0])),
            &[
                (*b"MKD ", build_lang_sys(0xFFFF, &[1])),
                (*b"SRB ", build_lang_sys(0xFFFF, &[2])),
            ],
        )]);
        let list = ScriptList::parse(&bytes).unwrap();
        let cyrl = list.find(*b"cyrl").unwrap();
        let pick = |tags: &[[u8; 4]]| -> Vec<u16> {
            cyrl.select_lang_sys(tags)
                .unwrap()
                .feature_indices()
                .collect()
        };
        assert_eq!(pick(&[*b"SRB "]), alloc::vec![2]);
        assert_eq!(pick(&[*b"XXX ", *b"MKD ", *b"SRB "]), alloc::vec![1]);
        // No candidate present: the default LangSys.
        assert_eq!(pick(&[*b"TRK "]), alloc::vec![0]);
        assert_eq!(pick(&[]), alloc::vec![0]);
    }

    #[test]
    fn select_lang_sys_tries_dflt_record_before_default_offset() {
        let bytes = build_minimal_scriptlist(&[(
            *b"latn",
            Some(build_lang_sys(0xFFFF, &[0])),
            &[(*b"dflt", build_lang_sys(0xFFFF, &[7]))],
        )]);
        let list = ScriptList::parse(&bytes).unwrap();
        let latn = list.find(*b"latn").unwrap();
        let picked: Vec<u16> = latn
            .select_lang_sys(&[*b"ENG "])
            .unwrap()
            .feature_indices()
            .collect();
        assert_eq!(picked, alloc::vec![7]);
    }

    #[test]
    fn select_lang_sys_without_default_or_match_is_none() {
        let bytes = build_minimal_scriptlist(&[(
            *b"latn",
            None,
            &[(*b"TRK ", build_lang_sys(0xFFFF, &[3]))],
        )]);
        let list = ScriptList::parse(&bytes).unwrap();
        let latn = list.find(*b"latn").unwrap();
        assert!(latn.select_lang_sys(&[*b"ENG "]).is_none());
        assert!(latn.select_lang_sys(&[*b"TRK "]).is_some());
    }

    #[test]
    fn lang_sys_required_feature_index_maps_sentinel_to_none() {
        let ls = build_lang_sys(0xFFFF, &[]);
        let bytes = build_minimal_scriptlist(&[(*b"DFLT", Some(ls), &[])]);
        let list = ScriptList::parse(&bytes).unwrap();
        let default = list.find(*b"DFLT").unwrap().default_lang_sys().unwrap();
        assert_eq!(default.required_feature_index(), None);
    }

    #[test]
    fn lang_sys_required_feature_index_reported_when_set() {
        let ls = build_lang_sys(3, &[]);
        let bytes = build_minimal_scriptlist(&[(*b"DFLT", Some(ls), &[])]);
        let list = ScriptList::parse(&bytes).unwrap();
        let default = list.find(*b"DFLT").unwrap().default_lang_sys().unwrap();
        assert_eq!(default.required_feature_index(), Some(3));
    }

    #[test]
    fn iter_yields_scripts_in_record_order() {
        let bytes = build_minimal_scriptlist(&[(*b"DFLT", None, &[]), (*b"latn", None, &[])]);
        let list = ScriptList::parse(&bytes).unwrap();
        let tags: Vec<_> = list.iter().map(|(t, _)| t).collect();
        assert_eq!(tags, alloc::vec![*b"DFLT", *b"latn"]);
    }

    #[test]
    fn rejects_truncated_script_records() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&2u16.to_be_bytes()); // claim 2 scripts
        bytes.extend_from_slice(&[0u8; 6]); // only one record
        assert!(matches!(
            ScriptList::parse(&bytes),
            Err(Error::Truncated { .. })
        ));
    }

    #[test]
    fn find_lang_sys_returns_none_when_absolute_offset_overflows_u16() {
        // Construct a ScriptList where Script sits at offset 0x8010
        // and declares a langSysRecord with rel_off 0x8000. The
        // buggy `(base + rel_off) as u16` wraps to 0x0010, where we
        // planted a byte-valid LangSys (feature_indices = [42]). A
        // correct implementation must not silently use that wrapped
        // location. It must report the offset as unreachable.
        let script_off: u16 = 0x8010;
        let rel_off: u16 = 0x8000; // base(0x8010) + rel(0x8000) = 0x10010 -> wraps to 0x0010
        let wrap_to: usize = 0x0010;

        let mut bytes = Vec::new();
        // scriptCount = 1.
        bytes.extend_from_slice(&1u16.to_be_bytes());
        // One ScriptRecord: tag + u16 scriptOffset.
        bytes.extend_from_slice(b"test");
        bytes.extend_from_slice(&script_off.to_be_bytes());
        // Pad to offset 0x10; plant a byte-valid LangSys there so a
        // buggy lookup would silently parse it.
        bytes.resize(wrap_to, 0);
        bytes.extend_from_slice(&0u16.to_be_bytes()); // lookupOrder
        bytes.extend_from_slice(&0xFFFFu16.to_be_bytes()); // requiredFeatureIndex
        bytes.extend_from_slice(&1u16.to_be_bytes()); // featureIndexCount
        bytes.extend_from_slice(&42u16.to_be_bytes()); // one feature index
                                                       // Pad to the real Script start.
        bytes.resize(script_off as usize, 0);
        // Script header: defaultLangSysOffset = 0 (absent), langSysCount = 1.
        bytes.extend_from_slice(&0u16.to_be_bytes());
        bytes.extend_from_slice(&1u16.to_be_bytes());
        // One LangSysRecord pointing past u16::MAX when added to base.
        bytes.extend_from_slice(b"ENG ");
        bytes.extend_from_slice(&rel_off.to_be_bytes());

        let list = ScriptList::parse(&bytes).unwrap();
        let script = list.find(*b"test").expect("script present");
        // Without the overflow guard this returns a LangSys with
        // feature index 42 parsed from the wrapped location, which
        // is wrong on every axis. Must be None.
        assert!(script.find_lang_sys(*b"ENG ").is_none());
    }
}
