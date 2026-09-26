//! Flattening evaluator.
//!
//! [`evaluate`] walks the COLRv1 paint DAG rooted at a base glyph and
//! returns a flat [`DrawCmd`] sequence. It drives the same walk as
//! [`crate::walk::paint_glyph_unclipped`], so both see identical
//! structure, variation deltas, and cycle handling, and folds each step
//! into the flat stream:
//!
//! - transforms compose into one 2x3 matrix per fill,
//! - a fill takes the innermost enclosing `PaintGlyph` as its outline,
//!   with that glyph's transform; a transform below the `PaintGlyph`
//!   moves only the paint, so its gradient geometry is carried into the
//!   outline's space,
//! - palette entries resolve to colors,
//! - each `PaintComposite` becomes an isolated layer holding the
//!   backdrop and a nested layer holding the source.
//!
//! Clip rectangles (ClipList boxes) have no [`DrawCmd`] and are dropped.
//!
//! Determinism: the output for a given (face, gid, options) tuple is
//! byte-for-byte stable. The walk is depth-first, backdrop before
//! source, and never reorders.
//!
//! Robustness: malformed offsets, unknown formats, and reference
//! cycles all truncate the output. The walker never panics.

use alloc::vec::Vec;

use sigilbuzz::tables::colr::CompositeMode;
use sigilbuzz::Face;

use crate::color::Color;
use crate::gradient::{Gradient, GradientKind};
use crate::options::EvalOptions;
use crate::transform::Transform2D;
use crate::walk::{self, ColorLineRef, ColorRef, PaintSink, Resolver, RootClip};

/// Glyph-id alias. Mirrors the on-disk u16 used throughout sigilbuzz.
pub type GlyphId = u16;

/// One unit of work emitted by the evaluator.
///
/// A renderer drives compositing by holding a layer stack: every
/// [`DrawCmd::PushLayer`] starts a new fresh layer, the contents up to
/// the matching [`DrawCmd::PopLayer`] are then blended back into the
/// surface below using the recorded [`CompositeMode`].
///
/// A `PaintComposite` arrives as `PushLayer { SrcOver }`, the backdrop,
/// `PushLayer { mode }`, the source, `PopLayer`, `PopLayer`: the outer
/// layer isolates the composite from whatever was drawn before it.
#[derive(Debug, Clone)]
pub enum DrawCmd {
    /// Fill an outline glyph with a paint source.
    FillGlyph {
        /// Outline glyph to fill: the innermost `PaintGlyph` enclosing
        /// the paint, or 0 for a paint no `PaintGlyph` encloses.
        gid: GlyphId,
        /// Transform from the outline's design units to the glyph's,
        /// accumulated from the root to the `PaintGlyph`. Gradient
        /// geometry in `paint` is in the same space as the outline.
        transform: Transform2D,
        /// What to fill the outline with.
        paint: PaintSource,
    },
    /// Begin a layer, used to wrap a composite group. The matching
    /// [`DrawCmd::PopLayer`] applies `composite_mode` against the
    /// surface below.
    PushLayer {
        /// Composite mode used on the matching `PopLayer`.
        composite_mode: CompositeMode,
    },
    /// Finish the most recent layer, blending it into the surface.
    PopLayer,
}

/// Resolved paint source attached to a [`DrawCmd::FillGlyph`].
#[derive(Debug, Clone)]
pub enum PaintSource {
    /// Solid RGBA fill.
    Solid {
        /// Resolved color with the paint alpha folded in. For a
        /// foreground fill this is the evaluation's foreground color
        /// (see [`EvalOptions::with_foreground`]) with the paint alpha
        /// applied.
        color: Color,
        /// True when the paint used COLR palette entry `0xFFFF`, the
        /// foreground (text) color. A renderer with its own text color
        /// can substitute it here, keeping `color.a` relative to the
        /// evaluation foreground's alpha.
        is_foreground: bool,
    },
    /// Resolved gradient: palette indices already substituted for
    /// f32 RGBA, alpha multiplied in, geometry in the space of the
    /// fill's outline.
    Gradient(Gradient),
}

/// Walks `face`'s COLRv1 paint tree for `gid`, returning the draw
/// commands required to render the color glyph. Equivalent to
/// [`evaluate_with`] with [`EvalOptions::default`].
#[must_use]
pub fn evaluate(face: &Face<'_>, gid: GlyphId) -> Vec<DrawCmd> {
    evaluate_with(face, gid, &EvalOptions::new())
}

/// Same as [`evaluate`] but applies variation deltas from `coords` to
/// every `PaintVar*` node visited. `coords` is the normalized axis
/// vector, the same shape sigilbuzz's
/// [`Face::glyph_outline_at_coords`](sigilbuzz::Face::glyph_outline_at_coords)
/// accepts. An empty slice is the static (no-deltas) path.
#[must_use]
pub fn evaluate_at_coords(face: &Face<'_>, gid: GlyphId, coords: &[f32]) -> Vec<DrawCmd> {
    evaluate_with(face, gid, &EvalOptions::new().with_coords(coords))
}

/// Walks `face`'s COLRv1 paint tree for `gid` with explicit
/// [`EvalOptions`]: variation coordinates, CPAL palette, and the
/// foreground color for palette entry `0xFFFF`.
///
/// Variation deltas come from the COLR table's own item variation store,
/// through its DeltaSetIndexMap when it has one, as in HarfBuzz.
///
/// ```
/// use sigilbuzz::Face;
/// use sigilbuzz_paint::{evaluate_with, Color, EvalOptions};
///
/// // A face without a COLR table has nothing to paint.
/// let sfnt = [0u8, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
/// let face = Face::parse_bytes(&sfnt, 0).unwrap();
/// let black = Color::new(0.0, 0.0, 0.0, 1.0);
/// let options = EvalOptions::new().with_palette_index(1).with_foreground(black);
/// assert!(evaluate_with(&face, 42, &options).is_empty());
/// ```
#[must_use]
pub fn evaluate_with(face: &Face<'_>, gid: GlyphId, options: &EvalOptions<'_>) -> Vec<DrawCmd> {
    let mut out = Vec::new();
    let Ok(Some(colr)) = face.colr() else {
        return out;
    };
    if colr.paint(gid).is_none() {
        // COLRv0 layers are not part of the flattened stream.
        return out;
    }
    let cpal = face.cpal().ok().flatten();
    let mut sink = DrawSink {
        resolver: Resolver::new(cpal.as_ref(), options),
        transforms: alloc::vec![Transform2D::IDENTITY],
        clips: Vec::new(),
        groups: Vec::new(),
        out: &mut out,
    };
    walk::paint_glyph_unclipped(face, gid, options.coords(), &mut sink);
    out
}

/// Folds walk steps into [`DrawCmd`]s.
struct DrawSink<'a, 'b, 'o> {
    resolver: Resolver<'a, 'b>,
    /// Accumulated transforms, paint space to glyph design units.
    transforms: Vec<Transform2D>,
    /// Enclosing clips: a glyph with its transform, or a rectangle.
    clips: Vec<Option<(GlyphId, Transform2D)>>,
    /// Indices of the open groups' `PushLayer` commands.
    groups: Vec<usize>,
    out: &'o mut Vec<DrawCmd>,
}

impl DrawSink<'_, '_, '_> {
    fn top(&self) -> Transform2D {
        self.transforms
            .last()
            .copied()
            .unwrap_or(Transform2D::IDENTITY)
    }

    /// The outline to fill, its transform, and the transform that
    /// carries paint geometry into the outline's space (`None` when the
    /// two spaces coincide).
    fn target(&self) -> (GlyphId, Transform2D, Option<Transform2D>) {
        let top = self.top();
        match self.clips.iter().rev().find_map(|c| *c) {
            Some((gid, clip)) if clip == top => (gid, clip, None),
            Some((gid, clip)) => match clip.inverse() {
                Some(inverse) => (gid, clip, Some(top.then(inverse))),
                None => (gid, top, None),
            },
            None => (0, top, None),
        }
    }

    fn fill(&mut self, paint: PaintSource) {
        let (gid, transform, _) = self.target();
        self.out.push(DrawCmd::FillGlyph {
            gid,
            transform,
            paint,
        });
    }

    fn gradient(&mut self, line: ColorLineRef<'_>, kind: GradientKind) {
        let (_, _, paint_to_outline) = self.target();
        let kind = paint_to_outline.map_or(kind, |m| map_gradient(kind, m));
        let gradient = Gradient {
            kind,
            stops: self.resolver.stops(line),
            extend: line.extend,
        };
        self.fill(PaintSource::Gradient(gradient));
    }
}

/// Carries gradient geometry through `m`. Exact for linear gradients
/// under any affine map, and for radial and sweep gradients under a
/// similarity (rotation, uniform scale, translation). A radial or sweep
/// gradient under any other map cannot be expressed in [`GradientKind`];
/// its radii scale by the square root of the area scale and its angles
/// turn with the x axis.
fn map_gradient(kind: GradientKind, m: Transform2D) -> GradientKind {
    let pt = |p: (f32, f32)| m.apply(p.0, p.1);
    match kind {
        GradientKind::Linear { p0, p1, p2 } => GradientKind::Linear {
            p0: pt(p0),
            p1: pt(p1),
            p2: pt(p2),
        },
        GradientKind::Radial { c0, r0, c1, r1 } => {
            let s = (m.xx * m.yy - m.xy * m.yx).abs().sqrt();
            GradientKind::Radial {
                c0: pt(c0),
                r0: r0 * s,
                c1: pt(c1),
                r1: r1 * s,
            }
        }
        GradientKind::Sweep {
            center,
            start_angle,
            end_angle,
        } => {
            let turn = m.yx.atan2(m.xx);
            GradientKind::Sweep {
                center: pt(center),
                start_angle: start_angle + turn,
                end_angle: end_angle + turn,
            }
        }
    }
}

impl PaintSink for DrawSink<'_, '_, '_> {
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
        let top = self.top();
        self.clips.push(Some((glyph, top)));
    }

    fn push_clip_rectangle(&mut self, _x_min: f32, _y_min: f32, _x_max: f32, _y_max: f32) {
        self.clips.push(None);
    }

    fn push_root_clip(&mut self, _clip: RootClip) {
        self.clips.push(None);
    }

    fn pop_clip(&mut self) {
        self.clips.pop();
    }

    fn push_group(&mut self) {
        self.groups.push(self.out.len());
        self.out.push(DrawCmd::PushLayer {
            composite_mode: CompositeMode::SrcOver,
        });
    }

    fn pop_group(&mut self, mode: CompositeMode) {
        let Some(at) = self.groups.pop() else {
            return;
        };
        if let Some(cmd) = self.out.get_mut(at) {
            *cmd = DrawCmd::PushLayer {
                composite_mode: mode,
            };
        }
        self.out.push(DrawCmd::PopLayer);
    }

    fn color(&mut self, color: ColorRef) {
        let (color, is_foreground) = self.resolver.color(color);
        self.fill(PaintSource::Solid {
            color,
            is_foreground,
        });
    }

    fn linear_gradient(
        &mut self,
        line: ColorLineRef<'_>,
        p0: (f32, f32),
        p1: (f32, f32),
        p2: (f32, f32),
    ) {
        self.gradient(line, GradientKind::Linear { p0, p1, p2 });
    }

    fn radial_gradient(
        &mut self,
        line: ColorLineRef<'_>,
        c0: (f32, f32),
        r0: f32,
        c1: (f32, f32),
        r1: f32,
    ) {
        self.gradient(line, GradientKind::Radial { c0, r0, c1, r1 });
    }

    fn sweep_gradient(
        &mut self,
        line: ColorLineRef<'_>,
        center: (f32, f32),
        start_angle: f32,
        end_angle: f32,
    ) {
        self.gradient(
            line,
            GradientKind::Sweep {
                center,
                start_angle,
                end_angle,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_geometry_maps_exactly() {
        let m = Transform2D {
            xx: 2.0,
            yx: 0.0,
            xy: 1.0,
            yy: 3.0,
            dx: 5.0,
            dy: -1.0,
        };
        let kind = GradientKind::Linear {
            p0: (0.0, 0.0),
            p1: (10.0, 0.0),
            p2: (0.0, 10.0),
        };
        assert_eq!(
            map_gradient(kind, m),
            GradientKind::Linear {
                p0: (5.0, -1.0),
                p1: (25.0, -1.0),
                p2: (15.0, 29.0),
            }
        );
    }

    #[test]
    fn radial_and_sweep_follow_a_similarity() {
        // Rotate a quarter turn and double.
        let m = Transform2D {
            xx: 0.0,
            yx: 2.0,
            xy: -2.0,
            yy: 0.0,
            dx: 0.0,
            dy: 0.0,
        };
        let radial = GradientKind::Radial {
            c0: (1.0, 0.0),
            r0: 1.0,
            c1: (1.0, 0.0),
            r1: 3.0,
        };
        assert_eq!(
            map_gradient(radial, m),
            GradientKind::Radial {
                c0: (0.0, 2.0),
                r0: 2.0,
                c1: (0.0, 2.0),
                r1: 6.0,
            }
        );
        let sweep = GradientKind::Sweep {
            center: (1.0, 1.0),
            start_angle: 0.0,
            end_angle: 1.0,
        };
        let GradientKind::Sweep {
            center,
            start_angle,
            end_angle,
        } = map_gradient(sweep, m)
        else {
            panic!("sweep stays a sweep");
        };
        let quarter = core::f32::consts::FRAC_PI_2;
        assert_eq!(center, (-2.0, 2.0));
        assert!((start_angle - quarter).abs() < 1e-6);
        assert!((end_angle - (1.0 + quarter)).abs() < 1e-6);
    }
}
