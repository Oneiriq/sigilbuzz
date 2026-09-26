//! OpenType `FeatureList`: the middle layer between a language
//! system's feature *indices* and the lookup list's lookups.
//!
//! ```text
//!   FeatureList
//!     u16 featureCount
//!     FeatureRecord records[featureCount]:
//!       Tag       featureTag
//!       Offset16  featureOffset     (relative to FeatureList start)
//!
//!   Feature
//!     Offset16  featureParamsOffset  (may be 0)
//!     u16       lookupIndexCount
//!     u16       lookupListIndices[lookupIndexCount]
//! ```
//!
//! Callers reach features by index (handed out by a `LangSys`) or by
//! iterating and filtering on the tag.

use crate::error::{Error, Result};
use crate::tables::parse::Reader;

/// Parsed `FeatureList`.
#[derive(Debug, Clone, Copy)]
pub struct FeatureList<'a> {
    data: &'a [u8],
    records_off: usize,
    feature_count: u16,
}

impl<'a> FeatureList<'a> {
    /// Parses a `FeatureList` from its raw bytes.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let feature_count = r.read_u16()?;
        let records_off = r.position();
        let need = records_off + feature_count as usize * 6;
        if data.len() < need {
            return Err(Error::Truncated {
                offset: records_off,
                context: "featureList records shorter than featureCount",
            });
        }
        Ok(Self {
            data,
            records_off,
            feature_count,
        })
    }

    /// Number of feature records.
    #[must_use]
    pub const fn len(&self) -> u16 {
        self.feature_count
    }

    /// True if the list has no features.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.feature_count == 0
    }

    /// Returns the feature at `index` as `(tag, Feature)`, or `None`
    /// if the index is out of range.
    #[must_use]
    pub fn get(&self, index: u16) -> Option<([u8; 4], Feature<'a>)> {
        if index >= self.feature_count {
            return None;
        }
        let (tag, offset) = self.record_at(index);
        Feature::parse_at(self.data, offset).ok().map(|f| (tag, f))
    }

    /// Iterates `(tag, Feature)` pairs in record order.
    // The iterator yields owned `(tag, Feature)` values and the list is
    // `Copy`, so an `IntoIterator for &FeatureList` impl would add API
    // surface without any benefit.
    #[allow(clippy::iter_without_into_iter)]
    pub fn iter(&self) -> FeatureIter<'a> {
        FeatureIter {
            list: *self,
            idx: 0,
        }
    }

    fn record_at(&self, i: u16) -> ([u8; 4], u16) {
        let off = self.records_off + i as usize * 6;
        let tag = [
            self.data[off],
            self.data[off + 1],
            self.data[off + 2],
            self.data[off + 3],
        ];
        let feature_off = u16::from_be_bytes([self.data[off + 4], self.data[off + 5]]);
        (tag, feature_off)
    }
}

/// Iterator over a `FeatureList`.
pub struct FeatureIter<'a> {
    list: FeatureList<'a>,
    idx: u16,
}

impl<'a> Iterator for FeatureIter<'a> {
    type Item = ([u8; 4], Feature<'a>);

    fn next(&mut self) -> Option<Self::Item> {
        while self.idx < self.list.feature_count {
            let (tag, off) = self.list.record_at(self.idx);
            self.idx += 1;
            if let Ok(f) = Feature::parse_at(self.list.data, off) {
                return Some((tag, f));
            }
        }
        None
    }
}

/// A single feature: a list of lookup indices to apply when the
/// feature is enabled.
#[derive(Debug, Clone, Copy)]
pub struct Feature<'a> {
    data: &'a [u8],
    lookup_indices_off: usize,
    lookup_index_count: u16,
}

impl<'a> Feature<'a> {
    fn parse_at(data: &'a [u8], offset: u16) -> Result<Self> {
        let base = offset as usize;
        if base >= data.len() {
            return Err(Error::Malformed {
                offset: base,
                context: "feature offset past end of FeatureList",
            });
        }
        let mut r = Reader::at(data, base)?;
        let _feature_params_offset = r.read_u16()?;
        let lookup_index_count = r.read_u16()?;
        let lookup_indices_off = r.position();
        let need = lookup_indices_off + lookup_index_count as usize * 2;
        if data.len() < need {
            return Err(Error::Truncated {
                offset: lookup_indices_off,
                context: "feature lookupListIndices shorter than count",
            });
        }
        Ok(Self {
            data,
            lookup_indices_off,
            lookup_index_count,
        })
    }

    /// Number of lookups this feature names.
    #[must_use]
    pub const fn len(&self) -> u16 {
        self.lookup_index_count
    }

    /// True if the feature names no lookups. Rare in practice but
    /// permitted by the spec.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.lookup_index_count == 0
    }

    /// Iterates the lookup indices (into the sibling `LookupList`).
    pub fn lookup_indices(&self) -> LookupIndexIter<'a> {
        LookupIndexIter {
            data: self.data,
            off: self.lookup_indices_off,
            remaining: self.lookup_index_count,
        }
    }
}

/// Iterator over a feature's lookup indices.
pub struct LookupIndexIter<'a> {
    data: &'a [u8],
    off: usize,
    remaining: u16,
}

impl Iterator for LookupIndexIter<'_> {
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

    /// Assembles a `FeatureList` where each feature record maps a
    /// tag to a body of (featureParamsOffset, count, indices).
    fn build_feature_list(records: &[([u8; 4], &[u16])]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&(records.len() as u16).to_be_bytes());
        let records_start = out.len();
        for _ in 0..records.len() {
            out.extend_from_slice(&[0u8; 6]);
        }
        for (i, (tag, indices)) in records.iter().enumerate() {
            let body_start = out.len();
            out.extend_from_slice(&0u16.to_be_bytes()); // featureParamsOffset
            out.extend_from_slice(&(indices.len() as u16).to_be_bytes());
            for idx in *indices {
                out.extend_from_slice(&idx.to_be_bytes());
            }
            let rec_off = records_start + i * 6;
            out[rec_off..rec_off + 4].copy_from_slice(tag);
            out[rec_off + 4..rec_off + 6].copy_from_slice(&(body_start as u16).to_be_bytes());
        }
        out
    }

    #[test]
    fn empty_feature_list_parses() {
        let bytes = build_feature_list(&[]);
        let fl = FeatureList::parse(&bytes).unwrap();
        assert!(fl.is_empty());
        assert_eq!(fl.len(), 0);
        assert!(fl.iter().next().is_none());
    }

    #[test]
    fn get_returns_tag_and_lookup_indices() {
        let bytes = build_feature_list(&[(*b"liga", &[0, 1, 2]), (*b"kern", &[5])]);
        let fl = FeatureList::parse(&bytes).unwrap();
        let (tag0, f0) = fl.get(0).unwrap();
        let (tag1, f1) = fl.get(1).unwrap();
        assert_eq!(tag0, *b"liga");
        assert_eq!(
            f0.lookup_indices().collect::<Vec<_>>(),
            alloc::vec![0, 1, 2]
        );
        assert_eq!(tag1, *b"kern");
        assert_eq!(f1.lookup_indices().collect::<Vec<_>>(), alloc::vec![5]);
    }

    #[test]
    fn get_returns_none_past_end() {
        let bytes = build_feature_list(&[(*b"liga", &[0])]);
        let fl = FeatureList::parse(&bytes).unwrap();
        assert!(fl.get(1).is_none());
        assert!(fl.get(u16::MAX).is_none());
    }

    #[test]
    fn iter_yields_records_in_order() {
        let bytes = build_feature_list(&[(*b"aaaa", &[0]), (*b"bbbb", &[1]), (*b"cccc", &[2])]);
        let fl = FeatureList::parse(&bytes).unwrap();
        let tags: Vec<_> = fl.iter().map(|(t, _)| t).collect();
        assert_eq!(tags, alloc::vec![*b"aaaa", *b"bbbb", *b"cccc"]);
    }

    #[test]
    fn feature_with_no_lookups_reports_empty() {
        let bytes = build_feature_list(&[(*b"liga", &[])]);
        let fl = FeatureList::parse(&bytes).unwrap();
        let (_, f) = fl.get(0).unwrap();
        assert!(f.is_empty());
        assert_eq!(f.len(), 0);
        assert!(f.lookup_indices().next().is_none());
    }

    #[test]
    fn rejects_truncated_records() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&3u16.to_be_bytes()); // claim 3 features
        bytes.extend_from_slice(&[0u8; 6]); // only provide 1 record
        assert!(matches!(
            FeatureList::parse(&bytes),
            Err(Error::Truncated { .. })
        ));
    }
}
