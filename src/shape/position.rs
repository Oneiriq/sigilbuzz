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
//!   [`Shaper::fallback_position`]). GPOS has the feature when the
//!   language system it picks lists it, even with no lookups, and the
//!   caller did not turn kerning off.
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

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::cell::{OnceCell, RefCell};

use super::attach::{self, Attach};
use super::fallback::{self, Extents, ExtentsTables, MarkPositioner};
use super::glyph_flags::FlagCx;
use super::gpos::{self, GposCx};
use super::shaper::{MarkZeroing, Shaper};
use super::{kern, Feature, LookupBudget, ProcessedSegment, VarCtx};
use crate::buffer::{Direction, Glyph};
use crate::error::Result;
use crate::face::Face;
use crate::tables::gdef::Gdef;
use crate::tables::glyf::PhantomMetrics;
use crate::tables::layout::{GlyphClasses, MatchGlyph};
use crate::tables::parse::{abs_f32, hb_round};
use crate::tables::{tag, Glyf, Gpos, Gvar, Hmtx, Hvar, Loca, Vmtx, Vorg, Vvar};
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
    /// The font's advances at the call's coords.
    pub(super) advances: &'a FontAdvances<'a, 'a>,
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
    fallback::adjust_spaces(face, input.advances, glyphs, horizontal)?;

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
    // HarfBuzz asks whether its feature map gives the kerning feature a
    // GPOS index: the run requests kerning and the language system GPOS
    // picks lists the feature. A FeatureVariations record that leaves
    // the feature no lookups does not bring the legacy tables back.
    let has_gpos_kern = apply_gpos
        && requested_kerning
        && input.gpos.is_some_and(|gpos| {
            segments
                .iter()
                .any(|s| lists_feature(gpos, kern_tag, s.script_priority))
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
            gdef: input.gdef,
            direction,
            ligature_direction,
            adjust_offsets,
            level: input.flags.level,
            advances: input.advances,
        };
        positioner.position_marks(glyphs)?;
    }
    Ok(())
}

/// HarfBuzz's default vertical positioning (`hb_ot_position_default`):
/// GPOS and the renderer work from a glyph's horizontal origin, so
/// every glyph of a vertical run is moved from its vertical origin
/// there. The vertical origin sits half the horizontal advance to the
/// right of the horizontal one and, vertically, where
/// [`FontAdvances::v_origin`] puts it.
pub(super) fn subtract_vertical_origins(advances: &FontAdvances<'_, '_>, glyphs: &mut [Glyph]) {
    for g in glyphs {
        let h_advance = advances.h_advance(g.glyph_id);
        g.x_offset -= h_advance / 2;
        g.y_offset -= advances.v_origin(g.glyph_id);
    }
}
/// True when the language system GPOS picks for the script tags of
/// `script_priority` lists feature `tag`, with or without lookups (see
/// [`crate::ot::layout_select::lists_feature`]).
fn lists_feature(gpos: &Gpos<'_>, tag: [u8; 4], script_priority: &[[u8; 4]]) -> bool {
    crate::ot::layout_select::lists_feature(
        gpos.script_list(),
        &gpos.features(),
        gpos.language_tags(),
        tag,
        script_priority,
    )
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

/// The font's advances and vertical origins at a set of coords, as
/// HarfBuzz's `hb_ot_get_glyph_h_advances`, `hb_ot_get_glyph_v_advances`
/// and `hb_ot_get_glyph_v_origins` read them. At the default instance
/// (no coords, or all zero) an advance is the `hmtx` (or `vmtx`) one.
/// Otherwise:
///
/// - with `HVAR` (or `VVAR`), the advance moves by the rounded delta,
///   and stops at zero, as HarfBuzz's `hb_max (0, ...)` stops it;
///   every rounding here is HarfBuzz's `roundf`, `floor(x + 0.5)`, so
///   halves round up (a delta of -13.5 moves the advance by -13);
/// - without it, in a `glyf` font with `gvar`, the advance is the
///   distance between the glyph's phantom points moved by their `gvar`
///   deltas (see [`crate::tables::Glyf::phantom_points_at_coords`]): the
///   first two in x horizontally, the last two in y vertically, rounded
///   and at least zero.
///
/// A vertical run of a font without `vmtx` advances every glyph by the
/// ascender-to-descender height (see [`FontAdvances::v_advance`]).
///
/// Each direction decides on its own: a font with `HVAR` but no `VVAR`,
/// the usual horizontal variable font, reads `gvar` for vertical
/// advances only. `gvar` is read when a glyph's advance first needs it,
/// and each glyph's phantom points and vertical origin are kept for the
/// rest of the call, as HarfBuzz caches them, so a glyph's outline is
/// walked at most once per direction however often it occurs.
///
/// When the phantom points cannot be computed (a `gvar` that does not
/// parse, a malformed glyph, a walk over its work budget), the glyph
/// keeps its `hmtx` (or `vmtx`) advance: the font's own default, and
/// what sigilbuzz used before it read phantom points. HarfBuzz
/// substitutes half an em (or an em) there.
///
/// Horizontal advances never need `vmtx`, which only places the
/// vertical phantom points: a horizontal run reads none, so a malformed
/// `vmtx` cannot fail it.
///
/// The pipeline builds one per shaping call and hands it to the
/// vertical origins, the fallback spaces, and the `stch` stretch.
pub(super) struct FontAdvances<'a, 'c> {
    face: &'c Face<'a>,
    coords: &'c [f32],
    hmtx: Hmtx<'a>,
    /// The vertical metrics of a vertical run.
    vmtx: Option<Vmtx<'a>>,
    /// The vertical origins of a vertical run.
    vorg: Option<Vorg<'a>>,
    /// `None` at the default instance.
    hvar: Option<Hvar<'a>>,
    /// `None` at the default instance and for a horizontal run.
    vvar: Option<Vvar<'a>>,
    /// The ascender and descender a vertical run falls back on (see
    /// [`font_h_extents`]); zero for a horizontal run.
    ascender: i32,
    descender: i32,
    /// `unitsPerEm` for a vertical run, zero for a horizontal one.
    upem: i32,
    /// Some coord is not zero.
    varied: bool,
    /// `glyf`, `loca` and, away from the default instance, `gvar`,
    /// read when first needed.
    outlines: OnceCell<Option<GlyfTables<'a>>>,
    /// Phantom-point advances computed so far, by glyph: `None` for a
    /// glyph whose phantom points could not be computed.
    h_phantom: RefCell<BTreeMap<u16, Option<i32>>>,
    /// The phantom points of a vertical run's glyphs, `vmtx` placing
    /// the top and bottom ones, varied by `gvar` when it parses:
    /// `None` for a glyph whose points could not be computed.
    v_phantom: RefCell<BTreeMap<u16, Option<PhantomPoints>>>,
    /// Vertical origins computed so far, by glyph.
    v_origins: RefCell<BTreeMap<u16, i32>>,
    /// The tables glyph extents come from, read when first needed.
    extents: ExtentsTables<'a>,
}

/// A glyph's four phantom points: left and right in x, top and bottom
/// in y (see [`crate::tables::Glyf::phantom_points_at_coords`]).
type PhantomPoints = [(f32, f32); 4];

/// The tables the phantom points come from.
struct GlyfTables<'a> {
    glyf: Glyf<'a>,
    loca: Loca<'a>,
    /// `None` at the default instance, and when the font has no `gvar`
    /// or its `gvar` does not parse (HarfBuzz's sanitizer drops it).
    gvar: Option<Gvar<'a>>,
}

impl<'a, 'c> FontAdvances<'a, 'c> {
    /// Reads the metrics tables the advances of `face` at `coords` come
    /// from. A `vertical` run also reads `vmtx`, `VORG` and `VVAR`; a
    /// horizontal one never does.
    pub(super) fn new(face: &'c Face<'a>, coords: &'c [f32], vertical: bool) -> Result<Self> {
        let varied = coords.iter().any(|&c| c != 0.0);
        let (vmtx, vorg) = if vertical {
            (face.vmtx()?, face.vorg()?)
        } else {
            (None, None)
        };
        let hvar = if varied { face.hvar()? } else { None };
        // HarfBuzz reads VVAR for the vertical origins of a VORG font
        // too, with or without vmtx.
        let vvar = if varied && (vmtx.is_some() || vorg.is_some()) {
            face.vvar()?
        } else {
            None
        };
        let ((ascender, descender), upem) = if vertical {
            (
                font_h_extents(face, coords)?,
                i32::from(face.head()?.units_per_em),
            )
        } else {
            ((0, 0), 0)
        };
        Ok(Self {
            face,
            coords,
            hmtx: face.hmtx()?,
            vmtx,
            vorg,
            hvar,
            vvar,
            ascender,
            descender,
            upem,
            varied,
            outlines: OnceCell::new(),
            h_phantom: RefCell::default(),
            v_phantom: RefCell::default(),
            v_origins: RefCell::default(),
            extents: ExtentsTables::default(),
        })
    }

    /// The ink extents of glyph `id` at the call's coords, as HarfBuzz's
    /// `hb_font_get_glyph_extents` computes them, or `None` when the
    /// font has no `glyf`, `CFF ` or `CFF2` (see
    /// [`ExtentsTables::glyph_extents`]). The tables are read once per
    /// call.
    pub(super) fn glyph_extents(&self, id: u16) -> Result<Option<Extents>> {
        self.extents
            .glyph_extents(self.face, self.coords, &self.hmtx, id)
    }

    /// The horizontal advance of glyph `id`, in font units.
    pub(super) fn h_advance(&self, id: u32) -> i32 {
        let id = id as u16;
        let base = i32::from(self.hmtx.advance(id).unwrap_or(0));
        if !self.varied {
            return base;
        }
        if let Some(hvar) = &self.hvar {
            let delta = hvar.advance_delta(id, self.coords);
            // HarfBuzz's `hb_max (0.0f, advance + roundf (delta))`.
            return base.saturating_add(hb_round(delta)).max(0);
        }
        self.h_phantom_advance(id).unwrap_or(base)
    }

    /// The vertical advance of glyph `id`, in font units, positive
    /// downward. Without `vmtx` it is the ascender-to-descender height
    /// of [`font_h_extents`], as in HarfBuzz.
    pub(super) fn v_advance(&self, id: u32) -> i32 {
        let Some(vmtx) = self.vmtx.as_ref() else {
            return self.ascender.saturating_sub(self.descender);
        };
        let id = id as u16;
        let base = i32::from(vmtx.advance(id).unwrap_or(0));
        if !self.varied {
            return base;
        }
        if let Some(vvar) = &self.vvar {
            let delta = vvar.advance_height_delta(id, self.coords);
            return base.saturating_add(hb_round(delta)).max(0);
        }
        // HarfBuzz takes the phantom points only from a gvar it reads.
        if !self.outlines().is_some_and(|t| t.gvar.is_some()) {
            return base;
        }
        match self.v_phantoms(id) {
            Some(pp) => hb_round(pp[2].1 - pp[3].1).max(0),
            None => base,
        }
    }

    /// The y of glyph `id`'s vertical origin, in font units, as
    /// HarfBuzz's `hb_ot_get_glyph_v_origins` places it:
    ///
    /// - with `VORG`, its value, moved away from the default instance
    ///   by the `VVAR` vertical origin delta and rounded;
    /// - else, in a `glyf` font with `vmtx`, the top phantom point:
    ///   the top of the glyph's box plus its `vmtx` top side bearing,
    ///   moved by its `gvar` delta away from the default instance and
    ///   rounded, or for a composite the top phantom point of its last
    ///   `USE_MY_METRICS` component (see
    ///   [`crate::tables::Glyf::phantom_points_at_coords`]). A glyph whose
    ///   points cannot be computed gets an em, as in HarfBuzz;
    /// - else the top of the glyph's box centered in the
    ///   ascender-to-descender span of [`font_h_extents`], with the box
    ///   varied as HarfBuzz varies glyph extents, or the ascender for a
    ///   glyph without extents (the font has no `glyf`, `CFF ` or
    ///   `CFF2`, or the outline is malformed).
    pub(super) fn v_origin(&self, id: u32) -> i32 {
        let id = id as u16;
        // The cell is only borrowed here and in the insert below, so
        // the borrows always succeed.
        if let Some(origin) = self
            .v_origins
            .try_borrow()
            .ok()
            .and_then(|cache| cache.get(&id).copied())
        {
            return origin;
        }
        let origin = self.compute_v_origin(id);
        if let Ok(mut cache) = self.v_origins.try_borrow_mut() {
            cache.insert(id, origin);
        }
        origin
    }

    /// [`FontAdvances::v_origin`] without the cache.
    fn compute_v_origin(&self, id: u16) -> i32 {
        if let Some(vorg) = &self.vorg {
            let y = vorg.vert_origin_y(id);
            let delta = if self.varied {
                self.vvar
                    .as_ref()
                    .and_then(|v| v.vorg_delta(id, self.coords))
            } else {
                None
            };
            return match delta {
                Some(delta) => hb_round(f32::from(y) + delta),
                None => i32::from(y),
            };
        }
        if self.vmtx.is_some() && self.outlines().is_some() {
            return self
                .v_phantoms(id)
                .map_or(self.upem, |pp| hb_round(pp[2].1));
        }
        let font_advance = i64::from(self.ascender) - i64::from(self.descender);
        match self.glyph_extents(id) {
            Ok(Some(e)) => {
                let origin = i64::from(e.y_bearing) + ((font_advance + i64::from(e.height)) >> 1);
                origin.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
            }
            _ => self.ascender,
        }
    }

    /// The tables the phantom points come from, or `None` when the font
    /// has no `glyf` or its `glyf` or `loca` cannot be read.
    fn outlines(&self) -> Option<&GlyfTables<'a>> {
        self.outlines
            .get_or_init(|| {
                let face = self.face;
                face.record(tag::GLYF)?;
                let (glyf, loca) = (face.glyf().ok()?, face.loca().ok()?);
                let gvar = if self.varied {
                    face.gvar().ok().flatten()
                } else {
                    None
                };
                Some(GlyfTables { glyf, loca, gvar })
            })
            .as_ref()
    }

    /// The advance of glyph `id` from its varied phantom points, or
    /// `None` when the font has no `glyf` or `gvar`, or the points
    /// cannot be computed; the caller then keeps the glyph's static
    /// advance.
    fn h_phantom_advance(&self, id: u16) -> Option<i32> {
        // The cells are only borrowed inside this method, which does
        // not call itself, so the borrows always succeed.
        if let Some(&advance) = self.h_phantom.try_borrow().ok()?.get(&id) {
            return advance;
        }
        let advance = self.outlines().and_then(|t| {
            let gvar = t.gvar.as_ref()?;
            let metrics = PhantomMetrics {
                hmtx: &self.hmtx,
                vmtx: None,
            };
            let pp = t
                .glyf
                .phantom_points_at_coords(&t.loca, id, Some(gvar), self.coords, &metrics)
                .ok()?;
            Some(hb_round(pp[1].0 - pp[0].0).max(0))
        });
        if let Ok(mut cache) = self.h_phantom.try_borrow_mut() {
            cache.insert(id, advance);
        }
        advance
    }

    /// The phantom points of glyph `id` in a vertical run, `vmtx`
    /// placing the top and bottom ones, moved by `gvar` when the run
    /// is varied and the font has a `gvar` that parses. `None` when the
    /// font has no `glyf` or the points cannot be computed.
    fn v_phantoms(&self, id: u16) -> Option<PhantomPoints> {
        // As in `h_phantom_advance`, the borrows always succeed.
        if let Some(&pp) = self.v_phantom.try_borrow().ok()?.get(&id) {
            return pp;
        }
        let pp = self.outlines().and_then(|t| {
            let metrics = PhantomMetrics {
                hmtx: &self.hmtx,
                vmtx: self.vmtx.as_ref(),
            };
            t.glyf
                .phantom_points_at_coords(&t.loca, id, t.gvar.as_ref(), self.coords, &metrics)
                .ok()
        });
        if let Ok(mut cache) = self.v_phantom.try_borrow_mut() {
            cache.insert(id, pp);
        }
        pp
    }
}

/// The ascender and descender HarfBuzz's vertical fallbacks read
/// (`hb_ot_get_font_h_extents`): the `hhea` values moved by the `MVAR`
/// `hasc` and `hdsc` deltas at `coords`, made positive and negative,
/// and rounded halves up. A font that asks for its `OS/2` typographic
/// metrics instead (`USE_TYPO_METRICS`) gets the `hhea` values here,
/// since sigilbuzz does not read `OS/2`. An `MVAR` that does not parse
/// counts as absent, as HarfBuzz's sanitizer drops it.
fn font_h_extents(face: &Face<'_>, coords: &[f32]) -> Result<(i32, i32)> {
    let hhea = face.hhea()?;
    let mvar = if coords.is_empty() {
        None
    } else {
        face.mvar().ok().flatten()
    };
    let delta = |tag: [u8; 4]| {
        mvar.as_ref()
            .and_then(|m| m.metric_delta(tag, coords))
            .unwrap_or(0.0)
    };
    let ascender = abs_f32(f32::from(hhea.ascent) + delta(*b"hasc"));
    let descender = -abs_f32(f32::from(hhea.descent) + delta(*b"hdsc"));
    Ok((hb_round(ascender), hb_round(descender)))
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
