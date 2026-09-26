//! Bounds of a color glyph computed from its paint tree.
//!
//! A COLRv1 glyph without a ClipList box has no declared bounds, so
//! HarfBuzz computes them before painting (`hb_paint_extents_*`) and
//! clips the glyph to the result. [`ExtentsSink`] is a port of that
//! computation driven by [`crate::walk`]:
//!
//! - a stack of transforms, a stack of clip bounds (starting unbounded),
//!   and a stack of group bounds (starting empty);
//! - a glyph clip contributes the control box of the glyph's outline,
//!   and a rectangle clip the rectangle, each mapped through the
//!   current transform to its axis-aligned bounding box and intersected
//!   with the enclosing clip;
//! - every fill unions the current clip into the current group, so a
//!   fill that no glyph or rectangle encloses makes the glyph unbounded;
//! - popping a group merges its bounds into the parent the way the
//!   composite mode can reach: `Clear` empties, `Src` and `SrcOut` take
//!   the source, `Dest` and `DestOut` keep the backdrop, `SrcIn` and
//!   `DestIn` intersect, and every other mode unions.
//!
//! Bounds here are in design units: the root transform is the identity.
//! HarfBuzz works at font scale, which differs only by the root scale
//! factor for fonts without synthetic slant.

use alloc::vec::Vec;

use sigilbuzz::tables::colr::CompositeMode;
use sigilbuzz::tables::PathOp;
use sigilbuzz::Face;

use crate::transform::Transform2D;
use crate::walk::{ColorLineRef, ColorRef, PaintSink, RootClip};

/// An axis-aligned box. The default `(0, 0, -1, -1)` is "void": it
/// holds no point yet.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Extents {
    pub(crate) x_min: f32,
    pub(crate) y_min: f32,
    pub(crate) x_max: f32,
    pub(crate) y_max: f32,
}

impl Default for Extents {
    fn default() -> Self {
        Self {
            x_min: 0.0,
            y_min: 0.0,
            x_max: -1.0,
            y_max: -1.0,
        }
    }
}

impl Extents {
    fn is_empty(&self) -> bool {
        self.x_min >= self.x_max || self.y_min >= self.y_max
    }

    fn is_void(&self) -> bool {
        self.x_min > self.x_max
    }

    fn add_point(&mut self, x: f32, y: f32) {
        if self.is_void() {
            *self = Self {
                x_min: x,
                y_min: y,
                x_max: x,
                y_max: y,
            };
        } else {
            self.x_min = self.x_min.min(x);
            self.y_min = self.y_min.min(y);
            self.x_max = self.x_max.max(x);
            self.y_max = self.y_max.max(y);
        }
    }

    fn union(&mut self, o: &Self) {
        if o.is_empty() {
            return;
        }
        if self.is_empty() {
            *self = *o;
            return;
        }
        self.x_min = self.x_min.min(o.x_min);
        self.y_min = self.y_min.min(o.y_min);
        self.x_max = self.x_max.max(o.x_max);
        self.y_max = self.y_max.max(o.y_max);
    }

    fn intersect(&mut self, o: &Self) {
        self.x_min = self.x_min.max(o.x_min);
        self.y_min = self.y_min.max(o.y_min);
        self.x_max = self.x_max.min(o.x_max);
        self.y_max = self.y_max.min(o.y_max);
    }

    /// The bounding box of this box's corners mapped through `t`.
    fn transformed(&self, t: Transform2D) -> Self {
        let mut out = Self::default();
        for (x, y) in [
            (self.x_min, self.y_min),
            (self.x_min, self.y_max),
            (self.x_max, self.y_min),
            (self.x_max, self.y_max),
        ] {
            let (x, y) = t.apply(x, y);
            out.add_point(x, y);
        }
        out
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Unbounded,
    Bounded,
    Empty,
}

/// A box that may also be "everywhere" or "nowhere".
#[derive(Debug, Clone, Copy)]
struct Bounds {
    status: Status,
    extents: Extents,
}

impl Bounds {
    const fn of(status: Status) -> Self {
        Self {
            status,
            extents: Extents {
                x_min: 0.0,
                y_min: 0.0,
                x_max: -1.0,
                y_max: -1.0,
            },
        }
    }

    fn from_extents(extents: Extents) -> Self {
        let status = if extents.is_empty() {
            Status::Empty
        } else {
            Status::Bounded
        };
        Self { status, extents }
    }

    fn union(&mut self, o: &Self) {
        match (o.status, self.status) {
            (Status::Unbounded, _) => self.status = Status::Unbounded,
            (Status::Bounded, Status::Empty) => *self = *o,
            (Status::Bounded, Status::Bounded) => self.extents.union(&o.extents),
            _ => {}
        }
    }

    fn intersect(&mut self, o: &Self) {
        match (o.status, self.status) {
            (Status::Empty, _) => self.status = Status::Empty,
            (Status::Bounded, Status::Unbounded) => *self = *o,
            (Status::Bounded, Status::Bounded) => {
                self.extents.intersect(&o.extents);
                if self.extents.is_empty() {
                    self.status = Status::Empty;
                }
            }
            _ => {}
        }
    }
}

/// Paint sink that accumulates the bounds of everything painted.
pub(crate) struct ExtentsSink<'f, 'a, 'c> {
    face: &'f Face<'a>,
    coords: &'c [f32],
    transforms: Vec<Transform2D>,
    clips: Vec<Bounds>,
    groups: Vec<Bounds>,
}

impl<'f, 'a, 'c> ExtentsSink<'f, 'a, 'c> {
    pub(crate) fn new(face: &'f Face<'a>, coords: &'c [f32]) -> Self {
        Self {
            face,
            coords,
            transforms: alloc::vec![Transform2D::IDENTITY],
            clips: alloc::vec![Bounds::of(Status::Unbounded)],
            groups: alloc::vec![Bounds::of(Status::Empty)],
        }
    }

    /// The accumulated bounds as a root clip: the extents, and whether
    /// any fill escaped every clip.
    pub(crate) fn root_clip(&self) -> RootClip {
        let group = self
            .groups
            .last()
            .copied()
            .unwrap_or(Bounds::of(Status::Empty));
        let e = group.extents;
        RootClip::Extents {
            x_min: e.x_min,
            y_min: e.y_min,
            x_max: e.x_max,
            y_max: e.y_max,
            bounded: group.status != Status::Unbounded,
        }
    }

    fn top(&self) -> Transform2D {
        self.transforms
            .last()
            .copied()
            .unwrap_or(Transform2D::IDENTITY)
    }

    fn push_clip(&mut self, extents: Option<Extents>) {
        let mut bounds = match extents {
            Some(e) => Bounds::from_extents(e.transformed(self.top())),
            None => Bounds::of(Status::Empty),
        };
        if let Some(parent) = self.clips.last() {
            bounds.intersect(parent);
        }
        self.clips.push(bounds);
    }

    fn paint(&mut self) {
        let clip = self
            .clips
            .last()
            .copied()
            .unwrap_or(Bounds::of(Status::Unbounded));
        if let Some(group) = self.groups.last_mut() {
            group.union(&clip);
        }
    }

    /// The control box of `glyph`'s outline, or `None` when it has no
    /// points.
    fn glyph_box(&self, glyph: u16) -> Option<Extents> {
        let outline = self
            .face
            .glyph_outline_at_coords(glyph, self.coords)
            .ok()
            .flatten()?;
        let mut e = Extents::default();
        for op in outline.ops() {
            match *op {
                PathOp::MoveTo { x, y } | PathOp::LineTo { x, y } => e.add_point(x, y),
                PathOp::QuadTo { cx, cy, x, y } => {
                    e.add_point(cx, cy);
                    e.add_point(x, y);
                }
                PathOp::CubicTo {
                    c1x,
                    c1y,
                    c2x,
                    c2y,
                    x,
                    y,
                } => {
                    e.add_point(c1x, c1y);
                    e.add_point(c2x, c2y);
                    e.add_point(x, y);
                }
                PathOp::Close => {}
            }
        }
        (!e.is_void()).then_some(e)
    }
}

impl PaintSink for ExtentsSink<'_, '_, '_> {
    fn push_transform(&mut self, transform: Transform2D) {
        let t = transform.then(self.top());
        self.transforms.push(t);
    }

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
        let extents = self.glyph_box(glyph);
        self.push_clip(extents);
    }

    fn push_clip_rectangle(&mut self, x_min: f32, y_min: f32, x_max: f32, y_max: f32) {
        self.push_clip(Some(Extents {
            x_min,
            y_min,
            x_max,
            y_max,
        }));
    }

    fn push_root_clip(&mut self, _clip: RootClip) {
        // The walk that feeds this sink is unclipped; a root clip never
        // arrives, but keep the stacks balanced if one does.
        self.clips.push(Bounds::of(Status::Unbounded));
    }

    fn pop_clip(&mut self) {
        if self.clips.len() > 1 {
            self.clips.pop();
        }
    }

    fn push_group(&mut self) {
        self.groups.push(Bounds::of(Status::Empty));
    }

    fn pop_group(&mut self, mode: CompositeMode) {
        if self.groups.len() < 2 {
            return;
        }
        let Some(src) = self.groups.pop() else {
            return;
        };
        let Some(backdrop) = self.groups.last_mut() else {
            return;
        };
        match mode {
            CompositeMode::Clear => backdrop.status = Status::Empty,
            CompositeMode::Src | CompositeMode::SrcOut => *backdrop = src,
            CompositeMode::Dest | CompositeMode::DestOut => {}
            CompositeMode::SrcIn | CompositeMode::DestIn => backdrop.intersect(&src),
            _ => backdrop.union(&src),
        }
    }

    fn color(&mut self, _color: ColorRef) {
        self.paint();
    }

    fn linear_gradient(
        &mut self,
        _line: ColorLineRef<'_>,
        _p0: (f32, f32),
        _p1: (f32, f32),
        _p2: (f32, f32),
    ) {
        self.paint();
    }

    fn radial_gradient(
        &mut self,
        _line: ColorLineRef<'_>,
        _c0: (f32, f32),
        _r0: f32,
        _c1: (f32, f32),
        _r1: f32,
    ) {
        self.paint();
    }

    fn sweep_gradient(
        &mut self,
        _line: ColorLineRef<'_>,
        _center: (f32, f32),
        _start_angle: f32,
        _end_angle: f32,
    ) {
        self.paint();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x_min: f32, y_min: f32, x_max: f32, y_max: f32) -> Extents {
        Extents {
            x_min,
            y_min,
            x_max,
            y_max,
        }
    }

    #[test]
    fn bounds_union_and_intersect_follow_status() {
        let a = Bounds::from_extents(rect(0.0, 0.0, 10.0, 10.0));
        let b = Bounds::from_extents(rect(5.0, 5.0, 20.0, 20.0));
        let mut u = a;
        u.union(&b);
        assert_eq!(u.extents, rect(0.0, 0.0, 20.0, 20.0));
        let mut i = a;
        i.intersect(&b);
        assert_eq!(i.extents, rect(5.0, 5.0, 10.0, 10.0));
        let mut disjoint = a;
        disjoint.intersect(&Bounds::from_extents(rect(50.0, 50.0, 60.0, 60.0)));
        assert_eq!(disjoint.status, Status::Empty);
        let mut unbounded = Bounds::of(Status::Unbounded);
        unbounded.intersect(&a);
        assert_eq!(unbounded.status, Status::Bounded);
        let mut empty = Bounds::of(Status::Empty);
        empty.union(&a);
        assert_eq!(empty.extents, a.extents);
        let mut any = a;
        any.union(&Bounds::of(Status::Unbounded));
        assert_eq!(any.status, Status::Unbounded);
    }

    #[test]
    fn transformed_boxes_cover_rotated_corners() {
        let r = rect(0.0, 0.0, 10.0, 20.0);
        let t = Transform2D {
            xx: 0.0,
            yx: 1.0,
            xy: -1.0,
            yy: 0.0,
            dx: 0.0,
            dy: 0.0,
        };
        assert_eq!(r.transformed(t), rect(-20.0, 0.0, 0.0, 10.0));
    }
}
