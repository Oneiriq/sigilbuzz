//! `MATH` — OpenType math typography table.
//!
//! Math fonts (STIX 2 Math, Latin Modern Math, Cambria Math, Asana Math,
//! XITS Math, …) ship a `MATH` table that math-typesetting engines like
//! LuaTeX and MathML renderers consume to lay out equations. The table
//! is divided into five logically distinct subtables:
//!
//! - [`MathConstants`] — ~70 font-wide layout constants (script scale
//!   percentages, fraction-rule shifts, radical inset, etc.).
//! - [`MathGlyphInfo`] — per-glyph italic correction, top-accent
//!   attachment, an "is extended shape" bitmap, and per-corner kerning.
//! - [`MathKern`] — piecewise math kerning that varies with the
//!   secondary glyph's vertical position.
//! - [`MathVariants`] — stretchy-glyph variant lists and assembly
//!   parts for tall operators (∑ ∫ ⎰ ⎱ ⎛ ⎜ ⎝ …).
//!
//! sigilbuzz parses the data; *evaluating* it (running a math layout
//! pass) is the consumer's job, just like `COLR` paint evaluation. All
//! views are zero-copy, borrowing `&'a [u8]` into the original `MATH`
//! table bytes.
//!
//! Reference: <https://learn.microsoft.com/en-us/typography/opentype/spec/math>.

use crate::error::{Error, Result};
use crate::tables::layout::{Coverage, DeviceOrVariationIndex};
use crate::tables::parse::Reader;

// =========================================================================
// MathValueRecord
// =========================================================================

/// A `MathValueRecord` — every numeric field in the MATH table is one
/// of these. The `value` is a design-unit (`FWord`) scalar; the optional
/// `device` references a Device or VariationIndex table that adjusts
/// the value at runtime (per-ppem hinting deltas or variable-font
/// axis-driven deltas, respectively).
///
/// The Device offset is parsed lazily into [`DeviceOrVariationIndex`];
/// callers that only need the design-unit value can ignore it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MathValue<'a> {
    /// Design-unit value of the record.
    pub value: i16,
    /// Resolved Device / VariationIndex sub-table, or `None` when the
    /// font emits the spec's "absent" sentinel (offset = 0).
    pub device: Option<DeviceOrVariationIndex>,
    /// Slice of the enclosing MATH table — kept so callers can later
    /// resolve any embedded VariationIndex against an
    /// `ItemVariationStore` if one is wired in.
    _table: &'a [u8],
}

// MathValueRecord is 4 bytes (i16 value + u16 Device/VariationIndex
// offset relative to the enclosing subtable). Each subtable inlines
// the read so it can use the right `data` slice as the device base.

// =========================================================================
// MATH header
// =========================================================================

/// Parsed `MATH` table.
///
/// The header is just three offsets: MathConstants, MathGlyphInfo,
/// MathVariants. Each is parsed lazily on the matching accessor so a
/// font missing one of them costs nothing.
#[derive(Debug, Clone, Copy)]
pub struct Math<'a> {
    data: &'a [u8],
    constants_off: u16,
    glyph_info_off: u16,
    variants_off: u16,
}

impl<'a> Math<'a> {
    /// Parses a `MATH` table header.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let major = r.read_u16()?;
        let minor = r.read_u16()?;
        if major != 1 {
            return Err(Error::Malformed {
                offset: 0,
                context: "unsupported MATH major version",
            });
        }
        let _ = minor;
        let constants_off = r.read_u16()?;
        let glyph_info_off = r.read_u16()?;
        let variants_off = r.read_u16()?;

        // Validate non-zero offsets fit inside the table.
        for (off, ctx) in [
            (constants_off, "MathConstants offset past end"),
            (glyph_info_off, "MathGlyphInfo offset past end"),
            (variants_off, "MathVariants offset past end"),
        ] {
            if off != 0 && off as usize > data.len() {
                return Err(Error::Malformed {
                    offset: off as usize,
                    context: ctx,
                });
            }
        }

        Ok(Self {
            data,
            constants_off,
            glyph_info_off,
            variants_off,
        })
    }

    /// Returns the [`MathConstants`] subtable, or `None` when the font
    /// does not provide one (rare — every real math font ships it).
    pub fn constants(&self) -> Result<Option<MathConstants<'a>>> {
        if self.constants_off == 0 {
            return Ok(None);
        }
        let bytes = self
            .data
            .get(self.constants_off as usize..)
            .ok_or(Error::Malformed {
                offset: self.constants_off as usize,
                context: "MathConstants offset past end",
            })?;
        MathConstants::parse(bytes).map(Some)
    }

    /// Returns the [`MathGlyphInfo`] subtable, or `None` when absent.
    pub fn glyph_info(&self) -> Result<Option<MathGlyphInfo<'a>>> {
        if self.glyph_info_off == 0 {
            return Ok(None);
        }
        let bytes = self
            .data
            .get(self.glyph_info_off as usize..)
            .ok_or(Error::Malformed {
                offset: self.glyph_info_off as usize,
                context: "MathGlyphInfo offset past end",
            })?;
        MathGlyphInfo::parse(bytes).map(Some)
    }

    /// Returns the [`MathVariants`] subtable, or `None` when absent.
    /// Fonts without stretchy operator support omit this.
    pub fn variants(&self) -> Result<Option<MathVariants<'a>>> {
        if self.variants_off == 0 {
            return Ok(None);
        }
        let bytes = self
            .data
            .get(self.variants_off as usize..)
            .ok_or(Error::Malformed {
                offset: self.variants_off as usize,
                context: "MathVariants offset past end",
            })?;
        MathVariants::parse(bytes).map(Some)
    }
}

// =========================================================================
// MathConstants
// =========================================================================

/// MATH constants header layout (offsets in bytes from the start of the
/// MathConstants subtable). The first four fields are bare `i16`s; every
/// subsequent constant is a 4-byte MathValueRecord.
const SCRIPT_PERCENT_SCALE_DOWN: usize = 0;
const SCRIPT_SCRIPT_PERCENT_SCALE_DOWN: usize = 2;
const DELIMITED_SUB_FORMULA_MIN_HEIGHT: usize = 4; // u16 (UFWord)
const DISPLAY_OPERATOR_MIN_HEIGHT: usize = 6; // u16 (UFWord)
const FIRST_VALUE_RECORD: usize = 8;
/// Number of MathValueRecord fields after the four scalar header fields.
const NUM_VALUE_RECORDS: usize = 51;
const MATH_CONSTANTS_LEN: usize =
    FIRST_VALUE_RECORD + NUM_VALUE_RECORDS * 4 + 2 /* RadicalDegreeBottomRaisePercent */;

/// Indices into the MathValueRecord block (relative to
/// [`FIRST_VALUE_RECORD`]). Order matches the OpenType MATH spec.
#[allow(missing_docs)]
mod c_idx {
    pub const MATH_LEADING: usize = 0;
    pub const AXIS_HEIGHT: usize = 1;
    pub const ACCENT_BASE_HEIGHT: usize = 2;
    pub const FLATTENED_ACCENT_BASE_HEIGHT: usize = 3;
    pub const SUBSCRIPT_SHIFT_DOWN: usize = 4;
    pub const SUBSCRIPT_TOP_MAX: usize = 5;
    pub const SUBSCRIPT_BASELINE_DROP_MIN: usize = 6;
    pub const SUPERSCRIPT_SHIFT_UP: usize = 7;
    pub const SUPERSCRIPT_SHIFT_UP_CRAMPED: usize = 8;
    pub const SUPERSCRIPT_BOTTOM_MIN: usize = 9;
    pub const SUPERSCRIPT_BASELINE_DROP_MAX: usize = 10;
    pub const SUB_SUPERSCRIPT_GAP_MIN: usize = 11;
    pub const SUPERSCRIPT_BOTTOM_MAX_WITH_SUBSCRIPT: usize = 12;
    pub const SPACE_AFTER_SCRIPT: usize = 13;
    pub const UPPER_LIMIT_GAP_MIN: usize = 14;
    pub const UPPER_LIMIT_BASELINE_RISE_MIN: usize = 15;
    pub const LOWER_LIMIT_GAP_MIN: usize = 16;
    pub const LOWER_LIMIT_BASELINE_DROP_MIN: usize = 17;
    pub const STACK_TOP_SHIFT_UP: usize = 18;
    pub const STACK_TOP_DISPLAY_STYLE_SHIFT_UP: usize = 19;
    pub const STACK_BOTTOM_SHIFT_DOWN: usize = 20;
    pub const STACK_BOTTOM_DISPLAY_STYLE_SHIFT_DOWN: usize = 21;
    pub const STACK_GAP_MIN: usize = 22;
    pub const STACK_DISPLAY_STYLE_GAP_MIN: usize = 23;
    pub const STRETCH_STACK_TOP_SHIFT_UP: usize = 24;
    pub const STRETCH_STACK_BOTTOM_SHIFT_DOWN: usize = 25;
    pub const STRETCH_STACK_GAP_ABOVE_MIN: usize = 26;
    pub const STRETCH_STACK_GAP_BELOW_MIN: usize = 27;
    pub const FRACTION_NUMERATOR_SHIFT_UP: usize = 28;
    pub const FRACTION_NUMERATOR_DISPLAY_STYLE_SHIFT_UP: usize = 29;
    pub const FRACTION_DENOMINATOR_SHIFT_DOWN: usize = 30;
    pub const FRACTION_DENOMINATOR_DISPLAY_STYLE_SHIFT_DOWN: usize = 31;
    pub const FRACTION_NUMERATOR_GAP_MIN: usize = 32;
    pub const FRACTION_NUM_DISPLAY_STYLE_GAP_MIN: usize = 33;
    pub const FRACTION_RULE_THICKNESS: usize = 34;
    pub const FRACTION_DENOMINATOR_GAP_MIN: usize = 35;
    pub const FRACTION_DENOM_DISPLAY_STYLE_GAP_MIN: usize = 36;
    pub const SKEWED_FRACTION_HORIZONTAL_GAP: usize = 37;
    pub const SKEWED_FRACTION_VERTICAL_GAP: usize = 38;
    pub const OVERBAR_VERTICAL_GAP: usize = 39;
    pub const OVERBAR_RULE_THICKNESS: usize = 40;
    pub const OVERBAR_EXTRA_ASCENDER: usize = 41;
    pub const UNDERBAR_VERTICAL_GAP: usize = 42;
    pub const UNDERBAR_RULE_THICKNESS: usize = 43;
    pub const UNDERBAR_EXTRA_DESCENDER: usize = 44;
    pub const RADICAL_VERTICAL_GAP: usize = 45;
    pub const RADICAL_DISPLAY_STYLE_VERTICAL_GAP: usize = 46;
    pub const RADICAL_RULE_THICKNESS: usize = 47;
    pub const RADICAL_EXTRA_ASCENDER: usize = 48;
    pub const RADICAL_KERN_BEFORE_DEGREE: usize = 49;
    pub const RADICAL_KERN_AFTER_DEGREE: usize = 50;
}

/// `MathConstants` — font-wide math-layout constants.
///
/// Parsed lazily: the struct just holds a slice of the subtable and
/// each accessor reads from the right offset on demand.
#[derive(Debug, Clone, Copy)]
pub struct MathConstants<'a> {
    data: &'a [u8],
}

impl<'a> MathConstants<'a> {
    /// Parses a MathConstants subtable. Validates the byte length so
    /// every accessor is then in-bounds.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        if data.len() < MATH_CONSTANTS_LEN {
            return Err(Error::Truncated {
                offset: data.len(),
                context: "MathConstants subtable truncated",
            });
        }
        Ok(Self { data })
    }

    fn read_i16_at(&self, off: usize) -> i16 {
        i16::from_be_bytes([self.data[off], self.data[off + 1]])
    }

    fn read_u16_at(&self, off: usize) -> u16 {
        u16::from_be_bytes([self.data[off], self.data[off + 1]])
    }

    fn value_record(&self, idx: usize) -> MathValue<'a> {
        let off = FIRST_VALUE_RECORD + idx * 4;
        // Pre-validated length, so direct slice is safe.
        let value = i16::from_be_bytes([self.data[off], self.data[off + 1]]);
        let device_off = u16::from_be_bytes([self.data[off + 2], self.data[off + 3]]);
        // Device offsets are from the start of the enclosing
        // MathConstants subtable.
        let device = DeviceOrVariationIndex::parse_from(self.data, device_off)
            .ok()
            .flatten();
        MathValue {
            value,
            device,
            _table: self.data,
        }
    }

    /// Percentage scale factor for script-style math, ×100. e.g.
    /// `80` means script subscripts/superscripts are at 80% of the
    /// base size.
    #[must_use]
    pub fn script_percent_scale_down(&self) -> i16 {
        self.read_i16_at(SCRIPT_PERCENT_SCALE_DOWN)
    }

    /// Percentage scale factor for script-script (nested) math.
    #[must_use]
    pub fn script_script_percent_scale_down(&self) -> i16 {
        self.read_i16_at(SCRIPT_SCRIPT_PERCENT_SCALE_DOWN)
    }

    /// Minimum height (in design units) at which delimiters around a
    /// subformula start scaling. Stored as `UFWord` (u16).
    #[must_use]
    pub fn delimited_sub_formula_min_height(&self) -> u16 {
        self.read_u16_at(DELIMITED_SUB_FORMULA_MIN_HEIGHT)
    }

    /// Minimum height for a display-style large operator.
    #[must_use]
    pub fn display_operator_min_height(&self) -> u16 {
        self.read_u16_at(DISPLAY_OPERATOR_MIN_HEIGHT)
    }

    /// Trailing constant: percentage of the radical-degree height to
    /// shift the degree above the radical baseline. Stored as a bare
    /// `i16` after the value-record block.
    #[must_use]
    pub fn radical_degree_bottom_raise_percent(&self) -> i16 {
        self.read_i16_at(FIRST_VALUE_RECORD + NUM_VALUE_RECORDS * 4)
    }

    // -- the named MathValueRecord accessors -----------------------------

    /// `mathLeading` — minimum gap between math content baselines.
    #[must_use]
    pub fn math_leading(&self) -> MathValue<'a> {
        self.value_record(c_idx::MATH_LEADING)
    }
    /// `axisHeight` — height of the math axis above the baseline.
    #[must_use]
    pub fn axis_height(&self) -> MathValue<'a> {
        self.value_record(c_idx::AXIS_HEIGHT)
    }
    /// `accentBaseHeight`.
    #[must_use]
    pub fn accent_base_height(&self) -> MathValue<'a> {
        self.value_record(c_idx::ACCENT_BASE_HEIGHT)
    }
    /// `flattenedAccentBaseHeight`.
    #[must_use]
    pub fn flattened_accent_base_height(&self) -> MathValue<'a> {
        self.value_record(c_idx::FLATTENED_ACCENT_BASE_HEIGHT)
    }
    /// `subscriptShiftDown`.
    #[must_use]
    pub fn subscript_shift_down(&self) -> MathValue<'a> {
        self.value_record(c_idx::SUBSCRIPT_SHIFT_DOWN)
    }
    /// `subscriptTopMax`.
    #[must_use]
    pub fn subscript_top_max(&self) -> MathValue<'a> {
        self.value_record(c_idx::SUBSCRIPT_TOP_MAX)
    }
    /// `subscriptBaselineDropMin`.
    #[must_use]
    pub fn subscript_baseline_drop_min(&self) -> MathValue<'a> {
        self.value_record(c_idx::SUBSCRIPT_BASELINE_DROP_MIN)
    }
    /// `superscriptShiftUp`.
    #[must_use]
    pub fn superscript_shift_up(&self) -> MathValue<'a> {
        self.value_record(c_idx::SUPERSCRIPT_SHIFT_UP)
    }
    /// `superscriptShiftUpCramped`.
    #[must_use]
    pub fn superscript_shift_up_cramped(&self) -> MathValue<'a> {
        self.value_record(c_idx::SUPERSCRIPT_SHIFT_UP_CRAMPED)
    }
    /// `superscriptBottomMin`.
    #[must_use]
    pub fn superscript_bottom_min(&self) -> MathValue<'a> {
        self.value_record(c_idx::SUPERSCRIPT_BOTTOM_MIN)
    }
    /// `superscriptBaselineDropMax`.
    #[must_use]
    pub fn superscript_baseline_drop_max(&self) -> MathValue<'a> {
        self.value_record(c_idx::SUPERSCRIPT_BASELINE_DROP_MAX)
    }
    /// `subSuperscriptGapMin`.
    #[must_use]
    pub fn sub_superscript_gap_min(&self) -> MathValue<'a> {
        self.value_record(c_idx::SUB_SUPERSCRIPT_GAP_MIN)
    }
    /// `superscriptBottomMaxWithSubscript`.
    #[must_use]
    pub fn superscript_bottom_max_with_subscript(&self) -> MathValue<'a> {
        self.value_record(c_idx::SUPERSCRIPT_BOTTOM_MAX_WITH_SUBSCRIPT)
    }
    /// `spaceAfterScript`.
    #[must_use]
    pub fn space_after_script(&self) -> MathValue<'a> {
        self.value_record(c_idx::SPACE_AFTER_SCRIPT)
    }
    /// `upperLimitGapMin`.
    #[must_use]
    pub fn upper_limit_gap_min(&self) -> MathValue<'a> {
        self.value_record(c_idx::UPPER_LIMIT_GAP_MIN)
    }
    /// `upperLimitBaselineRiseMin`.
    #[must_use]
    pub fn upper_limit_baseline_rise_min(&self) -> MathValue<'a> {
        self.value_record(c_idx::UPPER_LIMIT_BASELINE_RISE_MIN)
    }
    /// `lowerLimitGapMin`.
    #[must_use]
    pub fn lower_limit_gap_min(&self) -> MathValue<'a> {
        self.value_record(c_idx::LOWER_LIMIT_GAP_MIN)
    }
    /// `lowerLimitBaselineDropMin`.
    #[must_use]
    pub fn lower_limit_baseline_drop_min(&self) -> MathValue<'a> {
        self.value_record(c_idx::LOWER_LIMIT_BASELINE_DROP_MIN)
    }
    /// `stackTopShiftUp`.
    #[must_use]
    pub fn stack_top_shift_up(&self) -> MathValue<'a> {
        self.value_record(c_idx::STACK_TOP_SHIFT_UP)
    }
    /// `stackTopDisplayStyleShiftUp`.
    #[must_use]
    pub fn stack_top_display_style_shift_up(&self) -> MathValue<'a> {
        self.value_record(c_idx::STACK_TOP_DISPLAY_STYLE_SHIFT_UP)
    }
    /// `stackBottomShiftDown`.
    #[must_use]
    pub fn stack_bottom_shift_down(&self) -> MathValue<'a> {
        self.value_record(c_idx::STACK_BOTTOM_SHIFT_DOWN)
    }
    /// `stackBottomDisplayStyleShiftDown`.
    #[must_use]
    pub fn stack_bottom_display_style_shift_down(&self) -> MathValue<'a> {
        self.value_record(c_idx::STACK_BOTTOM_DISPLAY_STYLE_SHIFT_DOWN)
    }
    /// `stackGapMin`.
    #[must_use]
    pub fn stack_gap_min(&self) -> MathValue<'a> {
        self.value_record(c_idx::STACK_GAP_MIN)
    }
    /// `stackDisplayStyleGapMin`.
    #[must_use]
    pub fn stack_display_style_gap_min(&self) -> MathValue<'a> {
        self.value_record(c_idx::STACK_DISPLAY_STYLE_GAP_MIN)
    }
    /// `stretchStackTopShiftUp`.
    #[must_use]
    pub fn stretch_stack_top_shift_up(&self) -> MathValue<'a> {
        self.value_record(c_idx::STRETCH_STACK_TOP_SHIFT_UP)
    }
    /// `stretchStackBottomShiftDown`.
    #[must_use]
    pub fn stretch_stack_bottom_shift_down(&self) -> MathValue<'a> {
        self.value_record(c_idx::STRETCH_STACK_BOTTOM_SHIFT_DOWN)
    }
    /// `stretchStackGapAboveMin`.
    #[must_use]
    pub fn stretch_stack_gap_above_min(&self) -> MathValue<'a> {
        self.value_record(c_idx::STRETCH_STACK_GAP_ABOVE_MIN)
    }
    /// `stretchStackGapBelowMin`.
    #[must_use]
    pub fn stretch_stack_gap_below_min(&self) -> MathValue<'a> {
        self.value_record(c_idx::STRETCH_STACK_GAP_BELOW_MIN)
    }
    /// `fractionNumeratorShiftUp`.
    #[must_use]
    pub fn fraction_numerator_shift_up(&self) -> MathValue<'a> {
        self.value_record(c_idx::FRACTION_NUMERATOR_SHIFT_UP)
    }
    /// `fractionNumeratorDisplayStyleShiftUp`.
    #[must_use]
    pub fn fraction_numerator_display_style_shift_up(&self) -> MathValue<'a> {
        self.value_record(c_idx::FRACTION_NUMERATOR_DISPLAY_STYLE_SHIFT_UP)
    }
    /// `fractionDenominatorShiftDown`.
    #[must_use]
    pub fn fraction_denominator_shift_down(&self) -> MathValue<'a> {
        self.value_record(c_idx::FRACTION_DENOMINATOR_SHIFT_DOWN)
    }
    /// `fractionDenominatorDisplayStyleShiftDown`.
    #[must_use]
    pub fn fraction_denominator_display_style_shift_down(&self) -> MathValue<'a> {
        self.value_record(c_idx::FRACTION_DENOMINATOR_DISPLAY_STYLE_SHIFT_DOWN)
    }
    /// `fractionNumeratorGapMin`.
    #[must_use]
    pub fn fraction_numerator_gap_min(&self) -> MathValue<'a> {
        self.value_record(c_idx::FRACTION_NUMERATOR_GAP_MIN)
    }
    /// `fractionNumDisplayStyleGapMin`.
    #[must_use]
    pub fn fraction_num_display_style_gap_min(&self) -> MathValue<'a> {
        self.value_record(c_idx::FRACTION_NUM_DISPLAY_STYLE_GAP_MIN)
    }
    /// `fractionRuleThickness`.
    #[must_use]
    pub fn fraction_rule_thickness(&self) -> MathValue<'a> {
        self.value_record(c_idx::FRACTION_RULE_THICKNESS)
    }
    /// `fractionDenominatorGapMin`.
    #[must_use]
    pub fn fraction_denominator_gap_min(&self) -> MathValue<'a> {
        self.value_record(c_idx::FRACTION_DENOMINATOR_GAP_MIN)
    }
    /// `fractionDenomDisplayStyleGapMin`.
    #[must_use]
    pub fn fraction_denom_display_style_gap_min(&self) -> MathValue<'a> {
        self.value_record(c_idx::FRACTION_DENOM_DISPLAY_STYLE_GAP_MIN)
    }
    /// `skewedFractionHorizontalGap`.
    #[must_use]
    pub fn skewed_fraction_horizontal_gap(&self) -> MathValue<'a> {
        self.value_record(c_idx::SKEWED_FRACTION_HORIZONTAL_GAP)
    }
    /// `skewedFractionVerticalGap`.
    #[must_use]
    pub fn skewed_fraction_vertical_gap(&self) -> MathValue<'a> {
        self.value_record(c_idx::SKEWED_FRACTION_VERTICAL_GAP)
    }
    /// `overbarVerticalGap`.
    #[must_use]
    pub fn overbar_vertical_gap(&self) -> MathValue<'a> {
        self.value_record(c_idx::OVERBAR_VERTICAL_GAP)
    }
    /// `overbarRuleThickness`.
    #[must_use]
    pub fn overbar_rule_thickness(&self) -> MathValue<'a> {
        self.value_record(c_idx::OVERBAR_RULE_THICKNESS)
    }
    /// `overbarExtraAscender`.
    #[must_use]
    pub fn overbar_extra_ascender(&self) -> MathValue<'a> {
        self.value_record(c_idx::OVERBAR_EXTRA_ASCENDER)
    }
    /// `underbarVerticalGap`.
    #[must_use]
    pub fn underbar_vertical_gap(&self) -> MathValue<'a> {
        self.value_record(c_idx::UNDERBAR_VERTICAL_GAP)
    }
    /// `underbarRuleThickness`.
    #[must_use]
    pub fn underbar_rule_thickness(&self) -> MathValue<'a> {
        self.value_record(c_idx::UNDERBAR_RULE_THICKNESS)
    }
    /// `underbarExtraDescender`.
    #[must_use]
    pub fn underbar_extra_descender(&self) -> MathValue<'a> {
        self.value_record(c_idx::UNDERBAR_EXTRA_DESCENDER)
    }
    /// `radicalVerticalGap`.
    #[must_use]
    pub fn radical_vertical_gap(&self) -> MathValue<'a> {
        self.value_record(c_idx::RADICAL_VERTICAL_GAP)
    }
    /// `radicalDisplayStyleVerticalGap`.
    #[must_use]
    pub fn radical_display_style_vertical_gap(&self) -> MathValue<'a> {
        self.value_record(c_idx::RADICAL_DISPLAY_STYLE_VERTICAL_GAP)
    }
    /// `radicalRuleThickness`.
    #[must_use]
    pub fn radical_rule_thickness(&self) -> MathValue<'a> {
        self.value_record(c_idx::RADICAL_RULE_THICKNESS)
    }
    /// `radicalExtraAscender`.
    #[must_use]
    pub fn radical_extra_ascender(&self) -> MathValue<'a> {
        self.value_record(c_idx::RADICAL_EXTRA_ASCENDER)
    }
    /// `radicalKernBeforeDegree`.
    #[must_use]
    pub fn radical_kern_before_degree(&self) -> MathValue<'a> {
        self.value_record(c_idx::RADICAL_KERN_BEFORE_DEGREE)
    }
    /// `radicalKernAfterDegree`.
    #[must_use]
    pub fn radical_kern_after_degree(&self) -> MathValue<'a> {
        self.value_record(c_idx::RADICAL_KERN_AFTER_DEGREE)
    }
}

// =========================================================================
// MathGlyphInfo
// =========================================================================

/// Which corner a math kerning lookup applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernSide {
    /// Top-right corner of the glyph.
    TopRight,
    /// Top-left corner.
    TopLeft,
    /// Bottom-right corner.
    BottomRight,
    /// Bottom-left corner.
    BottomLeft,
}

/// Per-glyph math information: italic correction, top-accent attachment,
/// extended-shape membership, and the four-corner math kern table.
#[derive(Debug, Clone, Copy)]
pub struct MathGlyphInfo<'a> {
    /// Slice of the MathGlyphInfo subtable.
    data: &'a [u8],
    italic_correction_off: u16,
    top_accent_off: u16,
    extended_shape_off: u16,
    kern_info_off: u16,
}

impl<'a> MathGlyphInfo<'a> {
    /// Parses the four-offset MathGlyphInfo header. Each offset is
    /// relative to the start of the MathGlyphInfo subtable (i.e. `data`).
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let italic_correction_off = r.read_u16()?;
        let top_accent_off = r.read_u16()?;
        let extended_shape_off = r.read_u16()?;
        let kern_info_off = r.read_u16()?;
        for (off, ctx) in [
            (
                italic_correction_off,
                "MathItalicsCorrectionInfo offset past end",
            ),
            (top_accent_off, "MathTopAccentAttachment offset past end"),
            (extended_shape_off, "ExtendedShapeCoverage offset past end"),
            (kern_info_off, "MathKernInfo offset past end"),
        ] {
            if off != 0 && off as usize > data.len() {
                return Err(Error::Malformed {
                    offset: off as usize,
                    context: ctx,
                });
            }
        }
        Ok(Self {
            data,
            italic_correction_off,
            top_accent_off,
            extended_shape_off,
            kern_info_off,
        })
    }

    /// Italic correction (the kern that compensates for an italic
    /// glyph's tilt) for `gid`, or `None` when the font has no entry.
    pub fn italic_correction(&self, gid: u16) -> Option<MathValue<'a>> {
        Self::lookup_value(self.data, self.italic_correction_off, gid)
    }

    /// Top-accent attachment x-offset for `gid` (used to position
    /// combining accents above the glyph), or `None` when absent.
    pub fn top_accent_attachment(&self, gid: u16) -> Option<MathValue<'a>> {
        Self::lookup_value(self.data, self.top_accent_off, gid)
    }

    /// True when `gid` is in the "extended shape" coverage set —
    /// glyphs that already span the math axis and don't need
    /// accent-style superscript shifting.
    #[must_use]
    pub fn is_extended_shape(&self, gid: u16) -> bool {
        if self.extended_shape_off == 0 {
            return false;
        }
        let Some(bytes) = self.data.get(self.extended_shape_off as usize..) else {
            return false;
        };
        let Ok(cov) = Coverage::parse(bytes) else {
            return false;
        };
        cov.contains(gid)
    }

    /// Returns the [`MathKernInfo`] for `gid` on `side`, or `None`
    /// when no entry covers that corner. The same coverage table
    /// drives all four sides; an absent entry on one side does not
    /// prevent the other three from working.
    pub fn kern_info(&self, gid: u16, side: KernSide) -> Option<MathKern<'a>> {
        if self.kern_info_off == 0 {
            return None;
        }
        let base = self.data.get(self.kern_info_off as usize..)?;
        // MathKernInfo header:
        //   Offset16 mathKernCoverageOffset
        //   uint16   mathKernCount
        //   MathKernInfoRecord[mathKernCount]    (8 bytes each)
        let mut r = Reader::new(base);
        let cov_off = r.read_u16().ok()?;
        let count = r.read_u16().ok()?;
        if cov_off == 0 {
            return None;
        }
        let cov_bytes = base.get(cov_off as usize..)?;
        let cov = Coverage::parse(cov_bytes).ok()?;
        let idx = cov.index_of(gid)? as usize;
        if idx >= count as usize {
            return None;
        }
        let record_off = 4 + idx * 8;
        let record = base.get(record_off..record_off + 8)?;
        let side_off = match side {
            KernSide::TopRight => 0,
            KernSide::TopLeft => 2,
            KernSide::BottomRight => 4,
            KernSide::BottomLeft => 6,
        };
        let kern_off = u16::from_be_bytes([record[side_off], record[side_off + 1]]);
        if kern_off == 0 {
            return None;
        }
        let kern_bytes = base.get(kern_off as usize..)?;
        MathKern::parse(kern_bytes).ok()
    }

    /// Looks up a per-glyph MathValueRecord through a
    /// `MathItalicsCorrectionInfo` / `MathTopAccentAttachment` table
    /// (both share the same `Coverage + array` shape).
    fn lookup_value(data: &'a [u8], info_off: u16, gid: u16) -> Option<MathValue<'a>> {
        if info_off == 0 {
            return None;
        }
        let base = data.get(info_off as usize..)?;
        // Layout:
        //   Offset16 coverageOffset
        //   uint16   count
        //   MathValueRecord[count]   (4 bytes each)
        let mut r = Reader::new(base);
        let cov_off = r.read_u16().ok()?;
        let count = r.read_u16().ok()?;
        if cov_off == 0 {
            return None;
        }
        let cov_bytes = base.get(cov_off as usize..)?;
        let cov = Coverage::parse(cov_bytes).ok()?;
        let idx = cov.index_of(gid)? as usize;
        if idx >= count as usize {
            return None;
        }
        let record_off = 4 + idx * 4;
        let bytes = base.get(record_off..record_off + 4)?;
        let value = i16::from_be_bytes([bytes[0], bytes[1]]);
        let device_off = u16::from_be_bytes([bytes[2], bytes[3]]);
        let device = DeviceOrVariationIndex::parse_from(base, device_off)
            .ok()
            .flatten();
        Some(MathValue {
            value,
            device,
            _table: base,
        })
    }
}

// =========================================================================
// MathKern
// =========================================================================

/// Per-corner math kerning table. Provides a piecewise step function
/// keyed on the *secondary* glyph's vertical position relative to the
/// kerning glyph's baseline.
///
/// The shape is two parallel arrays: `correction_height[i]` (n entries)
/// and `kern_value[i]` (n + 1 entries). For a query height *h*, walk
/// `correction_height` and pick the first `i` where *h ≤
/// correction_height[i]*; the returned kern is `kern_value[i]`. If *h*
/// exceeds every height, the answer is `kern_value[n]`.
#[derive(Debug, Clone, Copy)]
pub struct MathKern<'a> {
    /// Slice covering exactly the MathKern subtable.
    data: &'a [u8],
    height_count: u16,
}

impl<'a> MathKern<'a> {
    /// Parses a MathKern subtable header, validating array lengths.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let height_count = r.read_u16()?;
        let needed = 2 + (height_count as usize) * 4 + (height_count as usize + 1) * 4;
        if data.len() < needed {
            return Err(Error::Truncated {
                offset: data.len(),
                context: "MathKern arrays truncated",
            });
        }
        Ok(Self { data, height_count })
    }

    /// Number of correction-height steps. The kern value array has
    /// `height_count + 1` entries.
    #[must_use]
    pub const fn height_count(&self) -> u16 {
        self.height_count
    }

    /// Returns the i-th correction-height MathValueRecord, or `None`
    /// for an out-of-range index.
    pub fn correction_height(&self, i: u16) -> Option<MathValue<'a>> {
        if i >= self.height_count {
            return None;
        }
        let off = 2 + (i as usize) * 4;
        let bytes = self.data.get(off..off + 4)?;
        let value = i16::from_be_bytes([bytes[0], bytes[1]]);
        let device_off = u16::from_be_bytes([bytes[2], bytes[3]]);
        let device = DeviceOrVariationIndex::parse_from(self.data, device_off)
            .ok()
            .flatten();
        Some(MathValue {
            value,
            device,
            _table: self.data,
        })
    }

    /// Returns the i-th kern MathValueRecord. Valid `i` is
    /// `0..=height_count`.
    pub fn kern_value(&self, i: u16) -> Option<MathValue<'a>> {
        if i > self.height_count {
            return None;
        }
        let off = 2 + (self.height_count as usize) * 4 + (i as usize) * 4;
        let bytes = self.data.get(off..off + 4)?;
        let value = i16::from_be_bytes([bytes[0], bytes[1]]);
        let device_off = u16::from_be_bytes([bytes[2], bytes[3]]);
        let device = DeviceOrVariationIndex::parse_from(self.data, device_off)
            .ok()
            .flatten();
        Some(MathValue {
            value,
            device,
            _table: self.data,
        })
    }
}

// =========================================================================
// MathVariants
// =========================================================================

/// One entry in a stretchy operator's progressive-size variant list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MathGlyphVariant {
    /// Glyph id of the variant.
    pub variant_glyph: u16,
    /// Advance dimension (height for vertical, width for horizontal)
    /// in font design units.
    pub advance_measurement: u16,
}

/// Flag bit on a [`GlyphPart`] marking it as a repeatable extender
/// (the part that the assembler tiles to fill arbitrary lengths).
pub const PART_FLAG_EXTENDER: u16 = 0x0001;

/// One part of an extensible-glyph assembly.
///
/// The full glyph is stitched from these in order, with adjacent
/// parts overlapping by at least `start_connector_length` /
/// `end_connector_length` design units so the seam is invisible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlyphPart {
    /// Glyph id of the part.
    pub glyph_id: u16,
    /// Connector length on the leading edge (top for vertical, left
    /// for horizontal).
    pub start_connector_length: u16,
    /// Connector length on the trailing edge.
    pub end_connector_length: u16,
    /// Full advance of this part, in design units.
    pub full_advance: u16,
    /// Bit flags. `PART_FLAG_EXTENDER` (0x0001) marks the repeatable
    /// extender; other bits are reserved.
    pub part_flags: u16,
}

impl GlyphPart {
    /// True when this part is the repeatable extender of its assembly.
    #[must_use]
    pub const fn is_extender(&self) -> bool {
        self.part_flags & PART_FLAG_EXTENDER != 0
    }
}

/// `GlyphAssembly` — the parts list used to compose stretchy glyphs
/// taller / wider than every variant in [`GlyphConstruction::variants`].
#[derive(Debug, Clone, Copy)]
pub struct GlyphAssembly<'a> {
    /// The italics-correction MathValueRecord for the assembled glyph.
    pub italics_correction: MathValue<'a>,
    /// Slice of just the `GlyphPartRecord` array (10 bytes each).
    parts: &'a [u8],
    part_count: u16,
}

impl GlyphAssembly<'_> {
    /// Number of `GlyphPart`s in this assembly.
    #[must_use]
    pub const fn part_count(&self) -> u16 {
        self.part_count
    }

    /// Returns the i-th `GlyphPart`, or `None` for an out-of-range index.
    #[must_use]
    pub fn part(&self, i: u16) -> Option<GlyphPart> {
        if i >= self.part_count {
            return None;
        }
        let off = (i as usize) * 10;
        let b = self.parts.get(off..off + 10)?;
        Some(GlyphPart {
            glyph_id: u16::from_be_bytes([b[0], b[1]]),
            start_connector_length: u16::from_be_bytes([b[2], b[3]]),
            end_connector_length: u16::from_be_bytes([b[4], b[5]]),
            full_advance: u16::from_be_bytes([b[6], b[7]]),
            part_flags: u16::from_be_bytes([b[8], b[9]]),
        })
    }

    /// Iterator over every part in order.
    pub fn iter(&self) -> impl Iterator<Item = GlyphPart> + '_ {
        (0..self.part_count).filter_map(|i| self.part(i))
    }
}

/// `MathGlyphConstruction` — variants list plus optional assembly.
#[derive(Debug, Clone, Copy)]
pub struct GlyphConstruction<'a> {
    /// Slice of the MathGlyphConstruction subtable.
    data: &'a [u8],
    assembly_off: u16,
    variant_count: u16,
}

impl<'a> GlyphConstruction<'a> {
    /// Parses a `MathGlyphConstruction` subtable.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let assembly_off = r.read_u16()?;
        let variant_count = r.read_u16()?;
        // Variant array follows the header: 4 bytes per record.
        let needed = 4 + (variant_count as usize) * 4;
        if data.len() < needed {
            return Err(Error::Truncated {
                offset: data.len(),
                context: "MathGlyphConstruction variant array truncated",
            });
        }
        if assembly_off != 0 && assembly_off as usize > data.len() {
            return Err(Error::Malformed {
                offset: assembly_off as usize,
                context: "GlyphAssembly offset past end",
            });
        }
        Ok(Self {
            data,
            assembly_off,
            variant_count,
        })
    }

    /// Number of progressive-size variants.
    #[must_use]
    pub const fn variant_count(&self) -> u16 {
        self.variant_count
    }

    /// Returns the i-th progressive variant.
    #[must_use]
    pub fn variant(&self, i: u16) -> Option<MathGlyphVariant> {
        if i >= self.variant_count {
            return None;
        }
        let off = 4 + (i as usize) * 4;
        let b = self.data.get(off..off + 4)?;
        Some(MathGlyphVariant {
            variant_glyph: u16::from_be_bytes([b[0], b[1]]),
            advance_measurement: u16::from_be_bytes([b[2], b[3]]),
        })
    }

    /// Iterator over every variant in order.
    pub fn variants(&self) -> impl Iterator<Item = MathGlyphVariant> + '_ {
        (0..self.variant_count).filter_map(|i| self.variant(i))
    }

    /// Returns the assembly-parts list used for sizes beyond the
    /// largest variant, or `None` when the font does not provide one.
    pub fn assembly(&self) -> Option<GlyphAssembly<'a>> {
        if self.assembly_off == 0 {
            return None;
        }
        let base = self.data.get(self.assembly_off as usize..)?;
        // GlyphAssembly:
        //   MathValueRecord italicsCorrection (4 bytes)
        //   uint16 partCount
        //   GlyphPartRecord parts[partCount]   (10 bytes each)
        let mut r = Reader::new(base);
        let value = r.read_i16().ok()?;
        let device_off = r.read_u16().ok()?;
        let part_count = r.read_u16().ok()?;
        let needed = 6 + (part_count as usize) * 10;
        let parts_block = base.get(6..needed)?;
        let device = DeviceOrVariationIndex::parse_from(base, device_off)
            .ok()
            .flatten();
        Some(GlyphAssembly {
            italics_correction: MathValue {
                value,
                device,
                _table: base,
            },
            parts: parts_block,
            part_count,
        })
    }
}

/// `MathVariants` — stretchy-operator construction tables.
#[derive(Debug, Clone, Copy)]
pub struct MathVariants<'a> {
    /// Slice of the MathVariants subtable.
    data: &'a [u8],
    /// Minimum overlap between adjacent assembly parts, in design
    /// units. Re-exposed as [`Self::min_connector_overlap`].
    min_connector_overlap: u16,
    vert_coverage_off: u16,
    horiz_coverage_off: u16,
    vert_count: u16,
    horiz_count: u16,
    /// Offset of the start of the vertical-construction-offset array
    /// (right after the header).
    vert_off_table_start: usize,
    /// Offset of the start of the horizontal-construction-offset array.
    horiz_off_table_start: usize,
}

impl<'a> MathVariants<'a> {
    /// Parses a MathVariants header and validates all child offsets.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let min_connector_overlap = r.read_u16()?;
        let vert_coverage_off = r.read_u16()?;
        let horiz_coverage_off = r.read_u16()?;
        let vert_count = r.read_u16()?;
        let horiz_count = r.read_u16()?;
        let vert_off_table_start = r.position();
        let horiz_off_table_start = vert_off_table_start + (vert_count as usize) * 2;
        let needed = horiz_off_table_start + (horiz_count as usize) * 2;
        if data.len() < needed {
            return Err(Error::Truncated {
                offset: data.len(),
                context: "MathVariants construction-offset arrays truncated",
            });
        }
        for (off, ctx) in [
            (vert_coverage_off, "MathVariants vertical Coverage past end"),
            (
                horiz_coverage_off,
                "MathVariants horizontal Coverage past end",
            ),
        ] {
            if off != 0 && off as usize > data.len() {
                return Err(Error::Malformed {
                    offset: off as usize,
                    context: ctx,
                });
            }
        }
        Ok(Self {
            data,
            min_connector_overlap,
            vert_coverage_off,
            horiz_coverage_off,
            vert_count,
            horiz_count,
            vert_off_table_start,
            horiz_off_table_start,
        })
    }

    /// Minimum overlap between adjacent assembly parts.
    #[must_use]
    pub const fn min_connector_overlap(&self) -> u16 {
        self.min_connector_overlap
    }

    /// Looks up the vertical [`GlyphConstruction`] for a stretchy
    /// glyph (e.g. a tall integral or matrix bracket).
    pub fn vertical_glyph_construction(&self, gid: u16) -> Option<GlyphConstruction<'a>> {
        self.lookup(
            gid,
            self.vert_coverage_off,
            self.vert_off_table_start,
            self.vert_count,
        )
    }

    /// Looks up the horizontal [`GlyphConstruction`] for a stretchy
    /// glyph (e.g. an extensible underbrace).
    pub fn horizontal_glyph_construction(&self, gid: u16) -> Option<GlyphConstruction<'a>> {
        self.lookup(
            gid,
            self.horiz_coverage_off,
            self.horiz_off_table_start,
            self.horiz_count,
        )
    }

    fn lookup(
        &self,
        gid: u16,
        cov_off: u16,
        off_table_start: usize,
        count: u16,
    ) -> Option<GlyphConstruction<'a>> {
        if cov_off == 0 || count == 0 {
            return None;
        }
        let cov_bytes = self.data.get(cov_off as usize..)?;
        let cov = Coverage::parse(cov_bytes).ok()?;
        let idx = cov.index_of(gid)? as usize;
        if idx >= count as usize {
            return None;
        }
        let off_pos = off_table_start + idx * 2;
        let b = self.data.get(off_pos..off_pos + 2)?;
        let cons_off = u16::from_be_bytes([b[0], b[1]]);
        if cons_off == 0 {
            return None;
        }
        let cons_bytes = self.data.get(cons_off as usize..)?;
        GlyphConstruction::parse(cons_bytes).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::{vec, vec::Vec};

    // ---- helpers ---------------------------------------------------------

    fn push_u16(v: &mut Vec<u8>, x: u16) {
        v.extend_from_slice(&x.to_be_bytes());
    }
    fn push_i16(v: &mut Vec<u8>, x: i16) {
        v.extend_from_slice(&x.to_be_bytes());
    }

    /// Build a minimal Coverage Format 1 covering the given gids in order.
    fn coverage_fmt1(gids: &[u16]) -> Vec<u8> {
        let mut v = Vec::new();
        push_u16(&mut v, 1); // format
        push_u16(&mut v, gids.len() as u16);
        for g in gids {
            push_u16(&mut v, *g);
        }
        v
    }

    // ---- MATH header -----------------------------------------------------

    #[test]
    fn math_header_rejects_bad_version() {
        let mut v = Vec::new();
        push_u16(&mut v, 2); // major
        push_u16(&mut v, 0); // minor
        push_u16(&mut v, 0);
        push_u16(&mut v, 0);
        push_u16(&mut v, 0);
        assert!(matches!(Math::parse(&v), Err(Error::Malformed { .. })));
    }

    #[test]
    fn math_header_with_all_offsets_zero_returns_none_subtables() {
        let mut v = Vec::new();
        push_u16(&mut v, 1);
        push_u16(&mut v, 0);
        push_u16(&mut v, 0);
        push_u16(&mut v, 0);
        push_u16(&mut v, 0);
        let m = Math::parse(&v).unwrap();
        assert!(m.constants().unwrap().is_none());
        assert!(m.glyph_info().unwrap().is_none());
        assert!(m.variants().unwrap().is_none());
    }

    #[test]
    fn math_header_rejects_offset_past_end() {
        let mut v = Vec::new();
        push_u16(&mut v, 1);
        push_u16(&mut v, 0);
        push_u16(&mut v, 9999); // bogus constants offset
        push_u16(&mut v, 0);
        push_u16(&mut v, 0);
        assert!(matches!(Math::parse(&v), Err(Error::Malformed { .. })));
    }

    // ---- MathConstants ---------------------------------------------------

    fn build_constants() -> Vec<u8> {
        let mut v = Vec::new();
        push_i16(&mut v, 80); // scriptPercentScaleDown
        push_i16(&mut v, 60); // scriptScriptPercentScaleDown
        push_u16(&mut v, 1500); // delimitedSubFormulaMinHeight
        push_u16(&mut v, 1800); // displayOperatorMinHeight
                                // 51 == NUM_VALUE_RECORDS; spelled inline to keep the literal
                                // a plain i16 for clippy's cast-possible-wrap lint.
        for i in 0i16..51 {
            push_i16(&mut v, 100 + i); // value
            push_u16(&mut v, 0); // device offset = 0 (none)
        }
        push_i16(&mut v, 65); // radicalDegreeBottomRaisePercent
        v
    }

    #[test]
    fn math_constants_parses_scalar_header_fields() {
        let bytes = build_constants();
        let c = MathConstants::parse(&bytes).unwrap();
        assert_eq!(c.script_percent_scale_down(), 80);
        assert_eq!(c.script_script_percent_scale_down(), 60);
        assert_eq!(c.delimited_sub_formula_min_height(), 1500);
        assert_eq!(c.display_operator_min_height(), 1800);
        assert_eq!(c.radical_degree_bottom_raise_percent(), 65);
    }

    #[test]
    fn math_constants_returns_value_records_for_named_fields() {
        let bytes = build_constants();
        let c = MathConstants::parse(&bytes).unwrap();
        // axisHeight is index 1 → 100 + 1 = 101.
        assert_eq!(c.axis_height().value, 101);
        assert!(c.axis_height().device.is_none());
        // fractionRuleThickness is index 34 → 100 + 34 = 134.
        assert_eq!(c.fraction_rule_thickness().value, 134);
        // radicalKernAfterDegree is the last value record (idx 50).
        assert_eq!(c.radical_kern_after_degree().value, 150);
    }

    #[test]
    fn math_constants_rejects_truncated_input() {
        let bytes = vec![0u8; MATH_CONSTANTS_LEN - 1];
        assert!(matches!(
            MathConstants::parse(&bytes),
            Err(Error::Truncated { .. })
        ));
    }

    // ---- MathGlyphInfo ---------------------------------------------------

    /// Builds a `MathItalicsCorrectionInfo` table covering a single gid
    /// with the given correction value. Returned bytes: header at the
    /// start, coverage appended after the value-record array.
    fn italics_info_one(gid: u16, value: i16) -> Vec<u8> {
        let mut v = Vec::new();
        // coverageOffset = 8 (after the 4-byte header + 4-byte record).
        push_u16(&mut v, 8);
        push_u16(&mut v, 1); // count
        push_i16(&mut v, value);
        push_u16(&mut v, 0); // device = 0
        v.extend_from_slice(&coverage_fmt1(&[gid]));
        v
    }

    /// Build a minimal MathGlyphInfo with only italics correction populated.
    fn glyph_info_italics_only(gid: u16, value: i16) -> Vec<u8> {
        let mut v = Vec::new();
        // header is 8 bytes (4 × Offset16). italics offset = 8.
        push_u16(&mut v, 8);
        push_u16(&mut v, 0);
        push_u16(&mut v, 0);
        push_u16(&mut v, 0);
        v.extend_from_slice(&italics_info_one(gid, value));
        v
    }

    #[test]
    fn glyph_info_returns_italic_correction_for_covered_gid() {
        let bytes = glyph_info_italics_only(7, 42);
        let gi = MathGlyphInfo::parse(&bytes).unwrap();
        let mv = gi.italic_correction(7).unwrap();
        assert_eq!(mv.value, 42);
        assert!(gi.italic_correction(8).is_none());
    }

    #[test]
    fn glyph_info_extended_shape_coverage_check() {
        let mut v = Vec::new();
        push_u16(&mut v, 0); // italics off
        push_u16(&mut v, 0); // top accent off
        push_u16(&mut v, 8); // extended shape off (after header)
        push_u16(&mut v, 0); // kern info off
        v.extend_from_slice(&coverage_fmt1(&[3, 5, 7]));
        let gi = MathGlyphInfo::parse(&v).unwrap();
        assert!(gi.is_extended_shape(3));
        assert!(gi.is_extended_shape(5));
        assert!(gi.is_extended_shape(7));
        assert!(!gi.is_extended_shape(4));
        assert!(!gi.is_extended_shape(99));
    }

    #[test]
    fn glyph_info_absent_subtables_yield_none() {
        // All four offsets zero — nothing is present.
        let bytes = vec![0u8; 8];
        let gi = MathGlyphInfo::parse(&bytes).unwrap();
        assert!(gi.italic_correction(0).is_none());
        assert!(gi.top_accent_attachment(0).is_none());
        assert!(!gi.is_extended_shape(0));
        assert!(gi.kern_info(0, KernSide::TopRight).is_none());
    }

    // ---- MathKern --------------------------------------------------------

    fn build_math_kern(heights: &[i16], kerns: &[i16]) -> Vec<u8> {
        assert_eq!(kerns.len(), heights.len() + 1);
        let mut v = Vec::new();
        push_u16(&mut v, heights.len() as u16);
        for h in heights {
            push_i16(&mut v, *h);
            push_u16(&mut v, 0);
        }
        for k in kerns {
            push_i16(&mut v, *k);
            push_u16(&mut v, 0);
        }
        v
    }

    #[test]
    fn math_kern_exposes_height_and_kern_arrays() {
        let bytes = build_math_kern(&[100, 200, 300], &[5, 10, 15, 20]);
        let mk = MathKern::parse(&bytes).unwrap();
        assert_eq!(mk.height_count(), 3);
        assert_eq!(mk.correction_height(0).unwrap().value, 100);
        assert_eq!(mk.correction_height(2).unwrap().value, 300);
        assert!(mk.correction_height(3).is_none());
        assert_eq!(mk.kern_value(0).unwrap().value, 5);
        assert_eq!(mk.kern_value(3).unwrap().value, 20);
        assert!(mk.kern_value(4).is_none());
    }

    #[test]
    fn math_kern_truncated_arrays_error() {
        let mut v = Vec::new();
        push_u16(&mut v, 5);
        v.extend_from_slice(&[0u8; 4]); // far short of needed
        assert!(matches!(MathKern::parse(&v), Err(Error::Truncated { .. })));
    }

    #[test]
    fn glyph_info_kern_info_lookup_finds_top_right_kern() {
        // Build a MathKernInfo table covering gid 9 with a non-null
        // top-right kern. Layout:
        //   [0..2]   coverageOffset
        //   [2..4]   mathKernCount = 1
        //   [4..12]  MathKernInfoRecord (4 × Offset16 — TR/TL/BR/BL)
        //   [12..]   coverage
        //   [..]     MathKern table (from build_math_kern)
        let kern_table = build_math_kern(&[50], &[7, 9]);
        let coverage = coverage_fmt1(&[9]);
        // Record top-right offset = 12 + coverage.len() (start of kern).
        let top_right_off = 12 + coverage.len() as u16;
        let mut info = Vec::new();
        push_u16(&mut info, 12); // coverage offset
        push_u16(&mut info, 1); // mathKernCount
        push_u16(&mut info, top_right_off); // top-right
        push_u16(&mut info, 0); // top-left
        push_u16(&mut info, 0); // bottom-right
        push_u16(&mut info, 0); // bottom-left
        info.extend_from_slice(&coverage);
        info.extend_from_slice(&kern_table);

        // Wrap in a MathGlyphInfo whose kern-info offset is 8.
        let mut gi_bytes = Vec::new();
        push_u16(&mut gi_bytes, 0);
        push_u16(&mut gi_bytes, 0);
        push_u16(&mut gi_bytes, 0);
        push_u16(&mut gi_bytes, 8); // kern info offset
        gi_bytes.extend_from_slice(&info);

        let gi = MathGlyphInfo::parse(&gi_bytes).unwrap();
        let mk = gi.kern_info(9, KernSide::TopRight).expect("present");
        assert_eq!(mk.height_count(), 1);
        assert_eq!(mk.correction_height(0).unwrap().value, 50);
        assert_eq!(mk.kern_value(1).unwrap().value, 9);
        assert!(gi.kern_info(9, KernSide::TopLeft).is_none());
        assert!(gi.kern_info(99, KernSide::TopRight).is_none());
    }

    // ---- MathVariants ----------------------------------------------------

    /// Builds a `MathGlyphConstruction` with two variants and no
    /// assembly. Returns just the construction-table bytes.
    fn construction_two_variants(v1: u16, a1: u16, v2: u16, a2: u16) -> Vec<u8> {
        let mut v = Vec::new();
        push_u16(&mut v, 0); // assembly offset = 0 (no assembly)
        push_u16(&mut v, 2); // variant count
        push_u16(&mut v, v1);
        push_u16(&mut v, a1);
        push_u16(&mut v, v2);
        push_u16(&mut v, a2);
        v
    }

    /// Builds a `MathGlyphConstruction` with one variant + one
    /// assembly part (extender) so we can exercise `assembly()`.
    fn construction_with_assembly() -> Vec<u8> {
        let mut v = Vec::new();
        // Construction header: assembly offset will be written after
        // we know the variant array size. For one variant, header (4)
        // + 4 = 8.
        push_u16(&mut v, 8); // assembly offset
        push_u16(&mut v, 1); // variant count
        push_u16(&mut v, 11); // variant glyph
        push_u16(&mut v, 1000); // advance
                                // GlyphAssembly: italicsCorrection (4) + partCount (2) + parts.
        push_i16(&mut v, 25); // italics correction value
        push_u16(&mut v, 0); // device = 0
        push_u16(&mut v, 1); // partCount
        push_u16(&mut v, 22); // glyphID
        push_u16(&mut v, 100); // startConnector
        push_u16(&mut v, 100); // endConnector
        push_u16(&mut v, 500); // fullAdvance
        push_u16(&mut v, PART_FLAG_EXTENDER);
        v
    }

    /// Builds a `MathVariants` table that maps gid 7 to a vertical
    /// construction with two variants and gid 8 to a horizontal
    /// construction with assembly. Layout is hand-laid so the
    /// integration of Coverage + offset arrays can be checked.
    fn build_math_variants() -> Vec<u8> {
        // Header is 10 bytes; after that, two construction-offset
        // arrays (vert then horiz, 1 entry each) → 4 more bytes.
        // After that we lay out the rest in order:
        //   verticalCoverage, verticalConstruction,
        //   horizontalCoverage, horizontalConstruction.
        let header_len = 10;
        let vert_offsets = 2;
        let horiz_offsets = 2;
        let mut v = Vec::with_capacity(64);
        let mut tail = Vec::new();

        let vert_cov_off = (header_len + vert_offsets + horiz_offsets + tail.len()) as u16;
        let vert_cov = coverage_fmt1(&[7]);
        tail.extend_from_slice(&vert_cov);

        let vert_cons_off = (header_len + vert_offsets + horiz_offsets + tail.len()) as u16;
        let vert_cons = construction_two_variants(101, 1000, 102, 2000);
        tail.extend_from_slice(&vert_cons);

        let horiz_cov_off = (header_len + vert_offsets + horiz_offsets + tail.len()) as u16;
        let horiz_cov = coverage_fmt1(&[8]);
        tail.extend_from_slice(&horiz_cov);

        let horiz_cons_off = (header_len + vert_offsets + horiz_offsets + tail.len()) as u16;
        let horiz_cons = construction_with_assembly();
        tail.extend_from_slice(&horiz_cons);

        // header
        push_u16(&mut v, 32); // minConnectorOverlap
        push_u16(&mut v, vert_cov_off);
        push_u16(&mut v, horiz_cov_off);
        push_u16(&mut v, 1); // vertGlyphCount
        push_u16(&mut v, 1); // horizGlyphCount
        push_u16(&mut v, vert_cons_off);
        push_u16(&mut v, horiz_cons_off);
        v.extend_from_slice(&tail);
        v
    }

    #[test]
    fn math_variants_returns_vertical_construction_for_covered_gid() {
        let bytes = build_math_variants();
        let mv = MathVariants::parse(&bytes).unwrap();
        assert_eq!(mv.min_connector_overlap(), 32);
        let cons = mv.vertical_glyph_construction(7).expect("covered");
        assert_eq!(cons.variant_count(), 2);
        let v0 = cons.variant(0).unwrap();
        assert_eq!(v0.variant_glyph, 101);
        assert_eq!(v0.advance_measurement, 1000);
        let v1 = cons.variant(1).unwrap();
        assert_eq!(v1.variant_glyph, 102);
        assert_eq!(v1.advance_measurement, 2000);
        assert!(cons.assembly().is_none());
        assert!(mv.vertical_glyph_construction(99).is_none());
    }

    #[test]
    fn math_variants_assembly_exposes_parts_and_extender_flag() {
        let bytes = build_math_variants();
        let mv = MathVariants::parse(&bytes).unwrap();
        let cons = mv.horizontal_glyph_construction(8).expect("horiz covered");
        assert_eq!(cons.variant_count(), 1);
        let asm = cons.assembly().expect("has assembly");
        assert_eq!(asm.italics_correction.value, 25);
        assert_eq!(asm.part_count(), 1);
        let p = asm.part(0).unwrap();
        assert_eq!(p.glyph_id, 22);
        assert_eq!(p.full_advance, 500);
        assert_eq!(p.start_connector_length, 100);
        assert_eq!(p.end_connector_length, 100);
        assert!(p.is_extender());
        assert_eq!(asm.iter().count(), 1);
    }

    #[test]
    fn math_variants_horizontal_lookup_misses_for_vertical_gid() {
        let bytes = build_math_variants();
        let mv = MathVariants::parse(&bytes).unwrap();
        // gid 7 is in the vertical coverage, not horizontal.
        assert!(mv.horizontal_glyph_construction(7).is_none());
    }

    #[test]
    fn math_variants_truncated_arrays_error() {
        // Header claims 5 vertical entries but the bytes stop at the header.
        let mut v = Vec::new();
        push_u16(&mut v, 0);
        push_u16(&mut v, 0);
        push_u16(&mut v, 0);
        push_u16(&mut v, 5);
        push_u16(&mut v, 0);
        assert!(matches!(
            MathVariants::parse(&v),
            Err(Error::Truncated { .. })
        ));
    }

    // ---- end-to-end via Math header --------------------------------------

    #[test]
    fn math_table_routes_offsets_to_each_subtable() {
        // Build a MATH table whose header points at a constants block,
        // glyph-info block, and variants block laid out back to back.
        let constants = build_constants();
        let gi = glyph_info_italics_only(3, 90);
        let mv = build_math_variants();

        let mut bytes = Vec::new();
        // Header is 10 bytes.
        push_u16(&mut bytes, 1); // major
        push_u16(&mut bytes, 0); // minor
        let const_off = 10u16;
        let gi_off = const_off + constants.len() as u16;
        let mv_off = gi_off + gi.len() as u16;
        push_u16(&mut bytes, const_off);
        push_u16(&mut bytes, gi_off);
        push_u16(&mut bytes, mv_off);
        bytes.extend_from_slice(&constants);
        bytes.extend_from_slice(&gi);
        bytes.extend_from_slice(&mv);

        let math = Math::parse(&bytes).unwrap();
        let c = math.constants().unwrap().expect("constants present");
        assert_eq!(c.script_percent_scale_down(), 80);
        let info = math.glyph_info().unwrap().expect("glyph info present");
        assert_eq!(info.italic_correction(3).unwrap().value, 90);
        let variants = math.variants().unwrap().expect("variants present");
        assert_eq!(variants.min_connector_overlap(), 32);
    }
}
