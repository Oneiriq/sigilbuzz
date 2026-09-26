//! Internal document model: the fills, paints, clip and mask shapes
//! and filter chains the parser collects and the blit stage consumes.

use alloc::string::String;
use alloc::vec::Vec;

use sigilbuzz::tables::PathOp;
use sigilbuzz_paint::{ColorStop, Extend};

use crate::affine::Affine;

// =========================================================================
// Internal model
// =========================================================================

/// One paintable surface collected from the document. `ops` is in the
/// document's intrinsic coordinate space. The world transform
/// (document -> pixel) is applied on top at rasterize time.
#[derive(Debug, Clone)]
pub(super) struct Fill {
    pub(super) ops: Vec<PathOp>,
    pub(super) paint: Paint,
    /// Composed transform from the element's nested `<g transform=...>`
    /// stack, in document coordinates.
    pub(super) xform: Affine,
    /// Optional clip-path geometry, expressed in the same document
    /// space the parent fill was emitted in (so the same `xform` and
    /// world transform apply to both).
    pub(super) clip: Option<ClipShape>,
    /// Indicates whether this fill is the outline of a stroke (closed
    /// fill ribbon). Affects nothing in rendering but documents the
    /// pipeline split.
    #[allow(dead_code)]
    pub(super) is_stroke: bool,
    /// Optional filter chain to apply to this fill. Resolved at parse
    /// time from `filter="url(#id)"`. When set, the fill is rendered
    /// to a temporary `SourceGraphic` pixmap, the filter pipeline is
    /// walked, and the final primitive's output is composited under
    /// the canvas via Porter-Duff source-over.
    pub(super) filter: Option<Filter>,
    /// Optional alpha mask (SVG `<mask>` element) to apply to this
    /// fill. Distinct from `clip`: clip is binary inside/outside,
    /// mask is a continuous luminance-derived alpha multiplier (so
    /// gradient mask edges feather the masked element). When set, the
    /// element rasterizes to a SourceGraphic pixmap, the mask
    /// children are rendered into a same-size buffer, and per-pixel
    /// BT.709 luminance * mask source alpha modulates the
    /// SourceGraphic alpha before composite.
    pub(super) mask: Option<MaskShape>,
}

/// Paint source for a [`Fill`]. SVG-in-OT documents use solid color
/// almost exclusively, with the rare gradient for designer emoji.
#[derive(Debug, Clone)]
pub(super) enum Paint {
    /// Straight (un-premultiplied) RGBA.
    Solid([u8; 4]),
    /// Reference to a parsed gradient. Geometry is in document space;
    /// the renderer composes the world transform on top.
    Gradient(GradientPaint),
}

#[derive(Debug, Clone)]
pub(super) struct GradientPaint {
    pub(super) kind: GradKind,
    pub(super) stops: Vec<ColorStop>,
    pub(super) extend: Extend,
    /// Per-element opacity multiplier folded into stop alpha at sample
    /// time.
    pub(super) opacity: f32,
    /// `gradientTransform`. Composed onto the gradient geometry
    /// *before* the document -> pixel `world` matrix.
    pub(super) gradient_xform: Affine,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum GradKind {
    Linear {
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
    },
    Radial {
        cx: f32,
        cy: f32,
        r: f32,
        fx: f32,
        fy: f32,
    },
}

#[derive(Debug, Clone)]
pub(super) struct ClipShape {
    pub(super) ops: Vec<PathOp>,
    /// Transform stack the clipPath's child path inherited (clipPath
    /// contents may carry their own `transform=`).
    pub(super) xform: Affine,
}

/// A parsed `<mask>` element. Stored as a list of [`Fill`] records
/// because masks can hold any combination of shape primitives,
/// gradients, and per-element transforms, the same machinery that
/// renders the rest of the document. At render time the mask's fills
/// paint into a same-size scratch ColorPixmap, then either a per-pixel
/// BT.709 luminance derivation (`mask-type="luminance"`, the default)
/// or the source alpha channel directly (`mask-type="alpha"`) is used
/// as the alpha mask multiplied against the masked element's coverage.
#[derive(Debug, Clone)]
pub(super) struct MaskShape {
    pub(super) fills: Vec<Fill>,
    /// `mask-type="luminance" | "alpha"`. Luminance is the SVG
    /// default; alpha skips the BT.709 derivation and uses the mask
    /// buffer's alpha channel directly.
    pub(super) mask_type: MaskType,
    /// `maskUnits`: coordinate system the mask region (`x`, `y`,
    /// `width`, `height`) is expressed in. `UserSpaceOnUse` is the
    /// SVG default for our prior implementation; `ObjectBoundingBox`
    /// reinterprets the region as `[0, 1]²` of the masked element's
    /// bounding box.
    pub(super) units: MaskUnits,
    /// Mask region as parsed from `x`, `y`, `width`, `height`.
    /// Interpretation depends on `units`. When `units` is
    /// `UserSpaceOnUse`, this is currently informational only. The
    /// luminance fast path renders the mask body across the entire
    /// canvas, matching the prior PR #236 behavior.
    pub(super) region_x: f32,
    pub(super) region_y: f32,
    pub(super) region_w: f32,
    pub(super) region_h: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum MaskType {
    Luminance,
    Alpha,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum MaskUnits {
    UserSpaceOnUse,
    ObjectBoundingBox,
}

/// A parsed `<filter>` element: an ordered list of primitives forming
/// a small DAG keyed by `result=` names. The DAG is evaluated at render
/// time against a `SourceGraphic` pixmap (the filtered shape rendered
/// into a transparent buffer) and a `SourceAlpha` pixmap (same shape,
/// alpha only).
#[derive(Debug, Clone)]
pub(super) struct Filter {
    pub(super) primitives: Vec<FilterPrimitive>,
}

/// One `<fe*>` element: an input ref (`in="..."`), an output name
/// (`result="..."`), and an operation. Inputs default to `SourceGraphic`
/// for the first primitive and the previous primitive's result
/// thereafter (per SVG 1.1 §15.6).
#[derive(Debug, Clone)]
pub(super) struct FilterPrimitive {
    /// `in="..."`. `None` means "use previous primitive's output, or
    /// SourceGraphic if no previous primitive".
    pub(super) input: Option<String>,
    /// Second input (only meaningful for primitives that take two. For
    /// the v1 set, none do, but parsed for forward-compat).
    #[allow(dead_code)]
    pub(super) input2: Option<String>,
    /// `result="..."`. Names this primitive's output for later refs.
    /// `None` means "anonymous; only the next primitive can reference
    /// it (via the implicit-input chain)".
    pub(super) result: Option<String>,
    pub(super) op: FilterOp,
}

/// The actual operation a [`FilterPrimitive`] performs.
#[derive(Debug, Clone)]
pub(super) enum FilterOp {
    /// `feGaussianBlur stdDeviation="σ"` or `"σx σy"`. Implemented as a
    /// 3-pass box-blur approximation (separable, O(N) per pass per axis)
    /// that is visually indistinguishable from a true Gaussian for σ >= 1 and
    /// vastly faster than convolving a full kernel.
    GaussianBlur { std_dev_x: f32, std_dev_y: f32 },
    /// `feColorMatrix` in any of its `type=` flavors.
    ColorMatrix { matrix: [f32; 20] },
    /// `feOffset dx=... dy=...`. Pure translation, integer-rounded at blit
    /// time.
    Offset { dx: f32, dy: f32 },
    /// `feFlood flood-color=... flood-opacity=...`. Constant-color pixmap
    /// of the filter region. Color stored straight (un-premultiplied);
    /// premultiplication happens at materialize time.
    Flood { color: [u8; 4] },
    /// `feMerge` with N `<feMergeNode in="...">` children. Composites the
    /// inputs in document order via Porter-Duff source-over.
    Merge { inputs: Vec<String> },
}

/// Parsed SVG document.
#[derive(Debug, Clone)]
pub(super) struct SvgDoc {
    pub(super) view_w: f32,
    pub(super) view_h: f32,
    pub(super) view_x: f32,
    pub(super) view_y: f32,
    pub(super) fills: Vec<Fill>,
}
