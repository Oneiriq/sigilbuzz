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
//! ```
//!
//! Format 3 (IVS-varied) is recognised by the format dispatcher and
//! its static coord is returned; full IVS resolution lands in a
//! follow-up commit alongside the v1.1 header path.

use alloc::vec::Vec;

use crate::error::{Error, Result};
use crate::tables::parse::Reader;

/// A parsed `BASE` table.
#[derive(Debug, Clone)]
pub struct Base<'a> {
    data: &'a [u8],
    horiz_axis_off: u16,
    vert_axis_off: u16,
}

impl<'a> Base<'a> {
    /// Parses a `BASE` table.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let major = r.read_u16()?;
        let _minor = r.read_u16()?;
        if major != 1 {
            return Err(Error::Malformed {
                offset: 0,
                context: "unsupported BASE major version",
            });
        }
        let horiz_axis_off = r.read_u16()?;
        let vert_axis_off = r.read_u16()?;

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
            BaseAxis::parse(self.data, self.horiz_axis_off as usize).ok()
        }
    }

    /// Vertical-text axis if the font carries one.
    #[must_use]
    pub fn vertical_axis(&self) -> Option<BaseAxis<'a>> {
        if self.vert_axis_off == 0 {
            None
        } else {
            BaseAxis::parse(self.data, self.vert_axis_off as usize).ok()
        }
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
}

impl<'a> BaseAxis<'a> {
    fn parse(data: &'a [u8], axis_off: usize) -> Result<Self> {
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
                return BaseScript::parse(self.data, script_off, tags).ok();
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
}

impl<'a> BaseScript<'a> {
    fn parse(data: &'a [u8], script_off: usize, tags: Vec<[u8; 4]>) -> Result<Self> {
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
        })
    }

    /// Returns the design-unit y-coordinate for `tag`, or `None`
    /// when the script carries no value for it.
    #[must_use]
    pub fn baseline(&self, tag: [u8; 4]) -> Option<i16> {
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
        // Skip to the slot's offset slot (each is 2 bytes).
        r.skip(slot * 2).ok()?;
        let coord_rel = r.read_u16().ok()?;
        if coord_rel == 0 {
            return None;
        }
        let coord_off = bv_off.checked_add(coord_rel as usize)?;
        read_base_coord_static(self.data, coord_off)
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

/// Reads the static coordinate of a BaseCoord at absolute offset
/// `off`. Recognises formats 1 and 2 (returning the embedded
/// coord); format 3's static coord is returned and the variation
/// index is ignored at this stage.
fn read_base_coord_static(data: &[u8], off: usize) -> Option<i16> {
    let mut r = Reader::at(data, off).ok()?;
    let format = r.read_u16().ok()?;
    let coord = r.read_i16().ok()?;
    match format {
        // Format 1: just the coord. Format 2: coord + reference
        // glyph + contour point (we ignore the latter two).
        // Format 3: coord + Offset16 device. We accept it here
        // and return the static value; full IVS resolution lands
        // when the v1.1 header path is wired up.
        1 | 2 | 3 => Some(coord),
        _ => None,
    }
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
}
