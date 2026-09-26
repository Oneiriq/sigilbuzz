//! `MathConstants`: the font-wide math layout constants.

use super::MathValue;
use crate::error::{Error, Result};
use crate::tables::layout::DeviceOrVariationIndex;

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
pub(super) const MATH_CONSTANTS_LEN: usize =
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

/// `MathConstants`: font-wide math-layout constants.
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

    /// Percentage scale factor for script-style math, x100. e.g.
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

    /// `mathLeading`: minimum gap between math content baselines.
    #[must_use]
    pub fn math_leading(&self) -> MathValue<'a> {
        self.value_record(c_idx::MATH_LEADING)
    }
    /// `axisHeight`: height of the math axis above the baseline.
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
