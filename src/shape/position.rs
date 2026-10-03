//! The positioning pass: the fallback space widths, mark-width
//! zeroing, GPOS, the kerning fallbacks, and attachment resolution, in
//! HarfBuzz's order (`hb_ot_position_default` and `hb_ot_position_plan`
//! in hb-ot-shape.cc).
//!
//! Which table positions the run follows HarfBuzz's plan:
//!
//! - `kerx` replaces GPOS unless the font also has GSUB and GPOS.
//! - GPOS runs its stage (see [`super::gpos`]), unless the run's shaper
//!   names a script GPOS lacks (Hebrew needs `hebr`).
//! - When GPOS has no `kern` feature for the run (`vkrn` for vertical
//!   runs), `kerx` kerns if the font has it, else the legacy `kern`
//!   table does, for the shapers HarfBuzz lets fall back to it (see
//!   [`Shaper::fallback_position`]).
//! - A `kern` or `kerx` table the plan applies marks the whole run
//!   unsafe to concatenate, whether or not kerning is requested.
//!
//! Mark widths are zeroed before GPOS or after all positioning,
//! depending on the shaper HarfBuzz picks for the run's script (see
//! [`Shaper::mark_zeroing`]), and not at all when `kerx` or a
//! state-machine `kern` table does the positioning. When no GPOS or
//! `kerx` runs, a zeroed mark in a forward run also moves back by the
//! advance it lost, so it hangs over the glyph before it, and when the
//! shaper asks for it the marks then get fallback positions (see
//! [`fallback_mark_positioning`]).

use alloc::vec::Vec;

use super::attach::{self, Attach};
use super::fallback::{self, MarkPositioner};
use super::glyph_flags::FlagCx;
use super::gpos::{self, GposCx};
use super::shaper::{MarkZeroing, Shaper};
use super::{kern, Feature, LookupBudget, ProcessedSegment, VarCtx};
use crate::buffer::{Direction, Glyph};
use crate::error::Result;
use crate::face::Face;
use crate::tables::gdef::Gdef;
use crate::tables::layout::{GlyphClasses, MatchGlyph};
use crate::tables::{tag, Gpos};
use crate::unicode::Script;

/// Everything the positioning pass reads besides the glyphs.
pub(super) struct Inputs<'a> {
    pub(super) face: &'a Face<'a>,
    pub(super) gdef: Option<&'a Gdef<'a>>,
    pub(super) gpos: Option<&'a Gpos<'a>>,
    pub(super) var: &'a VarCtx<'a>,
    pub(super) direction: Direction,
    pub(super) features: &'a [Feature],
    /// Script of the run's first strong character: HarfBuzz picks one
    /// shaper per buffer from it.
    pub(super) dominant_script: Option<Script>,
    /// That shaper.
    pub(super) shaper: Shaper,
    /// The font has GSUB (HarfBuzz then prefers GPOS over `kerx`).
    pub(super) has_gsub: bool,
    /// The AAT `morx` table did the substitution.
    pub(super) applied_morx: bool,
    /// Default ignorables get zero advances (the buffer flags neither
    /// preserve nor remove them).
    pub(super) zero_ignorables: bool,
    /// The marks get fallback positions (see
    /// [`fallback_mark_positioning`]).
    pub(super) fallback_marks: bool,
    /// The shaping call's glyph flag settings.
    pub(super) flags: FlagCx,
}

/// True when the font's GPOS positions a run of `shaper`: HarfBuzz
/// ignores GPOS for a shaper with a `gpos_tag` (Hebrew) unless GPOS
/// has a script of that tag.
pub(super) fn gpos_applies(gpos: Option<&Gpos<'_>>, shaper: Shaper) -> bool {
    gpos.is_some_and(|gpos| {
        shaper
            .gpos_tag()
            .map_or(true, |tag| gpos.script_list().find(tag).is_some())
    })
}

/// HarfBuzz's `fallback_mark_positioning` plan flag (`hb-ot-shape.cc`):
/// the run's shaper asks for fallback positions, and neither GPOS,
/// `kerx`, nor a cross-stream legacy `kern` table positions the run. A
/// run the AAT `morx` table substitutes (`applies_morx`) uses
/// HarfBuzz's "dumber" shaper instead of any complex one, which never
/// falls back. Decided before normalization, which recategorizes the
/// marks' combining classes when it is on.
pub(super) fn fallback_mark_positioning(
    face: &Face<'_>,
    gpos: Option<&Gpos<'_>>,
    has_gsub: bool,
    applies_morx: bool,
    shaper: Shaper,
) -> Result<bool> {
    if !shaper.fallback_position() || (applies_morx && shaper != Shaper::Default) {
        return Ok(false);
    }
    let has_gpos = gpos_applies(gpos, shaper);
    let has_kerx = face.table_bytes(tag::KERX).is_ok();
    // kerx wins over GPOS unless the font also has GSUB.
    let apply_gpos = has_gpos && (has_gsub || !has_kerx);
    if apply_gpos || has_kerx {
        return Ok(false);
    }
    if face.table_bytes(tag::KERN).is_ok() {
        return Ok(!face.kern()?.is_some_and(|k| k.has_cross_stream()));
    }
    Ok(true)
}

/// Positions `glyphs` (default advances already set), segment by
/// segment for GPOS, and resolves attachments. Leaves the run in
/// logical order.
pub(super) fn position(
    input: &Inputs<'_>,
    glyphs: &mut [Glyph],
    segments: &[ProcessedSegment],
    budget: &mut LookupBudget,
) -> Result<()> {
    let direction = input.direction;
    let horizontal = direction.is_horizontal();
    let face = input.face;

    // Space characters drawn with the space glyph get their own
    // widths first, with the default advances (`hb_ot_position_default`).
    fallback::adjust_spaces(face, input.var.coords, glyphs, horizontal)?;

    // Kerning is requested by `kern` (on by default) for horizontal
    // runs and by `vkrn` (off by default) for vertical ones.
    let kern_tag = if horizontal { *b"kern" } else { *b"vkrn" };
    let requested_kerning = if horizontal {
        !super::feature_disabled(input.features, kern_tag)
    } else {
        super::feature_enabled(input.features, kern_tag)
    };

    // The plan only needs to know which tables exist; each one is
    // parsed only when it is going to run.
    let has_kerx = face.table_bytes(tag::KERX).is_ok();
    let has_kern = face.table_bytes(tag::KERN).is_ok();
    let has_gpos = gpos_applies(input.gpos, input.shaper);
    let mut apply_kerx = has_kerx && !(input.has_gsub && has_gpos);
    let apply_gpos = has_gpos && !apply_kerx;
    let has_gpos_kern = apply_gpos
        && input.gpos.is_some_and(|gpos| {
            segments
                .iter()
                .any(|s| !lookups_for(gpos, kern_tag, s.script_priority).is_empty())
        });
    let mut apply_kern = false;
    if !apply_kerx && !has_gpos_kern {
        if has_kerx {
            apply_kerx = true;
        } else if has_kern {
            // Not every HarfBuzz shaper applies the legacy table.
            apply_kern = input.shaper.fallback_position();
        }
    }
    let kern_table = if apply_kern { face.kern()? } else { None };

    let zeroing = input.shaper.mark_zeroing();
    // A state-machine `kern` table positions marks itself, and a
    // cross-stream one moves them across the line.
    let machine_kern = apply_kern && kern_table.as_ref().is_some_and(|k| k.has_state_machine());
    let cross_kern = apply_kern && kern_table.as_ref().is_some_and(|k| k.has_cross_stream());
    let zero_marks = !apply_kerx && !machine_kern;
    let adjust_offsets =
        !(apply_gpos || apply_kerx || cross_kern || input.applied_morx) && direction.is_forward();

    let mut slots = attach::new_slots(glyphs.len());
    if zero_marks && zeroing == MarkZeroing::Early {
        zero_mark_widths(glyphs, input.gdef, adjust_offsets);
    }
    if apply_gpos {
        if let Some(gpos) = input.gpos {
            let cx = GposCx {
                gpos,
                gdef: input.gdef,
                var: input.var,
                flags: input.flags,
            };
            for seg in segments {
                if seg.range.is_empty() {
                    continue;
                }
                let required = required_lookups(gpos, seg.script_priority);
                let lookups = gpos::stage_lookups(input.features, horizontal, &required, |tag| {
                    lookups_for(gpos, tag, seg.script_priority)
                });
                let (Some(seg_glyphs), Some(seg_slots)) = (
                    glyphs.get_mut(seg.range.clone()),
                    slots.get_mut(seg.range.clone()),
                ) else {
                    continue;
                };
                let mut att = Attach::new(direction, seg_slots, input.flags);
                gpos::apply_stage(&cx, seg_glyphs, &mut att, &lookups, budget);
            }
        }
    }
    // HarfBuzz's `KerxTable::apply` (hb-aat-layout-kerx-table.hh),
    // which runs `kern` too, starts by marking the whole run, and the
    // plan applies the table whether or not kerning is requested.
    if apply_kerx || apply_kern {
        input.flags.unsafe_to_concat_all(glyphs);
    }
    if requested_kerning {
        if apply_kerx {
            if let Some(kerx) = face.kerx()? {
                kern::apply_kerx_table(face, &kerx, glyphs, input.gdef, direction, input.flags)?;
            }
        } else if apply_kern {
            if let Some(ref table) = kern_table {
                kern::apply_kern_table(table, glyphs, input.gdef, direction, input.flags);
            }
        }
    }
    if zero_marks && zeroing == MarkZeroing::Late {
        zero_mark_widths(glyphs, input.gdef, adjust_offsets);
    }
    if input.zero_ignorables {
        super::ignorables::zero_width(glyphs, !horizontal);
    }

    // Attachment offsets are resolved only now, against the final
    // advances, with the direction-specific advance compensation.
    attach::resolve_attachments(glyphs, &mut slots, direction);

    if input.fallback_marks {
        // Ligature components run in the run's direction, or in its
        // script's for a vertical run.
        let ligature_direction = if horizontal {
            direction
        } else {
            input
                .dominant_script
                .map_or(Direction::Ltr, Script::horizontal_direction)
        };
        let positioner = MarkPositioner {
            face,
            coords: input.var.coords,
            gdef: input.gdef,
            direction,
            ligature_direction,
            adjust_offsets,
            level: input.flags.level,
        };
        positioner.position_marks(glyphs)?;
    }
    Ok(())
}

/// HarfBuzz's default vertical positioning (`hb_ot_position_default`):
/// GPOS and the renderer work from a glyph's horizontal origin, so
/// every glyph of a vertical run is moved from its vertical origin
/// there. The vertical origin sits half the horizontal advance to the
/// right of the horizontal one and, vertically, at the `VORG` value;
/// without `VORG` it is the top of the glyph's box plus the `vmtx` top
/// side bearing, or with no `vmtx` the top of a box centered in the
/// ascender-to-descender span. Glyphs with no outline data (CFF fonts
/// without `VORG`) use the ascender.
///
/// The ascender and descender come from `hhea`; a font that asks for
/// its `OS/2` typographic metrics instead (`USE_TYPO_METRICS`) gets
/// hhea values here, as it already does for the vertical advance
/// fallback.
pub(super) fn subtract_vertical_origins(
    face: &Face<'_>,
    coords: &[f32],
    glyphs: &mut [Glyph],
) -> Result<()> {
    let hmtx = face.hmtx()?;
    let hvar = if coords.is_empty() {
        None
    } else {
        face.hvar()?
    };
    let vorg = face.vorg()?;
    let vmtx = face.vmtx()?;
    let hhea = face.hhea()?;
    let (ascender, descender) = (i32::from(hhea.ascent), i32::from(hhea.descent));
    for g in glyphs {
        let id = g.glyph_id as u16;
        let mut h_advance = i32::from(hmtx.advance(id).unwrap_or(0));
        if let Some(ref hvar) = hvar {
            h_advance = h_advance.saturating_add(round_half_away(hvar.advance_delta(id, coords)));
        }
        let y_origin = match vorg {
            Some(ref vorg) => i32::from(vorg.vert_origin_y(id)),
            None => match glyph_top_and_height(face, id)? {
                Some((top, height)) => match vmtx {
                    Some(ref vmtx) => top + i32::from(vmtx.tsb(id).unwrap_or(0)),
                    None => top + (((ascender - descender) - height) >> 1),
                },
                None => ascender,
            },
        };
        g.x_offset -= h_advance / 2;
        g.y_offset -= y_origin;
    }
    Ok(())
}

/// Top (`yMax`) and height of a glyph's outline box from `glyf`: zero
/// for a glyph without an outline, `None` when the font has no `glyf`.
fn glyph_top_and_height(face: &Face<'_>, id: u16) -> Result<Option<(i32, i32)>> {
    match face.glyph_bounds(id) {
        Ok(Some(b)) => Ok(Some((
            i32::from(b.y_max),
            i32::from(b.y_max) - i32::from(b.y_min),
        ))),
        Ok(None) => Ok(Some((0, 0))),
        Err(crate::error::Error::MissingTable { .. }) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Rounds a variation delta to the nearest unit, halves away from
/// zero, the way the advance deltas are rounded elsewhere.
pub(super) fn round_half_away(delta: f32) -> i32 {
    if delta >= 0.0 {
        (delta + 0.5) as i32
    } else {
        (delta - 0.5) as i32
    }
}

/// True when GPOS has lookups for feature `tag` under the script tags
/// of `script_priority` (HarfBuzz's `has_gpos_mark` asks this of
/// `mark`).
pub(super) fn has_feature(gpos: &Gpos<'_>, tag: [u8; 4], script_priority: &[[u8; 4]]) -> bool {
    !lookups_for(gpos, tag, script_priority).is_empty()
}

/// Lookup indices feature `tag` selects for one segment's script.
fn lookups_for(gpos: &Gpos<'_>, tag: [u8; 4], script_priority: &[[u8; 4]]) -> Vec<u16> {
    crate::ot::layout_select::feature_lookup_indices(
        gpos.script_list(),
        &gpos.features(),
        gpos.language_tags(),
        tag,
        script_priority,
    )
    .unwrap_or_default()
}

/// Lookups of the required feature of the language system GPOS picks
/// for one segment's script, which join the GPOS stage whatever their
/// tag (HarfBuzz's `hb_ot_map_builder_t::compile`).
fn required_lookups(gpos: &Gpos<'_>, script_priority: &[[u8; 4]]) -> Vec<u16> {
    crate::ot::layout_select::required_feature(
        gpos.script_list(),
        &gpos.features(),
        gpos.language_tags(),
        script_priority,
    )
    .map(|(_, lookups)| lookups)
    .unwrap_or_default()
}

/// HarfBuzz's `zero_mark_widths_by_gdef`: every mark loses both
/// advances. Marks are GDEF's, or for a font without GDEF glyph
/// classes the synthesized ones. With `adjust_offsets` the mark first
/// moves back by the advance it loses, so it still sits over the
/// preceding glyph.
fn zero_mark_widths(glyphs: &mut [Glyph], gdef: Option<&Gdef<'_>>, adjust_offsets: bool) {
    let classes = GlyphClasses::new(gdef);
    for g in glyphs {
        if classes.is_mark(MatchGlyph::from(&*g)) {
            if adjust_offsets {
                g.x_offset -= g.x_advance;
                g.y_offset -= g.y_advance;
            }
            g.x_advance = 0;
            g.y_advance = 0;
        }
    }
}

/// The font's horizontal advance of glyph `id`: its `hmtx` advance,
/// moved by the `HVAR` delta at `coords` when the font varies (HarfBuzz's
/// `hb_font_get_glyph_h_advance`), rounded to the nearest unit.
pub(super) fn font_advance(
    hmtx: &crate::tables::hmtx::Hmtx<'_>,
    hvar: Option<&crate::tables::hvar::Hvar<'_>>,
    coords: &[f32],
    id: u32,
) -> i32 {
    let id = id as u16;
    let base = i32::from(hmtx.advance(id).unwrap_or(0));
    match hvar {
        Some(hvar) => {
            let delta = hvar.advance_delta(id, coords);
            // Round to nearest without libm: the deltas are small.
            let rounded = if delta >= 0.0 {
                (delta + 0.5) as i32
            } else {
                (delta - 0.5) as i32
            };
            base.saturating_add(rounded)
        }
        None => base,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The mark-zeroing behavior of the shaper HarfBuzz uses for
    /// `script`. Sinhala, Tibetan, Mongolian and N'Ko all go to the
    /// Universal Shaping Engine there, whatever pipeline sigilbuzz runs
    /// them through.
    fn zeroing(script: Script) -> MarkZeroing {
        Shaper::for_script(script, true).mark_zeroing()
    }

    #[test]
    fn zeroing_follows_the_harfbuzz_shaper_of_each_script() {
        for s in [
            Script::Tibetan,
            Script::Brahmi,
            Script::Sharada,
            Script::Khojki,
            Script::Tirhuta,
            Script::Modi,
            Script::Mongolian,
            Script::NKo,
            Script::Sinhala,
            Script::Myanmar,
            Script::Javanese,
            Script::Adlam,
        ] {
            assert_eq!(zeroing(s), MarkZeroing::Early, "{s:?}");
        }
        for s in [
            Script::Devanagari,
            Script::Tamil,
            Script::Khmer,
            Script::Hangul,
        ] {
            assert_eq!(zeroing(s), MarkZeroing::None, "{s:?}");
        }
        for s in [
            Script::Arabic,
            Script::Syriac,
            Script::Hebrew,
            Script::Thai,
            Script::Latin,
            Script::Other,
        ] {
            assert_eq!(zeroing(s), MarkZeroing::Late, "{s:?}");
        }
    }

    #[test]
    fn zeroing_clears_both_advances_and_can_pull_the_mark_back() {
        // GDEF v1.0 marking glyph 2 as a mark.
        let mut bytes = alloc::vec![0, 1, 0, 0, 0, 12, 0, 0, 0, 0, 0, 0];
        bytes.extend_from_slice(&[0, 1, 0, 2, 0, 1, 0, 3]);
        let gdef = Gdef::parse(&bytes).unwrap();
        let mut glyphs = [Glyph::new(1, 0), Glyph::new(2, 1)];
        for g in &mut glyphs {
            g.x_advance = 300;
            g.y_advance = -1000;
        }
        zero_mark_widths(&mut glyphs, Some(&gdef), false);
        assert_eq!((glyphs[1].x_advance, glyphs[1].y_advance), (0, 0));
        assert_eq!((glyphs[1].x_offset, glyphs[1].y_offset), (0, 0));
        assert_eq!((glyphs[0].x_advance, glyphs[0].y_advance), (300, -1000));

        let mut glyphs = [Glyph::new(2, 0)];
        glyphs[0].x_advance = 300;
        zero_mark_widths(&mut glyphs, Some(&gdef), true);
        assert_eq!((glyphs[0].x_advance, glyphs[0].x_offset), (0, -300));
    }
}
