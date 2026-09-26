//! Paint sink that rasterizes a COLRv1 glyph.
//!
//! [`RasterSink`] receives the walk from `sigilbuzz_paint::walk` in
//! HarfBuzz's order and draws it the way a 2D graphics library (cairo,
//! Skia) draws HarfBuzz's paint callbacks:
//!
//! - The root clip picks the canvas: the glyph's clip box in pixels,
//!   rounded out to whole pixels, plus a one-pixel transparent margin.
//!   An unbounded glyph gets no canvas and renders empty.
//! - A stack of transforms maps paint coordinates to design units; one
//!   device transform maps design units to pixels (y down).
//! - A stack of coverage masks, one per clip: a glyph outline or a
//!   rectangle, rasterized with anti-aliasing under the transform in
//!   effect when it was pushed and multiplied into the enclosing clip.
//!   A transform pushed after the clip moves only what is painted
//!   inside it.
//! - A stack of premultiplied RGBA layers: `push_group` starts a
//!   transparent layer and `pop_group` composites it onto the layer
//!   below with the group's mode, so composites combine isolated
//!   source and backdrop groups.
//! - A fill paints the current clip on the top layer, source-over.
//!   Gradients are sampled by mapping each pixel center back into paint
//!   space, so they are exact under any transform.
//!
//! Hostile paint graphs are bounded three ways. Every clip outline
//! draws from one shared segment budget. Every canvas-sized pass (a
//! clip mask, a fill, a group, or a group composite) draws its pixel
//! count from a work budget. The live clip masks and layers share a
//! memory budget. Running out of work or memory fails the render.

use alloc::vec::Vec;

use sigilbuzz::tables::PathOp;
use sigilbuzz::Face;
use sigilbuzz_paint::walk::{ColorLineRef, ColorRef, PaintSink, Resolver, RootClip};
use sigilbuzz_paint::{CompositeMode, Extend, GradientKind, Transform2D};

use crate::affine::Affine;
use crate::colrv1::{composite_layer, mul_alpha, sample_gradient, to_premul};
use crate::flatten::{flatten_limited, MAX_SEGMENTS};
use crate::pixmap::{ColorPixmap, Pixmap};
use crate::raster::{rasterize_in, Window};

/// Largest canvas side, the same ceiling the SVG and bitmap paths use.
const MAX_SIDE: f32 = 16384.0;

/// Pixel updates allowed while rendering one glyph. A clip mask, a
/// fill, a group, and a group composite each cost the canvas's pixel
/// count. The paint walk visits up to 65536 paints and a font's clip
/// box can stretch the canvas to 16384 pixels on a side, so without
/// this a hostile paint graph could demand trillions of updates. Real
/// color glyphs need a few hundred passes, far below it at any size
/// short of the largest canvases.
const MAX_CANVAS_WORK: u64 = 1 << 34;

/// Bytes the live clip masks and layers may hold at once: one per mask
/// pixel and four per layer pixel. Each nested clip and group holds a
/// canvas-sized buffer, so deep nesting on a large canvas would
/// otherwise allocate without bound. Past it the render fails.
const MAX_LIVE_BYTES: u64 = 1 << 32;

/// Where the canvas sits in pixel space.
#[derive(Debug, Clone, Copy)]
struct Canvas {
    width: u32,
    height: u32,
    /// Pixel-space coordinates of the canvas's top-left pixel.
    origin_x: i32,
    origin_y: i32,
}

/// Rasterizes one COLRv1 glyph walk.
pub(crate) struct RasterSink<'f, 'a, 'c, 'r> {
    face: &'f Face<'a>,
    coords: &'c [f32],
    resolver: Resolver<'r, 'r>,
    /// Pixels per design unit.
    scale: f32,
    tolerance: f32,
    canvas: Option<Canvas>,
    /// Set when the root clip is too large to allocate, the live masks
    /// and layers would exceed [`MAX_LIVE_BYTES`], or the drawing would
    /// exceed [`MAX_CANVAS_WORK`].
    oversized: bool,
    /// Flattened segments the clip outlines may still produce.
    segments_left: usize,
    /// Pixel updates left, see [`MAX_CANVAS_WORK`].
    work_left: u64,
    /// Groups whose layer was not pushed, so their pops are skipped.
    skipped_groups: u32,
    /// Paint space to design units.
    transforms: Vec<Transform2D>,
    /// Effective clip coverage per pushed clip, canvas-sized.
    clips: Vec<Pixmap>,
    /// Layer stack; the bottom layer is the output.
    layers: Vec<ColorPixmap>,
}

impl<'f, 'a, 'c, 'r> RasterSink<'f, 'a, 'c, 'r> {
    pub(crate) fn new(
        face: &'f Face<'a>,
        coords: &'c [f32],
        resolver: Resolver<'r, 'r>,
        scale: f32,
        tolerance: f32,
    ) -> Self {
        Self {
            face,
            coords,
            resolver,
            scale,
            tolerance,
            canvas: None,
            oversized: false,
            segments_left: MAX_SEGMENTS,
            work_left: MAX_CANVAS_WORK,
            skipped_groups: 0,
            transforms: alloc::vec![Transform2D::IDENTITY],
            clips: Vec::new(),
            layers: Vec::new(),
        }
    }

    /// The rendered glyph, or `None` when the root clip was too large.
    pub(crate) fn finish(mut self) -> Option<ColorPixmap> {
        if self.oversized {
            return None;
        }
        Some(if self.layers.is_empty() {
            ColorPixmap::new(0, 0)
        } else {
            self.layers.swap_remove(0)
        })
    }

    fn top(&self) -> Transform2D {
        self.transforms
            .last()
            .copied()
            .unwrap_or(Transform2D::IDENTITY)
    }

    /// Design units to canvas pixels: y flips so rows run down.
    fn device(&self, canvas: Canvas) -> Transform2D {
        Transform2D {
            xx: self.scale,
            yx: 0.0,
            xy: 0.0,
            yy: -self.scale,
            dx: -canvas.origin_x as f32,
            dy: -canvas.origin_y as f32,
        }
    }

    /// Paint space to canvas pixels.
    fn to_canvas(&self, canvas: Canvas) -> Transform2D {
        self.top().then(self.device(canvas))
    }

    /// Charges one canvas-sized pass to the work budget. False, with
    /// the render marked oversized, once the budget cannot cover it.
    fn take_pass(&mut self) -> bool {
        if self.oversized {
            return false;
        }
        let pixels = self
            .canvas
            .map_or(1, |c| u64::from(c.width) * u64::from(c.height));
        match self.work_left.checked_sub(pixels) {
            Some(rest) => {
                self.work_left = rest;
                true
            }
            None => {
                self.oversized = true;
                false
            }
        }
    }

    /// Checks that `bytes` more fit in [`MAX_LIVE_BYTES`] next to the
    /// live masks and layers. Marks the render oversized when not.
    fn reserve(&mut self, bytes: u64) -> bool {
        if self.oversized {
            return false;
        }
        let live: u64 = self
            .clips
            .iter()
            .map(|m| m.data.len() as u64)
            .chain(self.layers.iter().map(|l| l.data.len() as u64))
            .sum();
        if live.saturating_add(bytes) > MAX_LIVE_BYTES {
            self.oversized = true;
            return false;
        }
        true
    }

    /// Coverage of `ops` (in paint space) on the canvas, or `None` when
    /// the pass or memory budget is spent.
    fn coverage(&mut self, canvas: Canvas, ops: &[PathOp]) -> Option<Pixmap> {
        if !self.take_pass() || !self.reserve(u64::from(canvas.width) * u64::from(canvas.height)) {
            return None;
        }
        let t = self.to_canvas(canvas);
        let affine = Affine {
            xx: t.xx,
            yx: t.yx,
            xy: t.xy,
            yy: t.yy,
            dx: t.dx,
            dy: t.dy,
        };
        let mut mask = Pixmap::new(canvas.width, canvas.height);
        // Every clip outline shares one segment budget, so a paint
        // graph that repeats a heavy outline stays bounded. Outlines
        // past the budget cover nothing.
        if self.segments_left == 0 {
            return Some(mask);
        }
        let segments = flatten_limited(
            ops.iter().copied(),
            &affine,
            self.tolerance,
            self.segments_left,
        );
        self.segments_left = self.segments_left.saturating_sub(segments.len());
        if segments.is_empty() {
            return Some(mask);
        }
        // Rasterize only the canvas window: a shape far larger than
        // the canvas stays cheap and still covers it.
        let window = Window {
            x0: 0,
            y0: 0,
            x1: i32::try_from(canvas.width).unwrap_or(i32::MAX),
            y1: i32::try_from(canvas.height).unwrap_or(i32::MAX),
        };
        let r = rasterize_in(&segments, Some(window));
        for y in 0..r.pixmap.height {
            let Ok(cy) = u32::try_from(i64::from(r.origin_y) + i64::from(y)) else {
                continue;
            };
            for x in 0..r.pixmap.width {
                if let Ok(cx) = u32::try_from(i64::from(r.origin_x) + i64::from(x)) {
                    mask.set(cx, cy, r.pixmap.get(x, y));
                }
            }
        }
        Some(mask)
    }

    /// Pushes `shape` intersected with the enclosing clip. `None` (a
    /// spent budget) and an enclosing clip of another size push an
    /// empty clip, which paints nothing.
    fn push_mask(&mut self, shape: Option<Pixmap>) {
        let Some(mut shape) = shape else {
            self.clips.push(Pixmap::new(0, 0));
            return;
        };
        if let Some(parent) = self.clips.last() {
            if parent.data.len() != shape.data.len() {
                self.clips.push(Pixmap::new(0, 0));
                return;
            }
            for (s, p) in shape.data.iter_mut().zip(&parent.data) {
                *s = ((u32::from(*s) * u32::from(*p) + 127) / 255) as u8;
            }
        }
        self.clips.push(shape);
    }

    /// Fills the current clip on the top layer with `color_at(x, y)`,
    /// the premultiplied color at canvas pixel center `(x, y)`.
    fn fill(&mut self, color_at: impl Fn(f32, f32) -> [u8; 4]) {
        if self.clips.last().map_or(true, Pixmap::is_empty) || !self.take_pass() {
            return;
        }
        let (Some(clip), Some(layer)) = (self.clips.last(), self.layers.last_mut()) else {
            return;
        };
        let width = layer.width as usize;
        if width == 0 {
            return;
        }
        let pixels = clip.data.iter().zip(layer.data.chunks_exact_mut(4));
        for (i, (&coverage, dst)) in pixels.enumerate() {
            if coverage == 0 {
                continue;
            }
            let (x, y) = (i % width, i / width);
            let src = mul_alpha(color_at(x as f32 + 0.5, y as f32 + 0.5), coverage);
            blend_src_over(dst, src);
        }
    }

    fn gradient(&mut self, line: ColorLineRef<'_>, kind: GradientKind) {
        let Some(canvas) = self.canvas else {
            return;
        };
        let Some(to_paint) = self.to_canvas(canvas).inverse() else {
            return;
        };
        let stops = self.resolver.stops(line);
        let extend: Extend = line.extend;
        self.fill(|x, y| {
            let p = to_paint.apply(x, y);
            sample_gradient(kind, &stops, extend, p)
        });
    }
}

/// `dst = src OVER dst` on one premultiplied pixel.
fn blend_src_over(dst: &mut [u8], src: [u8; 4]) {
    if src[3] == 0 {
        return;
    }
    let inv = 255 - u32::from(src[3]);
    for (d, s) in dst.iter_mut().zip(src) {
        *d = (u32::from(s) + (u32::from(*d) * inv + 127) / 255) as u8;
    }
}

impl PaintSink for RasterSink<'_, '_, '_, '_> {
    fn push_transform(&mut self, transform: Transform2D) {
        let t = transform.then(self.top());
        self.transforms.push(t);
    }

    // The sink works in design units: the root transform is the
    // identity and the device transform is applied at rasterization.
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

    fn push_clip_glyph(&mut self, glyph: u16) {
        let Some(canvas) = self.canvas else {
            self.clips.push(Pixmap::new(0, 0));
            return;
        };
        let outline = self
            .face
            .glyph_outline_at_coords(glyph, self.coords)
            .ok()
            .flatten()
            .unwrap_or_default();
        let shape = self.coverage(canvas, outline.ops());
        self.push_mask(shape);
    }

    fn push_clip_rectangle(&mut self, x_min: f32, y_min: f32, x_max: f32, y_max: f32) {
        let Some(canvas) = self.canvas else {
            self.clips.push(Pixmap::new(0, 0));
            return;
        };
        let ops = [
            PathOp::MoveTo { x: x_min, y: y_min },
            PathOp::LineTo { x: x_max, y: y_min },
            PathOp::LineTo { x: x_max, y: y_max },
            PathOp::LineTo { x: x_min, y: y_max },
            PathOp::Close,
        ];
        let shape = self.coverage(canvas, &ops);
        self.push_mask(shape);
    }

    fn push_root_clip(&mut self, clip: RootClip) {
        let (x0, y0, x1, y1) = clip.rect();
        let s = self.scale;
        let (left, right) = (x0 * s, x1 * s);
        let (top, bottom) = (-y1 * s, -y0 * s);
        let bounded = clip.is_bounded() && left < right && top < bottom;
        if !bounded {
            self.clips.push(Pixmap::new(0, 0));
            return;
        }
        let (left, top, right, bottom) = (left.floor(), top.floor(), right.ceil(), bottom.ceil());
        let (width, height) = (right - left + 2.0, bottom - top + 2.0);
        if !(width.is_finite() && height.is_finite()) || width > MAX_SIDE || height > MAX_SIDE {
            self.oversized = true;
            self.clips.push(Pixmap::new(0, 0));
            return;
        }
        let canvas = Canvas {
            width: width as u32,
            height: height as u32,
            origin_x: (left as i32).saturating_sub(1),
            origin_y: (top as i32).saturating_sub(1),
        };
        // One mask byte and four layer bytes per pixel.
        if !self.reserve(u64::from(canvas.width) * u64::from(canvas.height) * 5) {
            self.clips.push(Pixmap::new(0, 0));
            return;
        }
        // The clip box snapped out to whole pixels; the margin stays
        // outside it.
        let mut mask = Pixmap::new(canvas.width, canvas.height);
        for y in 1..canvas.height.saturating_sub(1) {
            for x in 1..canvas.width.saturating_sub(1) {
                mask.set(x, y, 255);
            }
        }
        self.canvas = Some(canvas);
        self.layers
            .push(ColorPixmap::new(canvas.width, canvas.height));
        self.clips.push(mask);
    }

    fn pop_clip(&mut self) {
        self.clips.pop();
    }

    fn push_group(&mut self) {
        let Some(c) = self.canvas else {
            return;
        };
        let bytes = u64::from(c.width) * u64::from(c.height) * 4;
        // Once one group is skipped every later one is too, so the
        // skipped pops pair up with the innermost pushes.
        if self.skipped_groups == 0 && self.take_pass() && self.reserve(bytes) {
            self.layers.push(ColorPixmap::new(c.width, c.height));
        } else {
            self.skipped_groups = self.skipped_groups.saturating_add(1);
        }
    }

    fn pop_group(&mut self, mode: CompositeMode) {
        if let Some(rest) = self.skipped_groups.checked_sub(1) {
            self.skipped_groups = rest;
            return;
        }
        if self.layers.len() < 2 {
            return;
        }
        if let Some(top) = self.layers.pop() {
            if !self.take_pass() {
                return;
            }
            if let Some(parent) = self.layers.last_mut() {
                composite_layer(parent, &top, mode);
            }
        }
    }

    fn color(&mut self, color: ColorRef) {
        let (color, _) = self.resolver.color(color);
        let premul = to_premul(color);
        self.fill(|_, _| premul);
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
