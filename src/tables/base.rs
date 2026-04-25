//! `BASE` — Baseline table.
//!
//! Provides per-script baseline metrics plus min/max boundaries so a
//! typesetting engine can align glyphs from different scripts on a
//! common typographic baseline. The classic example is mixed
//! Latin / CJK / Hebrew / math text, where each script wants a
//! different y-coordinate for its own baseline (`romn`, `ideo`,
//! `hang`, `math`, `hebr`, …) and the engine consults BASE to pick
//! one shared anchor and shift the rest accordingly.
//!
//! sigilbuzz exposes the parsed structure; the actual line-stacking
//! decision is the consumer's job. The table is small even in real
//! fonts (a handful of script entries with a few baseline tags
//! each), so the accessors do straightforward linear scans rather
//! than building lookup maps.
//!
//! # Layout
//!
//! ```text
//!   BASE Header:
//!     u16        majorVersion = 1
//!     u16        minorVersion = 0 or 1
//!     Offset16   horizAxisOffset           // 0 = absent
//!     Offset16   vertAxisOffset            // 0 = absent
//!     [Offset32  itemVarStoreOffset]       // v1.1 only (added later)
//!
//!   Axis (horizontal or vertical):
//!     Offset16   baseTagListOffset         // 0 = no tag list
//!     Offset16   baseScriptListOffset      // 0 = no script list
//!
//!   BaseTagList:
//!     u16        baseTagCount
//!     Tag        baselineTags[baseTagCount]      // sorted ascending
//!
//!   BaseScriptList:
//!     u16        baseScriptCount
//!     BaseScriptRecord records[baseScriptCount]
//!
//!   BaseScriptRecord:
//!     Tag        baseScriptTag
//!     Offset16   baseScriptOffset
//!
//!   BaseScript:
//!     Offset16   baseValuesOffset
//!     Offset16   defaultMinMaxOffset
//!     u16        baseLangSysCount
//!     BaseLangSysRecord langSysRecords[baseLangSysCount]
//!
//!   BaseValues:
//!     u16        defaultBaselineIndex      // index into baselineTags[]
//!     u16        baseCoordCount
//!     Offset16   baseCoords[baseCoordCount]
//!
//!   MinMax:
//!     Offset16   minCoordOffset
//!     Offset16   maxCoordOffset
//!     u16        featMinMaxCount
//!     FeatMinMaxRecord featMinMaxRecords[featMinMaxCount]
//!
//!   BaseCoord (format 1, static):
//!     u16        format = 1
//!     i16        coordinate
//!
//!   BaseCoord (format 2, contour-point — sigilbuzz returns the
//!   stored coordinate without resolving the point):
//!     u16        format = 2
//!     i16        coordinate
//!     u16        referenceGlyph
//!     u16        baseCoordPoint
//!
//!   BaseCoord (format 3, IVS-varied):
//!     u16        format = 3
//!     i16        coordinate
//!     Offset16   deviceTable / VariationIndex (relative to BaseCoord)
//! ```
//!
//! Format 3's device table is a `VariationIndex` triple
//! `(outerIndex, innerIndex, 0x8000)`; combined with the BASE
//! header's `itemVarStoreOffset` (v1.1) it yields the design-unit
//! delta to add to the static coordinate at a given normalized
//! axis position.

use alloc::vec::Vec;

use crate::error::{Error, Result};
use crate::tables::parse::Reader;
use crate::tables::variation_store::ItemVariationStore;

/// A parsed `BASE` table.
#[derive(Debug, Clone)]
pub struct Base<'a> {
    data: &'a [u8],
    horiz_axis_off: u16,
    vert_axis_off: u16,
    ivs: Option<ItemVariationStore<'a>>,
}

impl<'a> Base<'a> {
    /// Parses a `BASE` table.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let major = r.read_u16()?;
        let minor = r.read_u16()?;
        if major != 1 {
            return Err(Error::Malformed {
                offset: 0,
                context: "unsupported BASE major version",
            });
        }
        let horiz_axis_off = r.read_u16()?;
        let vert_axis_off = r.read_u16()?;

        // v1.1 adds a 32-bit ItemVariationStore offset. Older fonts
        // (the vast majority shipping BASE) end the header here.
        let ivs = if minor >= 1 {
            let store_off = r.read_u32()? as usize;
            if store_off == 0 {
                None
            } else {
                let bytes = data.get(store_off..).ok_or(Error::Malformed {
                    offset: store_off,
                    context: "BASE itemVarStore offset past end",
                })?;
                Some(ItemVariationStore::parse(bytes)?)
            }
        } else {
            None
        };

        if horiz_axis_off != 0 && (horiz_axis_off as usize) > data.len() {
            return Err(Error::Malformed {
                offset: 4,
                context: "BASE horizAxisOffset past end",
            });
        }
        if vert_axis_off != 0 && (vert_axis_off as usize) > data.len() {
            return Err(Error::Malformed {
                offset: 6,
                context: "BASE vertAxisOffset past end",
            });
        }

        Ok(Self {
            data,
            horiz_axis_off,
            vert_axis_off,
            ivs,
        })
    }

    /// Horizontal-text axis if the font carries one. Most fonts that
    /// ship BASE populate this and leave [`Self::vertical_axis`]
    /// empty.
    #[must_use]
    pub fn horizontal_axis(&self) -> Option<BaseAxis<'a>> {
        if self.horiz_axis_off == 0 {
            None
        } else {
            BaseAxis::parse(self.data, self.horiz_axis_off as usize, self.ivs.clone()).ok()
        }
    }

    /// Vertical-text axis if the font carries one.
    #[must_use]
    pub fn vertical_axis(&self) -> Option<BaseAxis<'a>> {
        if self.vert_axis_off == 0 {
            None
        } else {
            BaseAxis::parse(self.data, self.vert_axis_off as usize, self.ivs.clone()).ok()
        }
    }

    /// Borrowed reference to the v1.1 ItemVariationStore, when the
    /// font carries variable-font baselines. Static fonts and v1.0
    /// fonts return `None`.
    #[must_use]
    pub fn variation_store(&self) -> Option<&ItemVariationStore<'a>> {
        self.ivs.as_ref()
    }
}

/// One axis of the BASE table — either the horizontal-text axis or
/// the vertical-text axis. Carries an ordered list of baseline
/// tags and a per-script table of values keyed off those tags.
#[derive(Debug, Clone)]
pub struct BaseAxis<'a> {
    data: &'a [u8],
    /// Absolute offset of the BaseTagList from `data` start.
    /// Zero when the axis ships no tag list (rare but legal).
    tag_list_off: u16,
    /// Absolute offset of the BaseScriptList from `data` start.
    /// Zero when the axis carries scripts but no per-script values.
    script_list_off: u16,
    ivs: Option<ItemVariationStore<'a>>,
}

impl<'a> BaseAxis<'a> {
    fn parse(data: &'a [u8], axis_off: usize, ivs: Option<ItemVariationStore<'a>>) -> Result<Self> {
        let mut r = Reader::at(data, axis_off)?;
        let tag_list_rel = r.read_u16()?;
        let script_list_rel = r.read_u16()?;

        // Offsets in the spec are relative to the axis table start.
        // sigilbuzz stores the absolute positions to keep accessors
        // simple — they always slice from `data`.
        let tag_list_off = if tag_list_rel == 0 {
            0
        } else {
            let abs = axis_off
                .checked_add(tag_list_rel as usize)
                .ok_or(Error::Malformed {
                    offset: axis_off,
                    context: "BASE axis tag list offset overflow",
                })?;
            if abs > data.len() {
                return Err(Error::Malformed {
                    offset: axis_off,
                    context: "BASE axis tag list past end",
                });
            }
            abs as u16
        };
        let script_list_off = if script_list_rel == 0 {
            0
        } else {
            let abs = axis_off
                .checked_add(script_list_rel as usize)
                .ok_or(Error::Malformed {
                    offset: axis_off,
                    context: "BASE axis script list offset overflow",
                })?;
            if abs > data.len() {
                return Err(Error::Malformed {
                    offset: axis_off,
                    context: "BASE axis script list past end",
                });
            }
            abs as u16
        };

        Ok(Self {
            data,
            tag_list_off,
            script_list_off,
            ivs,
        })
    }

    /// Ordered baseline tags carried by the axis (e.g. `romn`,
    /// `ideo`, `hang`, `math`). Returns an empty `Vec` when the
    /// axis ships no tag list.
    #[must_use]
    pub fn baseline_tags(&self) -> Vec<[u8; 4]> {
        if self.tag_list_off == 0 {
            return Vec::new();
        }
        let off = self.tag_list_off as usize;
        let mut r = match Reader::at(self.data, off) {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };
        let count = match r.read_u16() {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };
        let mut tags = Vec::with_capacity(count as usize);
        for _ in 0..count {
            match r.read_tag() {
                Ok(t) => tags.push(t),
                Err(_) => break,
            }
        }
        tags
    }

    /// Looks up the per-script entry for `script_tag`. Returns
    /// `None` when the axis carries no script list, or when the
    /// list contains no record for the tag.
    #[must_use]
    pub fn script(&self, script_tag: [u8; 4]) -> Option<BaseScript<'a>> {
        if self.script_list_off == 0 {
            return None;
        }
        let list_off = self.script_list_off as usize;
        let mut r = Reader::at(self.data, list_off).ok()?;
        let count = r.read_u16().ok()?;
        // Each record is 4 (tag) + 2 (offset) = 6 bytes.
        for _ in 0..count {
            let tag = r.read_tag().ok()?;
            let script_rel = r.read_u16().ok()?;
            if tag == script_tag {
                if script_rel == 0 {
                    return None;
                }
                let script_off = list_off.checked_add(script_rel as usize)?;
                if script_off > self.data.len() {
                    return None;
                }
                let tags = self.baseline_tags();
                return BaseScript::parse(self.data, script_off, tags, self.ivs.clone()).ok();
            }
        }
        None
    }
}

/// Per-script baseline values. Backed by the axis's ordered tag
/// list — `baseline()` matches an incoming tag against the parent
/// axis's tag list, then reads the matching `BaseValues` slot.
#[derive(Debug, Clone)]
pub struct BaseScript<'a> {
    data: &'a [u8],
    /// Absolute offset of the BaseValues table, zero when absent.
    base_values_off: u16,
    /// Absolute offset of the default MinMax table, zero when absent.
    default_min_max_off: u16,
    /// The parent axis's ordered baseline tag list. Cached on the
    /// `BaseScript` so the `baseline()` lookup doesn't have to
    /// re-walk back through the axis.
    tags: Vec<[u8; 4]>,
    ivs: Option<ItemVariationStore<'a>>,
}

impl<'a> BaseScript<'a> {
    fn parse(
        data: &'a [u8],
        script_off: usize,
        tags: Vec<[u8; 4]>,
        ivs: Option<ItemVariationStore<'a>>,
    ) -> Result<Self> {
        let mut r = Reader::at(data, script_off)?;
        let base_values_rel = r.read_u16()?;
        let default_min_max_rel = r.read_u16()?;
        // baseLangSysCount + records follow but sigilbuzz doesn't
        // expose per-langSys MinMax overrides today — they're
        // exceedingly rare in real fonts.

        let base_values_off = if base_values_rel == 0 {
            0
        } else {
            let abs = script_off
                .checked_add(base_values_rel as usize)
                .ok_or(Error::Malformed {
                    offset: script_off,
                    context: "BASE BaseValues offset overflow",
                })?;
            if abs > data.len() {
                return Err(Error::Malformed {
                    offset: script_off,
                    context: "BASE BaseValues past end",
                });
            }
            abs as u16
        };
        let default_min_max_off = if default_min_max_rel == 0 {
            0
        } else {
            let abs = script_off
                .checked_add(default_min_max_rel as usize)
                .ok_or(Error::Malformed {
                    offset: script_off,
                    context: "BASE MinMax offset overflow",
                })?;
            if abs > data.len() {
                return Err(Error::Malformed {
                    offset: script_off,
                    context: "BASE MinMax past end",
                });
            }
            abs as u16
        };
        Ok(Self {
            data,
            base_values_off,
            default_min_max_off,
            tags,
            ivs,
        })
    }

    /// Returns the design-unit y-coordinate for `tag`, or `None`
    /// when the script carries no value for it. Reads the static
    /// coord; for variable fonts, [`Self::baseline_at_coords`] adds
    /// the IVS-derived delta.
    #[must_use]
    pub fn baseline(&self, tag: [u8; 4]) -> Option<i16> {
        let (coord, _ivs_idx) = self.coord_for_tag(tag)?;
        Some(coord)
    }

    /// Like [`Self::baseline`] but applies any v1.1 IVS deltas at
    /// the given normalized axis coordinates. When the BaseCoord
    /// is format 1 or format 2 — or the table has no IVS — this
    /// returns the same value as [`Self::baseline`].
    #[must_use]
    pub fn baseline_at_coords(&self, tag: [u8; 4], coords: &[f32]) -> Option<i16> {
        let (coord, ivs_idx) = self.coord_for_tag(tag)?;
        let Some((outer, inner)) = ivs_idx else {
            return Some(coord);
        };
        let Some(store) = self.ivs.as_ref() else {
            return Some(coord);
        };
        let delta = store.delta(outer, inner, coords);
        // Round-half-away-from-zero, saturating into i16. Matches
        // the convention used elsewhere in sigilbuzz for variable
        // metrics rounding.
        let adj = if delta >= 0.0 {
            delta + 0.5
        } else {
            delta - 0.5
        };
        #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
        let clamped = adj.max(i16::MIN as f32).min(i16::MAX as f32) as i16;
        Some(coord.saturating_add(clamped))
    }

    /// Resolves `(coordinate, optional VariationIndex)` for the
    /// baseline tag by consulting the parent axis's tag list. The
    /// `BaseValues` table stores per-coord offsets in the *same*
    /// order as the axis tag list, so we look up the tag's slot
    /// and read the `[slot]`th `BaseCoord`.
    fn coord_for_tag(&self, tag: [u8; 4]) -> Option<(i16, Option<(u16, u16)>)> {
        if self.base_values_off == 0 {
            return None;
        }
        let slot = self.tags.iter().position(|t| *t == tag)?;
        let bv_off = self.base_values_off as usize;
        let mut r = Reader::at(self.data, bv_off).ok()?;
        let _default_idx = r.read_u16().ok()?;
        let count = r.read_u16().ok()?;
        if slot >= count as usize {
            return None;
        }
        r.skip(slot * 2).ok()?;
        let coord_rel = r.read_u16().ok()?;
        if coord_rel == 0 {
            return None;
        }
        let coord_off = bv_off.checked_add(coord_rel as usize)?;
        read_base_coord(self.data, coord_off)
    }

    /// Returns the `(min, max)` design-unit clamps for the script.
    /// `feature_tag` selects a feature-specific override (e.g.
    /// `b"sups"` for superscripts) — pass `None` for the script's
    /// default range. Returns `None` when the script ships no
    /// MinMax entry.
    ///
    /// Spec note: when `feature_tag` is supplied but no record
    /// matches it, sigilbuzz falls back to the default range
    /// rather than returning `None`. That matches the line-stacking
    /// rule clients want — "give me the tightest available clamp,
    /// using the feature-specific one if present".
    #[must_use]
    pub fn min_max(&self, feature_tag: Option<[u8; 4]>) -> Option<(i16, i16)> {
        if self.default_min_max_off == 0 {
            return None;
        }
        let off = self.default_min_max_off as usize;
        let mut r = Reader::at(self.data, off).ok()?;
        let min_rel = r.read_u16().ok()?;
        let max_rel = r.read_u16().ok()?;
        let feat_count = r.read_u16().ok()?;

        if let Some(want) = feature_tag {
            for _ in 0..feat_count {
                let tag = r.read_tag().ok()?;
                let f_min_rel = r.read_u16().ok()?;
                let f_max_rel = r.read_u16().ok()?;
                if tag == want {
                    let mn = read_relative_coord(self.data, off, f_min_rel)?;
                    let mx = read_relative_coord(self.data, off, f_max_rel)?;
                    return Some((mn, mx));
                }
            }
        }

        let mn = read_relative_coord(self.data, off, min_rel)?;
        let mx = read_relative_coord(self.data, off, max_rel)?;
        Some((mn, mx))
    }
}

/// Reads a `BaseCoord` from `data` at absolute offset `off`. Returns
/// `(coordinate, Some((outer, inner)))` for format 3 with a
/// VariationIndex device, or `(coordinate, None)` for formats 1, 2,
/// and format 3 with a non-VariationIndex device.
fn read_base_coord(data: &[u8], off: usize) -> Option<(i16, Option<(u16, u16)>)> {
    let mut r = Reader::at(data, off).ok()?;
    let format = r.read_u16().ok()?;
    let coord = r.read_i16().ok()?;
    match format {
        1 => Some((coord, None)),
        2 => {
            // referenceGlyph + baseCoordPoint follow; sigilbuzz
            // returns the stored coord without consulting the
            // contour point. A renderer that needs hint-driven
            // baselines reaches for the glyph itself.
            Some((coord, None))
        }
        3 => {
            let dev_rel = r.read_u16().ok()?;
            if dev_rel == 0 {
                return Some((coord, None));
            }
            let dev_off = off.checked_add(dev_rel as usize)?;
            // VariationIndex layout: u16 outer, u16 inner, u16
            // deltaFormat = 0x8000.
            let mut dr = Reader::at(data, dev_off).ok()?;
            let outer = dr.read_u16().ok()?;
            let inner = dr.read_u16().ok()?;
            let fmt = dr.read_u16().ok()?;
            if fmt != 0x8000 {
                // Plain Device table — sigilbuzz doesn't apply
                // ppem-keyed adjustments to baselines, so treat
                // as static.
                return Some((coord, None));
            }
            Some((coord, Some((outer, inner))))
        }
        _ => None,
    }
}

/// Reads the static coordinate of a BaseCoord at absolute offset
/// `off`. Drops the variation index — used by `min_max`, which
/// doesn't expose IVS-varied clamps (the few real fonts shipping
/// these mark them static).
fn read_base_coord_static(data: &[u8], off: usize) -> Option<i16> {
    let (coord, _) = read_base_coord(data, off)?;
    Some(coord)
}

/// Reads the static coordinate at `base_off + rel`, or `None` when
/// `rel == 0`. Used by `min_max` for both the default and the
/// per-feature offsets.
fn read_relative_coord(data: &[u8], base_off: usize, rel: u16) -> Option<i16> {
    if rel == 0 {
        return None;
    }
    let off = base_off.checked_add(rel as usize)?;
    read_base_coord_static(data, off)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u16be(v: u16) -> [u8; 2] {
        v.to_be_bytes()
    }
    fn i16be(v: i16) -> [u8; 2] {
        v.to_be_bytes()
    }

    /// Builds a minimal BASE v1.0 with one horizontal axis carrying
    /// one script (`latn`) and one baseline tag (`romn`). When
    /// `min_max` is `Some`, also embeds a default MinMax with those
    /// (min, max) bounds.
    fn build_minimal_base(min_max: Option<(i16, i16)>, baseline_y: i16) -> Vec<u8> {
        let mut out: Vec<u8> = Vec::new();

        // Header.
        out.extend_from_slice(&u16be(1));
        out.extend_from_slice(&u16be(0));
        out.extend_from_slice(&u16be(8));
        out.extend_from_slice(&u16be(0));

        // Horizontal axis (offset 8).
        let axis_off = out.len();
        let tag_list_slot = axis_off;
        out.extend_from_slice(&u16be(0));
        out.extend_from_slice(&u16be(0));

        let tag_list_off = out.len();
        out.extend_from_slice(&u16be(1));
        out.extend_from_slice(b"romn");

        let script_list_off = out.len();
        out.extend_from_slice(&u16be(1));
        out.extend_from_slice(b"latn");
        let script_off_slot = out.len();
        out.extend_from_slice(&u16be(0));

        let script_off = out.len();
        let base_values_slot = out.len();
        out.extend_from_slice(&u16be(0));
        let default_min_max_slot = out.len();
        out.extend_from_slice(&u16be(0));
        out.extend_from_slice(&u16be(0));

        let base_values_off = out.len();
        out.extend_from_slice(&u16be(0));
        out.extend_from_slice(&u16be(1));
        let coord_off_slot = out.len();
        out.extend_from_slice(&u16be(0));

        let coord_off = out.len();
        out.extend_from_slice(&u16be(1));
        out.extend_from_slice(&i16be(baseline_y));

        let min_max_off = if let Some((mn, mx)) = min_max {
            let mm_off = out.len();
            let mm_min_slot = out.len();
            out.extend_from_slice(&u16be(0));
            let mm_max_slot = out.len();
            out.extend_from_slice(&u16be(0));
            out.extend_from_slice(&u16be(0));

            let min_coord_off = out.len();
            out.extend_from_slice(&u16be(1));
            out.extend_from_slice(&i16be(mn));
            let max_coord_off = out.len();
            out.extend_from_slice(&u16be(1));
            out.extend_from_slice(&i16be(mx));

            out[mm_min_slot..mm_min_slot + 2]
                .copy_from_slice(&u16be((min_coord_off - mm_off) as u16));
            out[mm_max_slot..mm_max_slot + 2]
                .copy_from_slice(&u16be((max_coord_off - mm_off) as u16));
            Some(mm_off)
        } else {
            None
        };

        out[tag_list_slot..tag_list_slot + 2]
            .copy_from_slice(&u16be((tag_list_off - axis_off) as u16));
        out[tag_list_slot + 2..tag_list_slot + 4]
            .copy_from_slice(&u16be((script_list_off - axis_off) as u16));
        out[script_off_slot..script_off_slot + 2]
            .copy_from_slice(&u16be((script_off - script_list_off) as u16));
        out[base_values_slot..base_values_slot + 2]
            .copy_from_slice(&u16be((base_values_off - script_off) as u16));
        out[coord_off_slot..coord_off_slot + 2]
            .copy_from_slice(&u16be((coord_off - base_values_off) as u16));
        if let Some(mm_off) = min_max_off {
            out[default_min_max_slot..default_min_max_slot + 2]
                .copy_from_slice(&u16be((mm_off - script_off) as u16));
        }
        out
    }

    #[test]
    fn parses_minimal_base_table() {
        let data = build_minimal_base(None, 0);
        let base = Base::parse(&data).unwrap();
        assert!(base.horizontal_axis().is_some());
        assert!(base.vertical_axis().is_none());
    }

    #[test]
    fn rejects_unsupported_major_version() {
        let mut data = build_minimal_base(None, 0);
        data[0..2].copy_from_slice(&2u16.to_be_bytes());
        assert!(matches!(Base::parse(&data), Err(Error::Malformed { .. })));
    }

    #[test]
    fn axis_returns_baseline_tags_in_order() {
        let data = build_minimal_base(None, 0);
        let base = Base::parse(&data).unwrap();
        let axis = base.horizontal_axis().unwrap();
        assert_eq!(axis.baseline_tags(), alloc::vec![*b"romn"]);
    }

    #[test]
    fn axis_returns_none_for_unknown_script() {
        let data = build_minimal_base(None, 0);
        let base = Base::parse(&data).unwrap();
        let axis = base.horizontal_axis().unwrap();
        assert!(axis.script(*b"hang").is_none());
    }

    #[test]
    fn script_returns_baseline_for_known_tag() {
        let data = build_minimal_base(None, 100);
        let base = Base::parse(&data).unwrap();
        let axis = base.horizontal_axis().unwrap();
        let script = axis.script(*b"latn").unwrap();
        assert_eq!(script.baseline(*b"romn"), Some(100));
        assert_eq!(script.baseline(*b"hang"), None);
    }

    #[test]
    fn script_returns_min_max_when_present() {
        let data = build_minimal_base(Some((-200, 800)), 0);
        let base = Base::parse(&data).unwrap();
        let axis = base.horizontal_axis().unwrap();
        let script = axis.script(*b"latn").unwrap();
        assert_eq!(script.min_max(None), Some((-200, 800)));
    }

    #[test]
    fn script_returns_none_for_min_max_when_absent() {
        let data = build_minimal_base(None, 0);
        let base = Base::parse(&data).unwrap();
        let axis = base.horizontal_axis().unwrap();
        let script = axis.script(*b"latn").unwrap();
        assert!(script.min_max(None).is_none());
    }

    #[test]
    fn missing_axis_offset_yields_none() {
        let mut data = Vec::new();
        data.extend_from_slice(&u16be(1));
        data.extend_from_slice(&u16be(0));
        data.extend_from_slice(&u16be(0));
        data.extend_from_slice(&u16be(0));
        let base = Base::parse(&data).unwrap();
        assert!(base.horizontal_axis().is_none());
        assert!(base.vertical_axis().is_none());
    }

    /// Multi-tag, multi-script BASE: two baseline tags (`romn`,
    /// `ideo`) and two scripts (`latn` and `hani`) carrying
    /// distinct y-coords for both tags. Exercises the slot-into-
    /// tag-list lookup and the script scan.
    fn build_multi_base() -> Vec<u8> {
        let mut out: Vec<u8> = Vec::new();
        out.extend_from_slice(&u16be(1));
        out.extend_from_slice(&u16be(0));
        out.extend_from_slice(&u16be(8));
        out.extend_from_slice(&u16be(0));

        let axis_off = out.len();
        let tag_list_slot = axis_off;
        out.extend_from_slice(&u16be(0));
        out.extend_from_slice(&u16be(0));

        let tag_list_off = out.len();
        out.extend_from_slice(&u16be(2));
        out.extend_from_slice(b"romn");
        out.extend_from_slice(b"ideo");

        let script_list_off = out.len();
        out.extend_from_slice(&u16be(2));
        out.extend_from_slice(b"latn");
        let latn_off_slot = out.len();
        out.extend_from_slice(&u16be(0));
        out.extend_from_slice(b"hani");
        let hani_off_slot = out.len();
        out.extend_from_slice(&u16be(0));

        let latn_script_off = out.len();
        let latn_bv_slot = out.len();
        out.extend_from_slice(&u16be(0));
        out.extend_from_slice(&u16be(0));
        out.extend_from_slice(&u16be(0));

        let latn_bv_off = out.len();
        out.extend_from_slice(&u16be(0));
        out.extend_from_slice(&u16be(2));
        let latn_c0_slot = out.len();
        out.extend_from_slice(&u16be(0));
        let latn_c1_slot = out.len();
        out.extend_from_slice(&u16be(0));

        let latn_c0_off = out.len();
        out.extend_from_slice(&u16be(1));
        out.extend_from_slice(&i16be(0));
        let latn_c1_off = out.len();
        out.extend_from_slice(&u16be(1));
        out.extend_from_slice(&i16be(-120));

        let hani_script_off = out.len();
        let hani_bv_slot = out.len();
        out.extend_from_slice(&u16be(0));
        out.extend_from_slice(&u16be(0));
        out.extend_from_slice(&u16be(0));

        let hani_bv_off = out.len();
        out.extend_from_slice(&u16be(1));
        out.extend_from_slice(&u16be(2));
        let hani_c0_slot = out.len();
        out.extend_from_slice(&u16be(0));
        let hani_c1_slot = out.len();
        out.extend_from_slice(&u16be(0));

        let hani_c0_off = out.len();
        out.extend_from_slice(&u16be(1));
        out.extend_from_slice(&i16be(120));
        let hani_c1_off = out.len();
        out.extend_from_slice(&u16be(1));
        out.extend_from_slice(&i16be(0));

        out[tag_list_slot..tag_list_slot + 2]
            .copy_from_slice(&u16be((tag_list_off - axis_off) as u16));
        out[tag_list_slot + 2..tag_list_slot + 4]
            .copy_from_slice(&u16be((script_list_off - axis_off) as u16));
        out[latn_off_slot..latn_off_slot + 2]
            .copy_from_slice(&u16be((latn_script_off - script_list_off) as u16));
        out[hani_off_slot..hani_off_slot + 2]
            .copy_from_slice(&u16be((hani_script_off - script_list_off) as u16));
        out[latn_bv_slot..latn_bv_slot + 2]
            .copy_from_slice(&u16be((latn_bv_off - latn_script_off) as u16));
        out[hani_bv_slot..hani_bv_slot + 2]
            .copy_from_slice(&u16be((hani_bv_off - hani_script_off) as u16));
        out[latn_c0_slot..latn_c0_slot + 2]
            .copy_from_slice(&u16be((latn_c0_off - latn_bv_off) as u16));
        out[latn_c1_slot..latn_c1_slot + 2]
            .copy_from_slice(&u16be((latn_c1_off - latn_bv_off) as u16));
        out[hani_c0_slot..hani_c0_slot + 2]
            .copy_from_slice(&u16be((hani_c0_off - hani_bv_off) as u16));
        out[hani_c1_slot..hani_c1_slot + 2]
            .copy_from_slice(&u16be((hani_c1_off - hani_bv_off) as u16));
        out
    }

    #[test]
    fn multi_script_baseline_lookup() {
        let data = build_multi_base();
        let base = Base::parse(&data).unwrap();
        let axis = base.horizontal_axis().unwrap();
        assert_eq!(axis.baseline_tags(), alloc::vec![*b"romn", *b"ideo"]);

        let latn = axis.script(*b"latn").unwrap();
        assert_eq!(latn.baseline(*b"romn"), Some(0));
        assert_eq!(latn.baseline(*b"ideo"), Some(-120));

        let hani = axis.script(*b"hani").unwrap();
        assert_eq!(hani.baseline(*b"romn"), Some(120));
        assert_eq!(hani.baseline(*b"ideo"), Some(0));
    }

    /// MinMax with a per-feature override: `sups` (superscripts)
    /// gets a tighter clamp than the script default; an unknown
    /// feature tag falls back to the default range.
    fn build_base_with_feature_minmax() -> Vec<u8> {
        let mut out: Vec<u8> = Vec::new();
        out.extend_from_slice(&u16be(1));
        out.extend_from_slice(&u16be(0));
        out.extend_from_slice(&u16be(8));
        out.extend_from_slice(&u16be(0));

        let axis_off = out.len();
        let tl_slot = axis_off;
        out.extend_from_slice(&u16be(0));
        out.extend_from_slice(&u16be(0));

        let tag_list_off = out.len();
        out.extend_from_slice(&u16be(1));
        out.extend_from_slice(b"romn");

        let script_list_off = out.len();
        out.extend_from_slice(&u16be(1));
        out.extend_from_slice(b"latn");
        let s_slot = out.len();
        out.extend_from_slice(&u16be(0));

        let script_off = out.len();
        out.extend_from_slice(&u16be(0));
        let mm_slot = out.len();
        out.extend_from_slice(&u16be(0));
        out.extend_from_slice(&u16be(0));

        let mm_off = out.len();
        let mm_min_slot = out.len();
        out.extend_from_slice(&u16be(0));
        let mm_max_slot = out.len();
        out.extend_from_slice(&u16be(0));
        out.extend_from_slice(&u16be(1));
        out.extend_from_slice(b"sups");
        let f_min_slot = out.len();
        out.extend_from_slice(&u16be(0));
        let f_max_slot = out.len();
        out.extend_from_slice(&u16be(0));

        let dmin = out.len();
        out.extend_from_slice(&u16be(1));
        out.extend_from_slice(&i16be(-200));
        let dmax = out.len();
        out.extend_from_slice(&u16be(1));
        out.extend_from_slice(&i16be(800));
        let fmin = out.len();
        out.extend_from_slice(&u16be(1));
        out.extend_from_slice(&i16be(-50));
        let fmax = out.len();
        out.extend_from_slice(&u16be(1));
        out.extend_from_slice(&i16be(600));

        out[tl_slot..tl_slot + 2].copy_from_slice(&u16be((tag_list_off - axis_off) as u16));
        out[tl_slot + 2..tl_slot + 4]
            .copy_from_slice(&u16be((script_list_off - axis_off) as u16));
        out[s_slot..s_slot + 2].copy_from_slice(&u16be((script_off - script_list_off) as u16));
        out[mm_slot..mm_slot + 2].copy_from_slice(&u16be((mm_off - script_off) as u16));
        out[mm_min_slot..mm_min_slot + 2].copy_from_slice(&u16be((dmin - mm_off) as u16));
        out[mm_max_slot..mm_max_slot + 2].copy_from_slice(&u16be((dmax - mm_off) as u16));
        out[f_min_slot..f_min_slot + 2].copy_from_slice(&u16be((fmin - mm_off) as u16));
        out[f_max_slot..f_max_slot + 2].copy_from_slice(&u16be((fmax - mm_off) as u16));
        out
    }

    #[test]
    fn min_max_feature_override_wins_for_known_tag() {
        let data = build_base_with_feature_minmax();
        let base = Base::parse(&data).unwrap();
        let axis = base.horizontal_axis().unwrap();
        let script = axis.script(*b"latn").unwrap();
        assert_eq!(script.min_max(None), Some((-200, 800)));
        assert_eq!(script.min_max(Some(*b"sups")), Some((-50, 600)));
        assert_eq!(script.min_max(Some(*b"subs")), Some((-200, 800)));
    }

    // --------------------------------------------------------------
    // v1.1 IVS-varied baseline (BaseCoord format 3).
    // --------------------------------------------------------------

    fn write_f2dot14(out: &mut Vec<u8>, v: f32) {
        #[allow(clippy::cast_possible_truncation)]
        let raw = (v * 16384.0).round() as i16;
        out.extend_from_slice(&raw.to_be_bytes());
    }

    /// Same one-axis, one-region, one-item IVS used by the
    /// `mvar`/`hvar` tests. Maps `(outer=0, inner=0)` to the given
    /// `delta` at the +1.0 axis tip, tapering linearly to 0 at
    /// the default position.
    fn build_ivs_one_item(delta: i16) -> Vec<u8> {
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
        out.extend_from_slice(&1u16.to_be_bytes()); // wordDeltaCount
        out.extend_from_slice(&1u16.to_be_bytes()); // regionIndexCount
        out.extend_from_slice(&0u16.to_be_bytes()); // region index 0
        out.extend_from_slice(&delta.to_be_bytes());
        out
    }

    /// Builds a v1.1 BASE with one horizontal axis, one script
    /// (`latn`), one tag (`romn`), and a format-3 BaseCoord whose
    /// VariationIndex points at the single `(outer=0, inner=0)`
    /// item in the embedded IVS.
    fn build_v11_base(static_y: i16, ivs_delta: i16) -> Vec<u8> {
        let mut out: Vec<u8> = Vec::new();

        // Header (v1.1: 12 bytes).
        out.extend_from_slice(&u16be(1)); // major
        out.extend_from_slice(&u16be(1)); // minor
        out.extend_from_slice(&u16be(12)); // horizAxisOffset
        out.extend_from_slice(&u16be(0));
        let ivs_slot = out.len();
        out.extend_from_slice(&0u32.to_be_bytes()); // itemVarStoreOffset placeholder

        let axis_off = out.len();
        let tl_slot = axis_off;
        out.extend_from_slice(&u16be(0));
        out.extend_from_slice(&u16be(0));

        let tag_list_off = out.len();
        out.extend_from_slice(&u16be(1));
        out.extend_from_slice(b"romn");

        let script_list_off = out.len();
        out.extend_from_slice(&u16be(1));
        out.extend_from_slice(b"latn");
        let s_slot = out.len();
        out.extend_from_slice(&u16be(0));

        let script_off = out.len();
        let bv_slot = out.len();
        out.extend_from_slice(&u16be(0));
        out.extend_from_slice(&u16be(0));
        out.extend_from_slice(&u16be(0));

        let bv_off = out.len();
        out.extend_from_slice(&u16be(0));
        out.extend_from_slice(&u16be(1));
        let coord_slot = out.len();
        out.extend_from_slice(&u16be(0));

        // Format 3 BaseCoord: u16 fmt, i16 coord, Offset16 device.
        let coord_off = out.len();
        out.extend_from_slice(&u16be(3));
        out.extend_from_slice(&i16be(static_y));
        let dev_slot = out.len();
        out.extend_from_slice(&u16be(0));

        // VariationIndex (inline, pointed at by dev_slot).
        let var_index_off = out.len();
        out.extend_from_slice(&u16be(0)); // outer
        out.extend_from_slice(&u16be(0)); // inner
        out.extend_from_slice(&u16be(0x8000)); // deltaFormat = VARIATION_INDEX

        out[dev_slot..dev_slot + 2]
            .copy_from_slice(&u16be((var_index_off - coord_off) as u16));

        out[tl_slot..tl_slot + 2].copy_from_slice(&u16be((tag_list_off - axis_off) as u16));
        out[tl_slot + 2..tl_slot + 4]
            .copy_from_slice(&u16be((script_list_off - axis_off) as u16));
        out[s_slot..s_slot + 2].copy_from_slice(&u16be((script_off - script_list_off) as u16));
        out[bv_slot..bv_slot + 2].copy_from_slice(&u16be((bv_off - script_off) as u16));
        out[coord_slot..coord_slot + 2].copy_from_slice(&u16be((coord_off - bv_off) as u16));

        let ivs_off = out.len() as u32;
        out[ivs_slot..ivs_slot + 4].copy_from_slice(&ivs_off.to_be_bytes());
        out.extend_from_slice(&build_ivs_one_item(ivs_delta));

        out
    }

    #[test]
    fn v11_static_baseline_is_unchanged_at_default_coord() {
        let data = build_v11_base(50, 30);
        let base = Base::parse(&data).unwrap();
        assert!(base.variation_store().is_some());
        let axis = base.horizontal_axis().unwrap();
        let script = axis.script(*b"latn").unwrap();
        // baseline() always returns the static coord.
        assert_eq!(script.baseline(*b"romn"), Some(50));
        // At coord 0 the IVS region (0..1..1) yields scalar 0 → no
        // delta.
        assert_eq!(script.baseline_at_coords(*b"romn", &[0.0]), Some(50));
    }

    #[test]
    fn v11_baseline_picks_up_ivs_delta_at_max_coord() {
        let data = build_v11_base(50, 30);
        let base = Base::parse(&data).unwrap();
        let axis = base.horizontal_axis().unwrap();
        let script = axis.script(*b"latn").unwrap();
        // At coord 1.0 the region yields scalar 1.0 and the delta
        // is 30 → 50 + 30 = 80.
        assert_eq!(script.baseline_at_coords(*b"romn", &[1.0]), Some(80));
        // At coord 0.5 the linear taper gives 50 + 15 = 65.
        assert_eq!(script.baseline_at_coords(*b"romn", &[0.5]), Some(65));
    }

    #[test]
    fn v11_baseline_at_coords_without_ivs_returns_static() {
        // A v1.0 fixture has no IVS; baseline_at_coords should
        // still return the static coord rather than failing.
        let data = build_minimal_base(None, 42);
        let base = Base::parse(&data).unwrap();
        let axis = base.horizontal_axis().unwrap();
        let script = axis.script(*b"latn").unwrap();
        assert_eq!(script.baseline_at_coords(*b"romn", &[0.5]), Some(42));
    }
}
