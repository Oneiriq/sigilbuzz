//! COLRv1 -> SVG emission, gated on the `color` Cargo feature.
//!
//! This module drives the paint walk from `sigilbuzz-paint`
//! (`walk::paint_glyph`, HarfBuzz's callback order) and writes an SVG
//! fragment that mirrors the COLRv1 paint tree:
//!
//! - A fill paints its innermost clip: a `PaintGlyph` outline becomes
//!   `<path transform=... d=...>` with the transform in effect at the
//!   `PaintGlyph`, so a transform below the `PaintGlyph` moves only the
//!   paint. Enclosing clips (outer glyphs, ClipList boxes) become
//!   `<clipPath>` definitions applied by `<g clip-path=...>` wrappers.
//! - Solid fills are `fill="rgb(...)"`; linear and radial gradients land
//!   in a `<defs>` block, referenced via `fill="url(#grad-N)"`, with a
//!   `gradientTransform` carrying the paint's own transform, so they are
//!   exact under any transform. A linear gradient's rotation point
//!   `p2` is folded into its end point.
//! - Fills and gradient stops on COLR palette entry `0xFFFF` (the text
//!   color) use `currentColor`, with the paint alpha as `fill-opacity`
//!   or `stop-opacity`, so the glyph takes the color of the text it is
//!   embedded in.
//! - Sweep gradients have no SVG 1.1 equivalent. We degrade them to a
//!   linear gradient running across the gradient's center. The colors
//!   are right, the angular distribution is not. The output carries an
//!   `<!-- sweep-fallback -->` comment so consumers that care can detect
//!   the substitution and route through a richer renderer.
//! - Each `PaintComposite` is an isolated `<g style="isolation:isolate">`
//!   holding the backdrop and a `<g style="mix-blend-mode:...">` holding
//!   the source. SVG's blend modes only cover a subset of the COLRv1
//!   composite list, so Porter-Duff modes are written as `normal`.
//! - The `viewBox` is the glyph's clip box, as in HarfBuzz: its ClipList
//!   box (which also clips the drawing) or the bounds of its paint tree,
//!   plus [`VIEWBOX_MARGIN`]. A glyph whose paint escapes every clip is
//!   unbounded and produces no SVG.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write as _;

use sigilbuzz::tables::PathOp;
use sigilbuzz::Face;
use sigilbuzz_paint::walk::{paint_glyph, ColorLineRef, ColorRef, PaintSink, Resolver, RootClip};
use sigilbuzz_paint::{
    Color, ColorStop, CompositeMode, EvalOptions, Extend, GradientKind, Transform2D,
};

use crate::{path_data, push_num, F2Dot14, GlyphId, VIEWBOX_MARGIN};

/// Emits a complete `<svg>` document for `gid`'s COLRv1 color glyph.
///
/// Returns `None` when the glyph has no COLRv1 paint, when its paint is
/// unbounded (no clip or ClipList box encloses it), or when nothing it
/// paints has a visible shape (every clipping outline glyph is missing
/// or empty). In that case the caller can fall back to
/// [`crate::glyph_to_svg`] for a black outline rendering.
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
    let colr = face.colr().ok().flatten()?;
    colr.paint(gid)?;
    // An opaque foreground keeps foreground alphas equal to the paint
    // alpha, which is what `fill-opacity` / `stop-opacity` need next to
    // `currentColor`.
    let options = EvalOptions::new()
        .with_coords(coords)
        .with_foreground(Color::BLACK);
    let cpal = face.cpal().ok().flatten();
    let mut sink = SvgSink {
        face,
        coords,
        resolver: Resolver::new(cpal.as_ref(), &options),
        transforms: alloc::vec![Transform2D::IDENTITY],
        frames: Vec::new(),
        view: None,
        defs: Defs::default(),
        body: String::new(),
        painted: false,
    };
    paint_glyph(face, gid, coords, &mut sink);
    if !sink.painted {
        return None;
    }
    let view = sink.view?;
    Some(assemble_svg(view, &sink.defs.into_svg(), &sink.body))
}

// =========================================================================
// The sink
// =========================================================================

/// A clip shape: a glyph outline or a rectangle, each with the
/// transform in effect when it was pushed.
#[derive(Debug, Clone)]
enum Shape {
    Glyph(GlyphId, Transform2D),
    Rect([f32; 4], Transform2D),
}

/// One open push: a clip (written lazily as a `<g clip-path>` wrapper),
/// a group (written at once as `<g>`, styled when it closes), or a root
/// clip from computed bounds, which only sizes the viewBox.
#[derive(Debug, Clone)]
enum Frame {
    Clip { shape: Shape, written: bool },
    Group { at: usize },
    Bounds,
}

struct SvgSink<'f, 'a, 'c, 'r> {
    face: &'f Face<'a>,
    coords: &'c [f32],
    resolver: Resolver<'r, 'r>,
    /// Paint space to design units.
    transforms: Vec<Transform2D>,
    frames: Vec<Frame>,
    /// The root clip rectangle in design units, when bounded.
    view: Option<[f32; 4]>,
    defs: Defs,
    body: String,
    /// Whether any fill produced a shape.
    painted: bool,
}

impl SvgSink<'_, '_, '_, '_> {
    fn top(&self) -> Transform2D {
        self.transforms
            .last()
            .copied()
            .unwrap_or(Transform2D::IDENTITY)
    }

    /// Path data for a clip shape, or `None` for an empty glyph.
    fn shape_path(&self, shape: &Shape) -> Option<String> {
        match shape {
            Shape::Glyph(gid, _) => {
                let outline = self
                    .face
                    .glyph_outline_at_coords(*gid, self.coords)
                    .ok()
                    .flatten()?;
                (!outline.is_empty()).then(|| path_data(outline.ops()))
            }
            Shape::Rect([x0, y0, x1, y1], _) => Some(path_data(&[
                PathOp::MoveTo { x: *x0, y: *y0 },
                PathOp::LineTo { x: *x1, y: *y0 },
                PathOp::LineTo { x: *x1, y: *y1 },
                PathOp::LineTo { x: *x0, y: *y1 },
                PathOp::Close,
            ])),
        }
    }

    /// Opens `<g clip-path>` wrappers for every clip frame not written
    /// yet, below index `end`.
    fn write_clips(&mut self, end: usize) {
        for i in 0..end {
            let Frame::Clip {
                shape,
                written: false,
            } = &self.frames[i]
            else {
                continue;
            };
            let shape = shape.clone();
            let id = self.defs.allocate_id("clip");
            let mut def = format!(r#"<clipPath id="{id}"><path"#);
            if let Some(t) = transform_attr(shape_transform(&shape)) {
                let _ = write!(def, r#" transform="{t}""#);
            }
            let d = self.shape_path(&shape).unwrap_or_default();
            let _ = write!(def, r#" d="{d}"/></clipPath>"#);
            self.defs.push(def);
            let _ = write!(self.body, r#"<g clip-path="url(#{id})">"#);
            self.frames[i] = Frame::Clip {
                shape,
                written: true,
            };
        }
    }

    /// Paints the current clip with `fill`. A gradient's geometry stays
    /// in paint space; its `gradientTransform` maps it into the user
    /// space of the shape it fills.
    fn fill(&mut self, fill: Fill<'_>) {
        // The innermost clip, when it is still pending, is the shape;
        // everything else wraps it. Otherwise fill the whole view.
        let n = self.frames.len();
        let innermost = match self.frames.last() {
            Some(Frame::Clip {
                shape,
                written: false,
            }) => Some(shape.clone()),
            _ => None,
        };
        let shape = match innermost {
            Some(shape) => {
                self.write_clips(n - 1);
                shape
            }
            None => {
                self.write_clips(n);
                let Some(view) = self.view else {
                    return;
                };
                Shape::Rect(view, Transform2D::IDENTITY)
            }
        };
        let Some(d) = self.shape_path(&shape) else {
            return;
        };
        let shape_t = shape_transform(&shape);
        let paint = match fill {
            Fill::Solid(color, is_foreground) => {
                let mut s = format!(r#" fill="{}""#, fill_color(color, is_foreground));
                if color.a < 1.0 - 1e-6 {
                    let _ = write!(s, r#" fill-opacity="{}""#, fmt_num(color.a));
                }
                s
            }
            Fill::Gradient(line, kind) => {
                let to_shape = shape_t
                    .inverse()
                    .map_or(Transform2D::IDENTITY, |inv| self.top().then(inv));
                let stops = self.resolver.stops(line);
                let id = emit_gradient_def(&mut self.defs, kind, &stops, line.extend, to_shape);
                format!(r#" fill="url(#{id})""#)
            }
        };
        self.body.push_str("<path");
        if let Some(t) = transform_attr(shape_t) {
            let _ = write!(self.body, r#" transform="{t}""#);
        }
        let _ = write!(self.body, r#" d="{d}"{paint}/>"#);
        self.painted = true;
    }
}

/// What a fill paints.
enum Fill<'l> {
    Solid(Color, bool),
    Gradient(ColorLineRef<'l>, GradientKind),
}

fn shape_transform(shape: &Shape) -> Transform2D {
    match shape {
        Shape::Glyph(_, t) | Shape::Rect(_, t) => *t,
    }
}

impl PaintSink for SvgSink<'_, '_, '_, '_> {
    fn push_transform(&mut self, transform: Transform2D) {
        let t = transform.then(self.top());
        self.transforms.push(t);
    }

    // Design units are the output space: the root transform is the
    // identity.
    fn push_root_transform(&mut self) {
        self.transforms.push(self.top());
    }

    fn push_inverse_root_transform(&mut self) {
        self.transforms.push(self.top());
    }

    fn pop_transform(&mut self) {
        if self.transforms.len() > 1 {
            self.transforms.pop();
        }
    }

    fn push_clip_glyph(&mut self, glyph: GlyphId) {
        let shape = Shape::Glyph(glyph, self.top());
        self.frames.push(Frame::Clip {
            shape,
            written: false,
        });
    }

    fn push_clip_rectangle(&mut self, x_min: f32, y_min: f32, x_max: f32, y_max: f32) {
        let shape = Shape::Rect([x_min, y_min, x_max, y_max], self.top());
        self.frames.push(Frame::Clip {
            shape,
            written: false,
        });
    }

    fn push_root_clip(&mut self, clip: RootClip) {
        let (x0, y0, x1, y1) = clip.rect();
        if clip.is_bounded() && x0 < x1 && y0 < y1 {
            self.view = Some([x0, y0, x1, y1]);
        }
        // A ClipList box clips the drawing; computed bounds enclose all
        // of it already, so they only size the viewBox.
        let frame = match clip {
            RootClip::ClipBox { .. } => Frame::Clip {
                shape: Shape::Rect([x0, y0, x1, y1], Transform2D::IDENTITY),
                written: false,
            },
            RootClip::Extents { .. } => Frame::Bounds,
        };
        self.frames.push(frame);
    }

    fn pop_clip(&mut self) {
        if let Some(Frame::Clip { written: true, .. }) = self.frames.pop() {
            self.body.push_str("</g>");
        }
    }

    fn push_group(&mut self) {
        let n = self.frames.len();
        self.write_clips(n);
        self.frames.push(Frame::Group {
            at: self.body.len(),
        });
        self.body.push_str("<g>");
    }

    fn pop_group(&mut self, mode: CompositeMode) {
        let Some(Frame::Group { at }) = self.frames.pop() else {
            return;
        };
        let style = match mode {
            CompositeMode::SrcOver => String::from(r#" style="isolation:isolate""#),
            mode => format!(
                r#" style="mix-blend-mode:{}""#,
                composite_to_blend_mode(mode)
            ),
        };
        self.body.insert_str(at + 2, &style);
        self.body.push_str("</g>");
    }

    fn color(&mut self, color: ColorRef) {
        let (color, is_foreground) = self.resolver.color(color);
        self.fill(Fill::Solid(color, is_foreground));
    }

    fn linear_gradient(
        &mut self,
        line: ColorLineRef<'_>,
        p0: (f32, f32),
        p1: (f32, f32),
        p2: (f32, f32),
    ) {
        self.fill(Fill::Gradient(line, GradientKind::Linear { p0, p1, p2 }));
    }

    fn radial_gradient(
        &mut self,
        line: ColorLineRef<'_>,
        c0: (f32, f32),
        r0: f32,
        c1: (f32, f32),
        r1: f32,
    ) {
        self.fill(Fill::Gradient(
            line,
            GradientKind::Radial { c0, r0, c1, r1 },
        ));
    }

    fn sweep_gradient(
        &mut self,
        line: ColorLineRef<'_>,
        center: (f32, f32),
        start_angle: f32,
        end_angle: f32,
    ) {
        self.fill(Fill::Gradient(
            line,
            GradientKind::Sweep {
                center,
                start_angle,
                end_angle,
            },
        ));
    }
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

// =========================================================================
// Gradient defs
// =========================================================================

/// Writes the gradient definition and returns its id. `to_shape` maps
/// the gradient's paint space into the user space of the filled shape.
fn emit_gradient_def(
    defs: &mut Defs,
    kind: GradientKind,
    stops: &[ColorStop],
    extend: Extend,
    to_shape: Transform2D,
) -> String {
    let id = defs.allocate_id("grad");
    let spread = spread_method(extend);
    let transform = transform_attr(to_shape)
        .map(|t| format!(r#" gradientTransform="{t}""#))
        .unwrap_or_default();
    let mut s = String::new();
    let tag = match kind {
        GradientKind::Linear { p0, p1, p2 } => {
            let (a, b) = reduce_linear_anchors(p0, p1, p2);
            let _ = write!(
                s,
                r#"<linearGradient id="{id}" gradientUnits="userSpaceOnUse" x1="{}" y1="{}" x2="{}" y2="{}" spreadMethod="{spread}"{transform}>"#,
                fmt_num(a.0),
                fmt_num(a.1),
                fmt_num(b.0),
                fmt_num(b.1),
            );
            "linearGradient"
        }
        GradientKind::Radial { c0, r0, c1, r1 } => {
            // SVG `radialGradient` lays out as (cx, cy) outer, (fx, fy)
            // inner. The inner radius `r0` is SVG2's `fr`; some viewers
            // respect it, others ignore it. Either way both circles are
            // written so the data is preserved.
            let _ = write!(
                s,
                r#"<radialGradient id="{id}" gradientUnits="userSpaceOnUse" cx="{}" cy="{}" r="{}" fx="{}" fy="{}" fr="{}" spreadMethod="{spread}"{transform}>"#,
                fmt_num(c1.0),
                fmt_num(c1.1),
                fmt_num(r1),
                fmt_num(c0.0),
                fmt_num(c0.1),
                fmt_num(r0),
            );
            "radialGradient"
        }
        GradientKind::Sweep {
            center,
            start_angle,
            end_angle,
        } => {
            // SVG 1.1 has no sweep gradient. We approximate with a
            // linear gradient running through the sweep's center,
            // oriented along the sector's bisector. The colors are
            // preserved; the angular distribution is not. A
            // `<!-- sweep-fallback -->` marker lets consumers detect
            // and re-route.
            let bisector = 0.5 * (start_angle + end_angle);
            let (dx, dy) = (bisector.cos(), bisector.sin());
            s.push_str("<!-- sweep-fallback -->");
            let _ = write!(
                s,
                r#"<linearGradient id="{id}" gradientUnits="userSpaceOnUse" x1="{}" y1="{}" x2="{}" y2="{}" spreadMethod="{spread}"{transform}>"#,
                fmt_num(center.0 - dx),
                fmt_num(center.1 - dy),
                fmt_num(center.0 + dx),
                fmt_num(center.1 + dy),
            );
            "linearGradient"
        }
    };
    for stop in stops {
        s.push_str(&stop_tag(stop));
    }
    let _ = write!(s, "</{tag}>");
    defs.push(s);
    id
}

/// Folds a COLRv1 linear gradient's rotation point `p2` into its end
/// point: color lines run parallel to `p0 p2`, so the gradient runs
/// from `p0` to `p1` projected onto the normal of `p0 p2`. With `p2` on
/// `p0` the gradient is plain `p0 -> p1`.
fn reduce_linear_anchors(
    p0: (f32, f32),
    p1: (f32, f32),
    p2: (f32, f32),
) -> ((f32, f32), (f32, f32)) {
    let (q1x, q1y) = (p1.0 - p0.0, p1.1 - p0.1);
    let (q2x, q2y) = (p2.0 - p0.0, p2.1 - p0.1);
    let s = q2x * q2x + q2y * q2y;
    if s < 0.000_001 {
        return (p0, p1);
    }
    let k = (q2x * q1x + q2y * q1y) / s;
    (p0, (p1.0 - k * q2x, p1.1 - k * q2y))
}

fn spread_method(extend: Extend) -> &'static str {
    match extend {
        Extend::Pad => "pad",
        Extend::Repeat => "repeat",
        Extend::Reflect => "reflect",
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
    // fidelity should use a Porter-Duff-aware backend such as
    // sigilbuzz-render.
    match mode {
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
        _ => "normal",
    }
}

// =========================================================================
// Outer SVG framing
// =========================================================================

/// Frames `body` in an `<svg>` whose viewBox is `view` (design units,
/// `[x_min, y_min, x_max, y_max]`) plus the margin, flipped so y points
/// up as in the font.
fn assemble_svg(view: [f32; 4], defs: &str, body: &str) -> String {
    let [min_x, min_y, max_x, max_y] = view;
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

    fn red_blue_green() -> Vec<ColorStop> {
        alloc::vec![
            ColorStop::new(0.0, Color::new(1.0, 0.0, 0.0, 1.0)),
            ColorStop::new(0.5, Color::new(0.0, 1.0, 0.0, 1.0)),
            ColorStop::new(1.0, Color::new(0.0, 0.0, 1.0, 1.0)),
        ]
    }

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
        let kind = GradientKind::Linear {
            p0: (0.0, 0.0),
            p1: (100.0, 0.0),
            p2: (0.0, 100.0),
        };
        let mut defs = Defs::default();
        let id = emit_gradient_def(
            &mut defs,
            kind,
            &red_blue_green(),
            Extend::Pad,
            Transform2D::IDENTITY,
        );
        let svg = defs.into_svg();
        assert!(svg.contains("<linearGradient"), "missing tag: {svg}");
        assert_eq!(svg.matches("<stop ").count(), 3);
        assert!(svg.contains(&format!(r#"id="{id}""#)));
        assert!(svg.contains(r#"x1="0""#));
        assert!(svg.contains(r#"x2="100""#));
        assert!(svg.contains(r#"spreadMethod="pad""#));
        assert!(svg.contains(r#"stop-color="rgb(255,0,0)""#));
        assert!(!svg.contains("gradientTransform"), "{svg}");
    }

    #[test]
    fn linear_gradient_folds_in_the_rotation_point() {
        // Color lines parallel to the diagonal p0 p2: the gradient runs
        // from p0 toward (50, -50).
        let kind = GradientKind::Linear {
            p0: (0.0, 0.0),
            p1: (100.0, 0.0),
            p2: (100.0, 100.0),
        };
        let mut defs = Defs::default();
        emit_gradient_def(
            &mut defs,
            kind,
            &red_blue_green(),
            Extend::Pad,
            Transform2D::IDENTITY,
        );
        let svg = defs.into_svg();
        assert!(svg.contains(r#"x1="0" y1="0" x2="50" y2="-50""#), "{svg}");
    }

    #[test]
    fn gradients_carry_the_paint_transform() {
        let kind = GradientKind::Radial {
            c0: (10.0, 20.0),
            r0: 5.0,
            c1: (10.0, 20.0),
            r1: 50.0,
        };
        let mut defs = Defs::default();
        emit_gradient_def(
            &mut defs,
            kind,
            &red_blue_green(),
            Extend::Reflect,
            Transform2D::scale(2.0, 1.0),
        );
        let svg = defs.into_svg();
        assert!(svg.contains("<radialGradient"));
        assert!(svg.contains(r#"r="50""#));
        assert!(svg.contains(r#"fr="5""#));
        assert!(svg.contains(r#"spreadMethod="reflect""#));
        // The scale stays on the gradient, so the circle becomes an
        // exact ellipse.
        assert!(
            svg.contains(r#"gradientTransform="matrix(2 0 0 1 0 0)""#),
            "{svg}"
        );
    }

    #[test]
    fn sweep_gradient_falls_back_to_linear_with_marker() {
        let kind = GradientKind::Sweep {
            center: (0.0, 0.0),
            start_angle: 0.0,
            end_angle: core::f32::consts::PI,
        };
        let mut defs = Defs::default();
        emit_gradient_def(
            &mut defs,
            kind,
            &red_blue_green(),
            Extend::Pad,
            Transform2D::IDENTITY,
        );
        let svg = defs.into_svg();
        // SVG 1.1 has no sweep. We emit a linearGradient and prefix
        // it with a sweep-fallback comment so consumers can detect.
        assert!(svg.contains("<!-- sweep-fallback -->"));
        assert!(svg.contains("<linearGradient"));
        assert!(!svg.contains("<radialGradient"));
    }

    #[test]
    fn foreground_stops_use_current_color() {
        let stops = [
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
        ];
        let tags: Vec<String> = stops.iter().map(stop_tag).collect();
        assert_eq!(tags[0], r#"<stop offset="0" stop-color="rgb(255,0,0)"/>"#);
        assert_eq!(
            tags[1],
            r#"<stop offset="1" stop-color="currentColor" stop-opacity="0.25"/>"#
        );
        assert_eq!(tags[2], r#"<stop offset="1" stop-color="currentColor"/>"#);
    }

    #[test]
    fn porter_duff_modes_blend_as_normal() {
        assert_eq!(composite_to_blend_mode(CompositeMode::Multiply), "multiply");
        assert_eq!(composite_to_blend_mode(CompositeMode::DestIn), "normal");
    }
}
