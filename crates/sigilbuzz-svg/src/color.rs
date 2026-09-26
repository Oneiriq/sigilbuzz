//! COLRv1 -> SVG emission, gated on the `color` Cargo feature.
//!
//! This module consumes the [`DrawCmd`] stream from `sigilbuzz-paint`
//! and turns it into an SVG fragment that mirrors the COLRv1 paint
//! tree's intent:
//!
//! - solid `PaintSolid` leaves render as `<path fill="rgb(...)" .../>`.
//! - linear / radial gradients land in a `<defs>` block and are
//!   referenced via `fill="url(#grad-N)"`.
//! - fills and gradient stops on COLR palette entry `0xFFFF` (the text
//!   color) use `currentColor`, with the paint alpha as `fill-opacity`
//!   or `stop-opacity`, so the glyph takes the color of the text it is
//!   embedded in.
//! - sweep gradients have no SVG 1.1 equivalent. We degrade them to a
//!   linear gradient running across the gradient's center. The
//!   colors are right, the angular distribution is not. The output
//!   carries an `<!-- sweep-fallback -->` comment so consumers that
//!   care can detect the substitution and route through a richer
//!   renderer.
//! - `PushLayer` / `PopLayer` map to `<g>` wrappers; SVG's blend modes
//!   only cover a subset of the COLRv1 composite list, so unsupported
//!   modes are passed through as `style="mix-blend-mode: <name>"` and
//!   left to the SVG viewer's CSS engine.
//!
//! The walker re-walks the same DrawCmd stream sigilbuzz-paint emits
//! to keep behavior aligned with other renderers built on the
//! evaluator (PDF backend, GPU backend). It does not parse the COLRv1
//! tree directly. That would duplicate the var-store / cycle-bounded
//! logic the evaluator already owns.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write as _;

use sigilbuzz::Face;
use sigilbuzz_paint::{
    evaluate_with, Color, ColorStop, CompositeMode, DrawCmd, EvalOptions, Gradient, GradientKind,
    PaintSource, Transform2D,
};

use crate::{path_bbox, path_data, push_num, F2Dot14, GlyphId, VIEWBOX_MARGIN};

/// Emits a complete `<svg>` document for `gid`'s COLRv1 color glyph.
///
/// Returns `None` when the glyph has no COLRv1 paint or none of the
/// referenced outline glyphs carry a non-empty path. In that case the
/// caller can fall back to [`crate::glyph_to_svg`] for a black
/// outline rendering.
///
/// Paints on the foreground palette entry (the text color) are filled
/// with `currentColor`, so the glyph inherits the CSS `color` of the
/// element it is placed in (black when nothing sets it). Palette
/// entries the font cannot supply are filled black.
#[must_use]
pub fn glyph_to_svg_color(face: &Face<'_>, gid: GlyphId) -> Option<String> {
    glyph_to_svg_color_at_coords(face, gid, &[])
}

/// Variable-font flavor of [`glyph_to_svg_color`].
#[must_use]
pub fn glyph_to_svg_color_at_coords(
    face: &Face<'_>,
    gid: GlyphId,
    coords: &[F2Dot14],
) -> Option<String> {
    // An opaque foreground keeps foreground alphas equal to the paint
    // alpha, which is what `fill-opacity` / `stop-opacity` need next to
    // `currentColor`.
    let options = EvalOptions::new()
        .with_coords(coords)
        .with_foreground(Color::BLACK);
    let cmds = evaluate_with(face, gid, &options);
    if cmds.is_empty() {
        return None;
    }
    render_color_svg(face, &cmds, coords)
}

// =========================================================================
// Render pipeline
// =========================================================================

fn render_color_svg(face: &Face<'_>, cmds: &[DrawCmd], coords: &[F2Dot14]) -> Option<String> {
    // First pass: for every FillGlyph in `cmds`, look up the
    // referenced outline. We push *one* `Option<LeafGeometry>` per
    // FillGlyph: `None` for whitespace / out-of-range / empty-outline
    // glyphs, `Some` for renderable ones. Storing one slot per
    // FillGlyph keeps the second pass aligned with the cmd stream
    // even when an interior leaf is missing; before, leaves were a
    // dense Vec and the second pass walked the full cmd stream, so
    // a missing-outline FillGlyph in the middle silently re-mapped
    // every later FillGlyph to the wrong leaf (issue #68).
    let mut bbox: Option<(f32, f32, f32, f32)> = None;
    let mut leaves: Vec<Option<LeafGeometry>> = Vec::new();
    for cmd in cmds {
        if let DrawCmd::FillGlyph { gid, transform, .. } = cmd {
            let leaf = build_leaf(face, *gid, coords, *transform);
            if let Some((leaf, projected)) = leaf {
                bbox = Some(union_bbox(bbox, projected));
                leaves.push(Some(leaf));
            } else {
                leaves.push(None);
            }
        }
    }
    let bbox = bbox?;
    if leaves.iter().all(Option::is_none) {
        return None;
    }

    // Second pass: walk the cmd stream alongside the leaf list,
    // emitting defs (gradients) and the body (paths + groups). The
    // FillGlyph counter advances on every FillGlyph cmd whether or
    // not its leaf is `Some`, keeping the two streams in lockstep.
    let (defs, body) = emit_defs_and_body(cmds, &leaves);

    Some(assemble_svg(bbox, &defs, &body))
}

/// Resolves one FillGlyph's outline + transform into a `LeafGeometry`
/// plus its projected bbox. Returns `None` when the outline is missing
/// or empty, or when the bbox computation has nothing to fold (a
/// `Close`-only path stream, in theory).
fn build_leaf(
    face: &Face<'_>,
    gid: GlyphId,
    coords: &[F2Dot14],
    transform: Transform2D,
) -> Option<(LeafGeometry, (f32, f32, f32, f32))> {
    let outline = face.glyph_outline_at_coords(gid, coords).ok().flatten()?;
    if outline.is_empty() {
        return None;
    }
    let d = path_data(outline.ops());
    let local_bbox = path_bbox(outline.ops())?;
    let projected = project_bbox(transform, local_bbox);
    Some((LeafGeometry { d }, projected))
}

struct LeafGeometry {
    d: String,
}

#[derive(Default)]
struct Defs {
    fragments: Vec<String>,
    next_id: u32,
}

impl Defs {
    fn allocate_id(&mut self, prefix: &str) -> String {
        let id = format!("{prefix}-{}", self.next_id);
        self.next_id += 1;
        id
    }
    fn push(&mut self, frag: String) {
        self.fragments.push(frag);
    }
    fn into_svg(self) -> String {
        if self.fragments.is_empty() {
            String::new()
        } else {
            let mut out = String::from("<defs>");
            for f in self.fragments {
                out.push_str(&f);
            }
            out.push_str("</defs>");
            out
        }
    }
}

fn emit_defs_and_body(cmds: &[DrawCmd], leaves: &[Option<LeafGeometry>]) -> (String, String) {
    let mut defs = Defs::default();
    let mut body = String::new();
    let mut leaf_idx: usize = 0;

    for cmd in cmds {
        match cmd {
            DrawCmd::FillGlyph {
                transform, paint, ..
            } => {
                // The leaf list has exactly one slot per FillGlyph in
                // input order; advance on every FillGlyph so the next
                // one keeps lockstep with the cmd stream. Missing-
                // outline glyphs (`None` slot) emit nothing.
                let slot = leaves.get(leaf_idx);
                leaf_idx += 1;
                if let Some(Some(leaf)) = slot {
                    emit_fill(&mut defs, &mut body, *transform, paint, &leaf.d);
                }
            }
            DrawCmd::PushLayer { composite_mode } => {
                push_layer(&mut body, *composite_mode);
            }
            DrawCmd::PopLayer => {
                body.push_str("</g>");
            }
        }
    }

    (defs.into_svg(), body)
}

fn emit_fill(
    defs: &mut Defs,
    body: &mut String,
    transform: Transform2D,
    paint: &PaintSource,
    d: &str,
) {
    let xform = transform_attr(transform);
    match paint {
        // The evaluator's foreground is opaque, so a foreground fill's
        // alpha is exactly the paint alpha.
        PaintSource::Solid {
            color: c,
            is_foreground,
        } => {
            body.push_str("<path");
            if let Some(t) = xform {
                let _ = write!(body, r#" transform="{t}""#);
            }
            let _ = write!(
                body,
                r#" d="{d}" fill="{}""#,
                fill_color(*c, *is_foreground)
            );
            if c.a < 1.0 - 1e-6 {
                let _ = write!(body, r#" fill-opacity="{}""#, fmt_num(c.a));
            }
            body.push_str("/>");
        }
        PaintSource::Gradient(g) => {
            let id = emit_gradient_def(defs, g);
            body.push_str("<path");
            if let Some(t) = xform {
                let _ = write!(body, r#" transform="{t}""#);
            }
            let _ = write!(body, r#" d="{d}" fill="url(#{id})"/>"#);
        }
    }
}

fn push_layer(body: &mut String, mode: CompositeMode) {
    let blend = composite_to_blend_mode(mode);
    let _ = write!(body, r#"<g style="mix-blend-mode:{blend}">"#);
}

// =========================================================================
// Gradient defs
// =========================================================================

fn emit_gradient_def(defs: &mut Defs, g: &Gradient) -> String {
    match g.kind {
        GradientKind::Linear { p0, p1, .. } => {
            let id = defs.allocate_id("grad");
            let mut s = String::new();
            let _ = write!(
                s,
                r#"<linearGradient id="{id}" gradientUnits="userSpaceOnUse" x1="{}" y1="{}" x2="{}" y2="{}" spreadMethod="{}">"#,
                fmt_num(p0.0),
                fmt_num(p0.1),
                fmt_num(p1.0),
                fmt_num(p1.1),
                spread_method(g),
            );
            for stop in &g.stops {
                s.push_str(&stop_tag(stop));
            }
            s.push_str("</linearGradient>");
            defs.push(s);
            id
        }
        GradientKind::Radial { c0, r0, c1, r1 } => {
            let id = defs.allocate_id("grad");
            // SVG `radialGradient` lays out as (cx, cy) outer, (fx, fy) inner.
            // The inner radius `r0` is exposed via the SVG2 `fr` attribute;
            // some viewers respect it, others ignore it. Either way we emit
            // both circles so the data is preserved.
            let mut s = String::new();
            let _ = write!(
                s,
                r#"<radialGradient id="{id}" gradientUnits="userSpaceOnUse" cx="{}" cy="{}" r="{}" fx="{}" fy="{}" fr="{}" spreadMethod="{}">"#,
                fmt_num(c1.0),
                fmt_num(c1.1),
                fmt_num(r1),
                fmt_num(c0.0),
                fmt_num(c0.1),
                fmt_num(r0),
                spread_method(g),
            );
            for stop in &g.stops {
                s.push_str(&stop_tag(stop));
            }
            s.push_str("</radialGradient>");
            defs.push(s);
            id
        }
        GradientKind::Sweep {
            center,
            start_angle,
            end_angle,
        } => {
            // SVG 1.1 has no sweep gradient. We approximate with a
            // linear gradient running through the sweep's center,
            // oriented along the sector's bisector. The color bands
            // are placed in input order across the sweep's angular
            // extent; the spatial distribution is wrong but the
            // colors are preserved. A `<!-- sweep-fallback -->`
            // marker lets consumers detect and re-route.
            let id = defs.allocate_id("grad");
            let bisector = 0.5 * (start_angle + end_angle);
            let dx = bisector.cos();
            let dy = bisector.sin();
            let r = 1.0_f32; // unit-vector axis; userSpaceOnUse keeps coords stable.
            let mut s = String::new();
            s.push_str("<!-- sweep-fallback -->");
            let _ = write!(
                s,
                r#"<linearGradient id="{id}" gradientUnits="userSpaceOnUse" x1="{}" y1="{}" x2="{}" y2="{}" spreadMethod="{}">"#,
                fmt_num(center.0 - dx * r),
                fmt_num(center.1 - dy * r),
                fmt_num(center.0 + dx * r),
                fmt_num(center.1 + dy * r),
                spread_method(g),
            );
            for stop in &g.stops {
                s.push_str(&stop_tag(stop));
            }
            s.push_str("</linearGradient>");
            defs.push(s);
            id
        }
    }
}

fn spread_method(g: &Gradient) -> &'static str {
    match g.extend {
        sigilbuzz_paint::Extend::Pad => "pad",
        sigilbuzz_paint::Extend::Repeat => "repeat",
        sigilbuzz_paint::Extend::Reflect => "reflect",
    }
}

fn stop_tag(stop: &ColorStop) -> String {
    let color = fill_color(stop.color, stop.is_foreground);
    if (stop.color.a - 1.0).abs() < 1e-6 {
        format!(
            r#"<stop offset="{}" stop-color="{color}"/>"#,
            fmt_num(stop.offset),
        )
    } else {
        format!(
            r#"<stop offset="{}" stop-color="{color}" stop-opacity="{}"/>"#,
            fmt_num(stop.offset),
            fmt_num(stop.color.a),
        )
    }
}

// =========================================================================
// Color + transform helpers
// =========================================================================

/// The SVG paint for a resolved color: `currentColor` for the COLR
/// foreground entry, so the glyph follows the surrounding text color,
/// else an `rgb(...)` literal. Alpha is emitted separately.
fn fill_color(c: Color, is_foreground: bool) -> String {
    if is_foreground {
        String::from("currentColor")
    } else {
        color_to_rgb(c)
    }
}

fn color_to_rgb(c: Color) -> String {
    let r = (c.r.clamp(0.0, 1.0) * 255.0).round() as u32;
    let g = (c.g.clamp(0.0, 1.0) * 255.0).round() as u32;
    let b = (c.b.clamp(0.0, 1.0) * 255.0).round() as u32;
    format!("rgb({r},{g},{b})")
}

fn transform_attr(t: Transform2D) -> Option<String> {
    if (t.xx - 1.0).abs() < 1e-6
        && (t.yx).abs() < 1e-6
        && (t.xy).abs() < 1e-6
        && (t.yy - 1.0).abs() < 1e-6
        && (t.dx).abs() < 1e-6
        && (t.dy).abs() < 1e-6
    {
        return None;
    }
    Some(format!(
        "matrix({} {} {} {} {} {})",
        fmt_num(t.xx),
        fmt_num(t.yx),
        fmt_num(t.xy),
        fmt_num(t.yy),
        fmt_num(t.dx),
        fmt_num(t.dy),
    ))
}

fn fmt_num(v: f32) -> String {
    let mut s = String::new();
    push_num(&mut s, v);
    s
}

fn composite_to_blend_mode(mode: CompositeMode) -> &'static str {
    // The COLRv1 spec carries the full Porter-Duff alphabet plus the
    // PDF blend modes. CSS / SVG only standardize the PDF blend modes.
    // Porter-Duff cases that have no CSS equivalent fall back to
    // `normal` so the output stays renderable. Consumers wanting full
    // fidelity should drive sigilbuzz-paint into a Porter-Duff-aware
    // backend (sigilbuzz-gpu, future sigilbuzz-pdf).
    match mode {
        CompositeMode::Clear => "normal",
        CompositeMode::Src => "normal",
        CompositeMode::Dest => "normal",
        CompositeMode::SrcOver => "normal",
        CompositeMode::DestOver => "normal",
        CompositeMode::SrcIn => "normal",
        CompositeMode::DestIn => "normal",
        CompositeMode::SrcOut => "normal",
        CompositeMode::DestOut => "normal",
        CompositeMode::SrcAtop => "normal",
        CompositeMode::DestAtop => "normal",
        CompositeMode::Xor => "normal",
        CompositeMode::Plus => "normal",
        CompositeMode::Screen => "screen",
        CompositeMode::Overlay => "overlay",
        CompositeMode::Darken => "darken",
        CompositeMode::Lighten => "lighten",
        CompositeMode::ColorDodge => "color-dodge",
        CompositeMode::ColorBurn => "color-burn",
        CompositeMode::HardLight => "hard-light",
        CompositeMode::SoftLight => "soft-light",
        CompositeMode::Difference => "difference",
        CompositeMode::Exclusion => "exclusion",
        CompositeMode::Multiply => "multiply",
        CompositeMode::HslHue => "hue",
        CompositeMode::HslSaturation => "saturation",
        CompositeMode::HslColor => "color",
        CompositeMode::HslLuminosity => "luminosity",
    }
}

// =========================================================================
// Bounding-box composition
// =========================================================================

fn project_bbox(t: Transform2D, b: (f32, f32, f32, f32)) -> (f32, f32, f32, f32) {
    let corners = [
        t.apply(b.0, b.1),
        t.apply(b.2, b.1),
        t.apply(b.0, b.3),
        t.apply(b.2, b.3),
    ];
    let mut mnx = f32::INFINITY;
    let mut mny = f32::INFINITY;
    let mut mxx = f32::NEG_INFINITY;
    let mut mxy = f32::NEG_INFINITY;
    for (x, y) in corners {
        if x < mnx {
            mnx = x;
        }
        if x > mxx {
            mxx = x;
        }
        if y < mny {
            mny = y;
        }
        if y > mxy {
            mxy = y;
        }
    }
    (mnx, mny, mxx, mxy)
}

fn union_bbox(a: Option<(f32, f32, f32, f32)>, b: (f32, f32, f32, f32)) -> (f32, f32, f32, f32) {
    match a {
        None => b,
        Some(a) => (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3)),
    }
}

// =========================================================================
// Outer SVG framing
// =========================================================================

fn assemble_svg(bbox: (f32, f32, f32, f32), defs: &str, body: &str) -> String {
    let (min_x, min_y, max_x, max_y) = bbox;
    let vx = min_x - VIEWBOX_MARGIN;
    let vy = min_y - VIEWBOX_MARGIN;
    let vw = (max_x - min_x) + 2.0 * VIEWBOX_MARGIN;
    let vh = (max_y - min_y) + 2.0 * VIEWBOX_MARGIN;
    let flip_offset = vy + vh + vy;
    let mut out = String::new();
    out.push_str(r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox=""#);
    out.push_str(&fmt_num(vx));
    out.push(' ');
    out.push_str(&fmt_num(vy));
    out.push(' ');
    out.push_str(&fmt_num(vw));
    out.push(' ');
    out.push_str(&fmt_num(vh));
    out.push_str(r#"">"#);
    out.push_str(defs);
    out.push_str(r#"<g transform="matrix(1 0 0 -1 0 "#);
    out.push_str(&fmt_num(flip_offset));
    out.push_str(r#")">"#);
    out.push_str(body);
    out.push_str("</g></svg>");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solid_color_is_rgb() {
        let c = Color::new(1.0, 0.5, 0.0, 1.0);
        assert_eq!(color_to_rgb(c), "rgb(255,128,0)");
    }

    #[test]
    fn identity_transform_is_omitted() {
        assert!(transform_attr(Transform2D::IDENTITY).is_none());
    }

    #[test]
    fn non_identity_transform_serialises() {
        let t = Transform2D::translate(10.0, 20.0);
        let s = transform_attr(t).unwrap();
        assert_eq!(s, "matrix(1 0 0 1 10 20)");
    }

    #[test]
    fn linear_gradient_def_includes_all_stops() {
        use sigilbuzz_paint::{ColorStop, Extend, Gradient, GradientKind};
        let g = Gradient {
            kind: GradientKind::Linear {
                p0: (0.0, 0.0),
                p1: (100.0, 0.0),
                p2: (0.0, 100.0),
            },
            stops: alloc::vec![
                ColorStop::new(0.0, Color::new(1.0, 0.0, 0.0, 1.0)),
                ColorStop::new(0.5, Color::new(0.0, 1.0, 0.0, 1.0)),
                ColorStop::new(1.0, Color::new(0.0, 0.0, 1.0, 1.0)),
            ],
            extend: Extend::Pad,
        };
        let mut defs = Defs::default();
        let id = emit_gradient_def(&mut defs, &g);
        let svg = defs.into_svg();
        assert!(svg.contains("<linearGradient"), "missing tag: {svg}");
        assert_eq!(svg.matches("<stop ").count(), 3);
        assert!(svg.contains(&format!(r#"id="{id}""#)));
        assert!(svg.contains(r#"x1="0""#));
        assert!(svg.contains(r#"x2="100""#));
        assert!(svg.contains(r#"spreadMethod="pad""#));
        assert!(svg.contains(r#"stop-color="rgb(255,0,0)""#));
    }

    #[test]
    fn radial_gradient_def_includes_all_stops() {
        use sigilbuzz_paint::{ColorStop, Extend, Gradient, GradientKind};
        let g = Gradient {
            kind: GradientKind::Radial {
                c0: (10.0, 20.0),
                r0: 5.0,
                c1: (10.0, 20.0),
                r1: 50.0,
            },
            stops: alloc::vec![
                ColorStop::new(0.0, Color::new(1.0, 1.0, 1.0, 1.0)),
                ColorStop::new(1.0, Color::new(0.0, 0.0, 0.0, 1.0)),
            ],
            extend: Extend::Reflect,
        };
        let mut defs = Defs::default();
        emit_gradient_def(&mut defs, &g);
        let svg = defs.into_svg();
        assert!(svg.contains("<radialGradient"));
        assert!(svg.contains(r#"r="50""#));
        assert!(svg.contains(r#"fr="5""#));
        assert!(svg.contains(r#"spreadMethod="reflect""#));
    }

    #[test]
    fn sweep_gradient_falls_back_to_linear_with_marker() {
        use sigilbuzz_paint::{ColorStop, Extend, Gradient, GradientKind};
        let g = Gradient {
            kind: GradientKind::Sweep {
                center: (0.0, 0.0),
                start_angle: 0.0,
                end_angle: core::f32::consts::PI,
            },
            stops: alloc::vec![
                ColorStop::new(0.0, Color::new(1.0, 0.0, 0.0, 1.0)),
                ColorStop::new(1.0, Color::new(0.0, 0.0, 1.0, 1.0)),
            ],
            extend: Extend::Pad,
        };
        let mut defs = Defs::default();
        emit_gradient_def(&mut defs, &g);
        let svg = defs.into_svg();
        // SVG 1.1 has no sweep. We emit a linearGradient and prefix
        // it with a sweep-fallback comment so consumers can detect.
        assert!(svg.contains("<!-- sweep-fallback -->"));
        assert!(svg.contains("<linearGradient"));
        assert!(!svg.contains("<radialGradient"));
    }

    #[test]
    fn solid_fill_includes_opacity_when_alpha_below_one() {
        let mut defs = Defs::default();
        let mut body = String::new();
        emit_fill(
            &mut defs,
            &mut body,
            Transform2D::IDENTITY,
            &PaintSource::Solid {
                color: Color::new(0.5, 0.5, 0.5, 0.5),
                is_foreground: false,
            },
            "M 0 0 Z",
        );
        assert!(body.contains(r#"fill="rgb(128,128,128)""#));
        assert!(body.contains(r#"fill-opacity="0.5""#));
    }

    #[test]
    fn solid_fill_omits_opacity_at_full_alpha() {
        let mut defs = Defs::default();
        let mut body = String::new();
        emit_fill(
            &mut defs,
            &mut body,
            Transform2D::IDENTITY,
            &PaintSource::Solid {
                color: Color::new(0.0, 0.0, 0.0, 1.0),
                is_foreground: false,
            },
            "M 0 0 Z",
        );
        assert!(!body.contains("fill-opacity"));
    }

    #[test]
    fn foreground_solid_fills_with_current_color() {
        let mut defs = Defs::default();
        let mut body = String::new();
        emit_fill(
            &mut defs,
            &mut body,
            Transform2D::IDENTITY,
            &PaintSource::Solid {
                color: Color::new(0.0, 0.0, 0.0, 0.5),
                is_foreground: true,
            },
            "M 0 0 Z",
        );
        assert!(body.contains(r#"fill="currentColor""#), "{body}");
        assert!(body.contains(r#"fill-opacity="0.5""#), "{body}");
        assert!(!body.contains("rgb("), "{body}");
    }

    #[test]
    fn foreground_stops_use_current_color() {
        use sigilbuzz_paint::{Extend, Gradient, GradientKind};
        let g = Gradient {
            kind: GradientKind::Linear {
                p0: (0.0, 0.0),
                p1: (100.0, 0.0),
                p2: (0.0, 100.0),
            },
            stops: alloc::vec![
                ColorStop::new(0.0, Color::new(1.0, 0.0, 0.0, 1.0)),
                ColorStop {
                    offset: 1.0,
                    color: Color::new(0.0, 0.0, 0.0, 0.25),
                    is_foreground: true,
                },
                ColorStop {
                    offset: 1.0,
                    color: Color::BLACK,
                    is_foreground: true,
                },
            ],
            extend: Extend::Pad,
        };
        let mut defs = Defs::default();
        emit_gradient_def(&mut defs, &g);
        let svg = defs.into_svg();
        assert!(
            svg.contains(r#"<stop offset="0" stop-color="rgb(255,0,0)"/>"#),
            "{svg}"
        );
        assert!(
            svg.contains(r#"<stop offset="1" stop-color="currentColor" stop-opacity="0.25"/>"#),
            "{svg}"
        );
        assert!(
            svg.contains(r#"<stop offset="1" stop-color="currentColor"/>"#),
            "{svg}"
        );
    }

    #[test]
    fn push_layer_emits_blend_mode_group() {
        let mut body = String::new();
        push_layer(&mut body, CompositeMode::Multiply);
        assert!(body.contains(r#"style="mix-blend-mode:multiply""#));
    }

    #[test]
    fn missing_middle_outline_does_not_misalign_later_leaves() {
        // Three FillGlyph cmds with distinguishable solid colors;
        // the middle glyph has no outline (its leaf slot is `None`).
        // Before issue #68 the second-pass walker advanced its
        // dense-leaf cursor only on `Some` slots, so the third
        // FillGlyph silently picked up the second's `d=` payload, and
        // here the fix routes each FillGlyph through its own
        // matching leaf slot, missing-outline ones emit nothing,
        // and later glyphs keep the path data the first pass paired
        // with them.
        let cmds = alloc::vec![
            DrawCmd::FillGlyph {
                gid: 1,
                transform: Transform2D::IDENTITY,
                paint: PaintSource::Solid {
                    color: Color::new(1.0, 0.0, 0.0, 1.0),
                    is_foreground: false
                },
            },
            DrawCmd::FillGlyph {
                gid: 2,
                transform: Transform2D::IDENTITY,
                paint: PaintSource::Solid {
                    color: Color::new(0.0, 1.0, 0.0, 1.0),
                    is_foreground: false
                },
            },
            DrawCmd::FillGlyph {
                gid: 3,
                transform: Transform2D::IDENTITY,
                paint: PaintSource::Solid {
                    color: Color::new(0.0, 0.0, 1.0, 1.0),
                    is_foreground: false
                },
            },
        ];
        let leaves: alloc::vec::Vec<Option<LeafGeometry>> = alloc::vec![
            Some(LeafGeometry {
                d: alloc::string::String::from("M 0 0 L 1 0 Z"),
            }),
            None,
            Some(LeafGeometry {
                d: alloc::string::String::from("M 0 0 L 3 0 Z"),
            }),
        ];

        let (_defs, body) = emit_defs_and_body(&cmds, &leaves);
        // The first FillGlyph (red) should land on its own leaf.
        assert!(
            body.contains(r#"d="M 0 0 L 1 0 Z" fill="rgb(255,0,0)""#),
            "first fill missing or wrong d=: {body}"
        );
        // The second FillGlyph (green) had no outline and emits
        // nothing. Its color must not appear anywhere in the body.
        assert!(
            !body.contains("rgb(0,255,0)"),
            "missing-outline glyph leaked into body: {body}"
        );
        // The third FillGlyph (blue) keeps its original `d` rather
        // than picking up the second slot's path. Before the fix
        // this assertion failed because the dense-leaf cursor walked
        // the wrong way and blue inherited green's geometry.
        assert!(
            body.contains(r#"d="M 0 0 L 3 0 Z" fill="rgb(0,0,255)""#),
            "third fill leaf misaligned: {body}"
        );
    }
}
