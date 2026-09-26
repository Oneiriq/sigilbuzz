//! `BASE`: Baseline table.
//!
//! Provides per-script baseline metrics plus min/max boundaries so a
//! typesetting engine can align glyphs from different scripts on a
//! common typographic baseline. The classic example is mixed
//! Latin / CJK / Hebrew / math text, where each script wants a
//! different y-coordinate for its own baseline (`romn`, `ideo`,
//! `hang`, `math`, `hebr`, ...) and the engine consults BASE to pick
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
//!   BaseCoord (format 2, contour-point: sigilbuzz returns the
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

/// One axis of the BASE table: either the horizontal-text axis or
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
        // simple: they always slice from `data`.
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
        let Ok(mut r) = Reader::at(self.data, off) else {
            return Vec::new();
        };
        let Ok(count) = r.read_u16() else {
            return Vec::new();
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
/// list: `baseline()` matches an incoming tag against the parent
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
        // expose per-langSys MinMax overrides today. They're
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
            let abs =
                script_off
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
    /// is format 1 or format 2 (or the table has no IVS), this
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
    /// `b"sups"` for superscripts). Pass `None` for the script's
    /// default range. Returns `None` when the script ships no
    /// MinMax entry.
    ///
    /// Spec note: when `feature_tag` is supplied but no record
    /// matches it, sigilbuzz falls back to the default range
    /// rather than returning `None`. That matches the line-stacking
    /// rule clients want: "give me the tightest available clamp,
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
                // Plain Device table: sigilbuzz doesn't apply
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
/// `off`. Drops the variation index. Used by `min_max`, which
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
mod tests;
