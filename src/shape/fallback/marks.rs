//! Fallback mark positioning (`_hb_ot_shape_fallback_mark_position`
//! and `recategorize_combining_class` in `hb-ot-shape-fallback.cc`,
//! HarfBuzz 14.5.0).
//!
//! When no GPOS, `kerx`, or cross-stream `kern` table positions a run
//! and its shaper asks for it (the default, Arabic, Hebrew, and Hangul
//! shapers), every mark is placed from its combining class and the ink
//! extents of its base: centered, left, or right aligned, stacked above
//! or below with a gap of a sixteenth of an em. Hebrew, Arabic, Thai,
//! Lao, and Tibetan classes are first folded into the positional
//! classes they stand for.

use super::extents::{glyph_extents, Extents};
use crate::buffer::{char_class, Direction, Glyph};
use crate::error::Result;
use crate::face::Face;
use crate::shape::lig;
use crate::tables::gdef::Gdef;
use crate::tables::layout::skip_iter::GlyphClasses;

// Canonical combining classes with a position (UAX #44).
const ATTACHED_BELOW_LEFT: u8 = 200;
const ATTACHED_BELOW: u8 = 202;
const ATTACHED_ABOVE: u8 = 214;
const ATTACHED_ABOVE_RIGHT: u8 = 216;
const BELOW_LEFT: u8 = 218;
const BELOW: u8 = 220;
const BELOW_RIGHT: u8 = 222;
const ABOVE_LEFT: u8 = 228;
const ABOVE: u8 = 230;
const ABOVE_RIGHT: u8 = 232;
const DOUBLE_BELOW: u8 = 233;
const DOUBLE_ABOVE: u8 = 234;

/// `recategorize_combining_class`: the positional class a nonspacing
/// mark's modified combining class `class` stands for. Thai and Lao
/// marks of class zero get one too.
pub(in crate::shape) fn recategorize_combining_class(u: char, class: u8) -> u8 {
    if class >= 200 {
        return class;
    }
    let mut class = class;
    // Thai and Lao need some per-character work.
    if u32::from(u) & !0xFF == 0x0E00 {
        if class == 0 {
            match u {
                '\u{0E31}' | '\u{0E34}'..='\u{0E37}' | '\u{0E47}' | '\u{0E4C}'..='\u{0E4E}' => {
                    class = ABOVE_RIGHT;
                }
                '\u{0EB1}' | '\u{0EB4}'..='\u{0EB7}' | '\u{0EBB}' | '\u{0ECC}' | '\u{0ECD}' => {
                    class = ABOVE;
                }
                '\u{0EBC}' => class = BELOW,
                _ => {}
            }
        } else if u == '\u{0E3A}' {
            // Thai phinthu is below-right.
            class = BELOW_RIGHT;
        }
    }
    // The cases are modified combining classes (see
    // `unicode::normalize::modified_combining_class`).
    match class {
        // Hebrew: sheva, hataf segol, hataf patah, hataf qamats, hiriq,
        // tsere, segol, patah, qamats, qubuts, meteg.
        15..=25 => BELOW,
        13 => ATTACHED_ABOVE,  // rafe
        10 => ABOVE_RIGHT,     // shin dot
        11 | 14 => ABOVE_LEFT, // sin dot, holam
        26 => ABOVE,           // point varika
        // Arabic and Syriac: fathatan, dammatan, fatha, damma, shadda,
        // sukun, superscript alef, superscript alaph.
        28 | 29 | 31 | 32 | 27 | 34 | 35 | 36 => ABOVE,
        30 | 33 => BELOW, // kasratan, kasra
        // Thai: sara u and uu, mai.
        3 => BELOW_RIGHT,
        107 => ABOVE_RIGHT,
        // Lao: sign u and uu, mai.
        118 => BELOW,
        122 => ABOVE,
        // Tibetan: sign aa, sign i, sign u.
        129 => BELOW,
        132 => ABOVE,
        131 => BELOW,
        // Dagesh (12) and everything else keep their class.
        other => other,
    }
}

/// What fallback mark positioning reads besides the glyphs.
pub(in crate::shape) struct MarkPositioner<'a> {
    pub(in crate::shape) face: &'a Face<'a>,
    pub(in crate::shape) coords: &'a [f32],
    pub(in crate::shape) gdef: Option<&'a Gdef<'a>>,
    /// The direction the run is shaped in.
    pub(in crate::shape) direction: Direction,
    /// The horizontal direction ligature components run in: the run's
    /// own, or its script's for a vertical run.
    pub(in crate::shape) ligature_direction: Direction,
    /// Zeroed marks move back by the advance they lose (see
    /// `position::position`).
    pub(in crate::shape) adjust_offsets: bool,
    /// The run's cluster level, for the glyph flags.
    pub(in crate::shape) level: crate::buffer::ClusterLevel,
}

/// True for glyphs mark positioning treats as marks.
fn is_mark(g: &Glyph) -> bool {
    g.char_class & char_class::MARK != 0
}

/// True for default-ignorable glyphs GSUB left alone, which neither end
/// a mark cluster nor count as its marks.
fn is_ignorable(g: &Glyph) -> bool {
    g.unicode_props & crate::buffer::unicode_prop::DEFAULT_IGNORABLE != 0
}

impl MarkPositioner<'_> {
    /// `_hb_ot_shape_fallback_mark_position`: positions every mark over
    /// or under the base it follows. `glyphs` is in logical order, with
    /// every other positioning done.
    pub(in crate::shape) fn position_marks(&self, glyphs: &mut [Glyph]) -> Result<()> {
        let mut start = 0;
        for i in 1..glyphs.len() {
            if !is_mark(&glyphs[i]) && !is_ignorable(&glyphs[i]) {
                self.position_cluster(glyphs, start, i)?;
                start = i;
            }
        }
        self.position_cluster(glyphs, start, glyphs.len())
    }

    fn position_cluster(&self, glyphs: &mut [Glyph], start: usize, end: usize) -> Result<()> {
        if end < start + 2 {
            return Ok(());
        }
        let mut i = start;
        while i < end {
            if !is_mark(&glyphs[i]) {
                let mut j = i + 1;
                while j < end && (is_ignorable(&glyphs[j]) || is_mark(&glyphs[j])) {
                    j += 1;
                }
                self.position_around_base(glyphs, i, j)?;
                i = j - 1;
            }
            i += 1;
        }
        Ok(())
    }

    fn h_advance(&self, gid: u16) -> Result<i32> {
        super::advance(self.face, self.coords, gid, true)
    }

    /// `position_around_base`: positions the marks in
    /// `glyphs[base + 1..end]` around `glyphs[base]`.
    fn position_around_base(&self, glyphs: &mut [Glyph], base: usize, end: usize) -> Result<()> {
        // The marks' places depend on their base.
        crate::shape::glyph_flags::unsafe_to_break(glyphs, base, end, self.level);
        let base_glyph = glyphs[base];
        let Some(mut base_extents) =
            glyph_extents(self.face, self.coords, base_glyph.glyph_id as u16)?
        else {
            // Without extents, zero the marks and go home.
            self.zero_mark_advances(&mut glyphs[base + 1..end]);
            return Ok(());
        };
        base_extents.y_bearing = base_extents.y_bearing.saturating_add(base_glyph.y_offset);
        // The horizontal advance works better than the ink here, and
        // also for glyphs without ink.
        base_extents.x_bearing = 0;
        base_extents.width = self.h_advance(base_glyph.glyph_id as u16)?;

        let lig_id = lig::lig_id(&base_glyph);
        let num_lig_components =
            i32::from(lig::num_comps(&base_glyph, &GlyphClasses::new(self.gdef)));

        let forward = self.direction.is_forward();
        let (mut x_offset, mut y_offset) = if forward {
            (
                base_glyph.x_advance.saturating_neg(),
                base_glyph.y_advance.saturating_neg(),
            )
        } else {
            (0, 0)
        };

        let mut component_extents = base_extents;
        let mut last_lig_component = -1;
        let mut last_class = u16::from(u8::MAX);
        let mut cluster_extents = base_extents;
        for g in &mut glyphs[base + 1..end] {
            let class = g.combining_class;
            if class == 0 {
                if forward {
                    x_offset = x_offset.saturating_sub(g.x_advance);
                    y_offset = y_offset.saturating_sub(g.y_advance);
                } else {
                    x_offset = x_offset.saturating_add(g.x_advance);
                    y_offset = y_offset.saturating_add(g.y_advance);
                }
                continue;
            }
            if num_lig_components > 1 {
                let this_lig_id = lig::lig_id(g);
                let mut this_component = i32::from(lig::lig_comp(g)) - 1;
                // Attach to the last component unless the mark belongs
                // to this ligature.
                if lig_id == 0 || lig_id != this_lig_id || this_component >= num_lig_components {
                    this_component = num_lig_components - 1;
                }
                if last_lig_component != this_component {
                    last_lig_component = this_component;
                    last_class = u16::from(u8::MAX);
                    component_extents = base_extents;
                    let slot = if self.ligature_direction == Direction::Ltr {
                        this_component
                    } else {
                        num_lig_components - 1 - this_component
                    };
                    let shift = i64::from(slot) * i64::from(component_extents.width)
                        / i64::from(num_lig_components);
                    component_extents.x_bearing =
                        clamp(i64::from(component_extents.x_bearing) + shift);
                    component_extents.width /= num_lig_components;
                }
            }
            if last_class != u16::from(class) {
                last_class = u16::from(class);
                cluster_extents = component_extents;
            }
            self.position_mark(&mut cluster_extents, g, class)?;
            g.x_advance = 0;
            g.y_advance = 0;
            g.x_offset = g.x_offset.saturating_add(x_offset);
            g.y_offset = g.y_offset.saturating_add(y_offset);
        }
        Ok(())
    }

    /// `position_mark`: places mark `g` of positional class `class`
    /// against `base` (the extents of what it stacks on), and grows
    /// `base` by the mark.
    fn position_mark(&self, base: &mut Extents, g: &mut Glyph, class: u8) -> Result<()> {
        let Some(mark) = glyph_extents(self.face, self.coords, g.glyph_id as u16)? else {
            return Ok(());
        };
        let upem = i32::from(self.face.head()?.units_per_em);
        let y_gap = upem / 16;
        let (bx, bw) = (i64::from(base.x_bearing), i64::from(base.width));
        let (mx, mw) = (i64::from(mark.x_bearing), i64::from(mark.width));
        let center = || clamp(bx + (bw - mw) / 2 - mx);
        g.x_offset = match class {
            DOUBLE_BELOW | DOUBLE_ABOVE if self.direction == Direction::Ltr => {
                clamp(bx + bw - mw / 2 - mx)
            }
            DOUBLE_BELOW | DOUBLE_ABOVE if self.direction == Direction::Rtl => {
                clamp(bx - mw / 2 - mx)
            }
            ATTACHED_BELOW_LEFT | BELOW_LEFT | ABOVE_LEFT => {
                base.x_bearing.saturating_sub(mark.x_bearing)
            }
            ATTACHED_ABOVE_RIGHT | BELOW_RIGHT | ABOVE_RIGHT => clamp(bx + bw - mw - mx),
            _ => center(),
        };
        g.y_offset = 0;
        match class {
            DOUBLE_BELOW | BELOW_LEFT | BELOW | BELOW_RIGHT | ATTACHED_BELOW_LEFT
            | ATTACHED_BELOW => {
                if !matches!(class, ATTACHED_BELOW_LEFT | ATTACHED_BELOW) {
                    base.height = base.height.saturating_sub(y_gap);
                }
                g.y_offset = clamp(
                    i64::from(base.y_bearing) + i64::from(base.height) - i64::from(mark.y_bearing),
                );
                // Never shift a below mark up.
                if (y_gap > 0) == (g.y_offset > 0) {
                    base.height = base.height.saturating_sub(g.y_offset);
                    g.y_offset = 0;
                }
                base.height = base.height.saturating_add(mark.height);
            }
            DOUBLE_ABOVE | ABOVE_LEFT | ABOVE | ABOVE_RIGHT | ATTACHED_ABOVE
            | ATTACHED_ABOVE_RIGHT => {
                if !matches!(class, ATTACHED_ABOVE | ATTACHED_ABOVE_RIGHT) {
                    base.y_bearing = base.y_bearing.saturating_add(y_gap);
                    base.height = base.height.saturating_sub(y_gap);
                }
                g.y_offset = clamp(
                    i64::from(base.y_bearing) - i64::from(mark.y_bearing) - i64::from(mark.height),
                );
                // Do not shift an above mark down too far.
                if (y_gap > 0) != (g.y_offset > 0) {
                    let correction = -(g.y_offset / 2);
                    base.y_bearing = base.y_bearing.saturating_add(correction);
                    base.height = base.height.saturating_sub(correction);
                    g.y_offset = g.y_offset.saturating_add(correction);
                }
                base.y_bearing = base.y_bearing.saturating_sub(mark.height);
                base.height = base.height.saturating_add(mark.height);
            }
            // Left and right marks are not positioned.
            _ => {}
        }
        Ok(())
    }

    /// `zero_mark_advances`: zeroes the advances of the nonspacing
    /// marks among `glyphs`.
    fn zero_mark_advances(&self, glyphs: &mut [Glyph]) {
        for g in glyphs
            .iter_mut()
            .filter(|g| g.char_class & char_class::NONSPACING_MARK != 0)
        {
            if self.adjust_offsets {
                g.x_offset = g.x_offset.saturating_sub(g.x_advance);
                g.y_offset = g.y_offset.saturating_sub(g.y_advance);
            }
            g.x_advance = 0;
            g.y_advance = 0;
        }
    }
}

/// `hb_clamp_to<hb_position_t>`.
fn clamp(v: i64) -> i32 {
    v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hebrew_and_arabic_classes_fold_into_positions() {
        use crate::unicode::normalize::modified_combining_class as mcc;
        let folded = |c: char| recategorize_combining_class(c, mcc(c));
        assert_eq!(folded('\u{05B0}'), BELOW); // sheva
        assert_eq!(folded('\u{05BD}'), BELOW); // meteg
        assert_eq!(folded('\u{05BF}'), ATTACHED_ABOVE); // rafe
        assert_eq!(folded('\u{05C1}'), ABOVE_RIGHT); // shin dot
        assert_eq!(folded('\u{05C2}'), ABOVE_LEFT); // sin dot
        assert_eq!(folded('\u{05B9}'), ABOVE_LEFT); // holam
        assert_eq!(folded('\u{05BC}'), 12); // dagesh keeps its class
        assert_eq!(folded('\u{064E}'), ABOVE); // fatha
        assert_eq!(folded('\u{0651}'), ABOVE); // shadda
        assert_eq!(folded('\u{0650}'), BELOW); // kasra
        assert_eq!(folded('\u{064D}'), BELOW); // kasratan
        assert_eq!(folded('\u{0301}'), ABOVE);
        // The classes the Arabic reordering gives modifier marks.
        assert_eq!(recategorize_combining_class('\u{0655}', 25), BELOW);
        assert_eq!(recategorize_combining_class('\u{0654}', 26), ABOVE);
    }

    #[test]
    fn thai_lao_and_tibetan_marks_get_positions() {
        use crate::unicode::normalize::modified_combining_class as mcc;
        let folded = |c: char| recategorize_combining_class(c, mcc(c));
        assert_eq!(folded('\u{0E31}'), ABOVE_RIGHT); // mai han-akat, class 0
        assert_eq!(folded('\u{0E38}'), BELOW_RIGHT); // sara u
        assert_eq!(folded('\u{0E48}'), ABOVE_RIGHT); // mai ek
        assert_eq!(folded('\u{0E3A}'), BELOW_RIGHT); // phinthu
        assert_eq!(folded('\u{0EB4}'), ABOVE);
        assert_eq!(folded('\u{0EBC}'), BELOW);
        assert_eq!(folded('\u{0EB8}'), BELOW);
        assert_eq!(folded('\u{0EC8}'), ABOVE);
        assert_eq!(folded('\u{0F71}'), BELOW);
        assert_eq!(folded('\u{0F72}'), ABOVE);
        assert_eq!(folded('\u{0F74}'), BELOW);
        assert_eq!(folded('a'), 0);
    }
}
