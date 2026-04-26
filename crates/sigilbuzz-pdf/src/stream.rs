//! PathOp -> PDF content-stream operator translation.
//!
//! PDF's path-construction operators (`m`, `l`, `c`, `h`, `f`) are a
//! near-perfect match for sigilbuzz's [`PathOp`] enum, with one
//! exception: PDF has *no* quadratic Bezier operator. Quadratics
//! must be promoted to cubics. The standard degree-elevation
//! conversion is exact: a quadratic with endpoints `p0`, `p2` and
//! control point `c` is identical to the cubic with endpoints
//! `p0`, `p2` and control points
//!
//! ```text
//!   c1 = p0 + 2/3 * (c - p0)
//!   c2 = p2 + 2/3 * (c - p2)
//! ```
//!
//! That is the conversion the emitter performs verbatim — no
//! flattening, no error term, no subdivision. The cubic produced is
//! mathematically equal to the original quadratic at every `t`.
//!
//! The emitter also tracks the current pen position so that
//! `QuadTo` can supply `p0`. PDF itself does not require us to
//! track this (the `c` operator is self-contained once we hand it
//! the four points), but the quad-to-cubic formula does.
//!
//! Type 3 d1 prologue: every CharProc must begin with the `d1`
//! operator, which records the glyph's advance and bbox up-front so
//! the PDF rasteriser does not have to scan the stream to find them.
//! The format is `wx wy llx lly urx ury d1`, where `wy` is always 0
//! for horizontal writing modes.

use alloc::vec::Vec;

use sigilbuzz::tables::PathOp;

use crate::Bbox;

/// Format a single `f32` with the smallest representation that
/// round-trips to the same value, then drop a trailing `.0` if any —
/// PDF parsers accept both `1` and `1.0`, and dropping the suffix
/// keeps the output compact and snapshot-stable.
///
/// Non-finite inputs (NaN, ±∞) are coerced to `0` because PDF
/// numeric objects do not admit `NaN` / `inf` tokens — emitting them
/// would break content-stream parsing in every conforming reader. A
/// pathological glyph outline (CFF charstring whose blend evaluation
/// overflows under extreme variation coords, for example) would
/// otherwise leak those literals into the output. See issue #216.
fn write_num(out: &mut Vec<u8>, value: f32) {
    let safe = if value.is_finite() { value } else { 0.0 };
    use core::fmt::Write;
    // Buffered into a local stack string so we can post-process the
    // ".0" suffix without an allocation. 32 bytes is comfortably
    // larger than any f32's `{}` rendering (max ~15 chars).
    let mut buf = heapless_str::HeaplessStr::<32>::new();
    let _ = write!(buf, "{safe}");
    let s = buf.as_str();
    let trimmed = s.strip_suffix(".0").unwrap_or(s);
    out.extend_from_slice(trimmed.as_bytes());
}

/// A 32-byte ASCII scratch — kept tiny and inline so the no-std
/// build does not pull in any extra crate. Only the fmt::Write
/// machinery is used.
mod heapless_str {
    use core::fmt;

    pub struct HeaplessStr<const N: usize> {
        buf: [u8; N],
        len: usize,
    }

    impl<const N: usize> HeaplessStr<N> {
        pub const fn new() -> Self {
            Self {
                buf: [0; N],
                len: 0,
            }
        }
        pub fn as_str(&self) -> &str {
            // SAFETY: only `fmt::Write` mutates `buf`, and that path
            // only writes valid UTF-8 strings up to `len`.
            unsafe { core::str::from_utf8_unchecked(&self.buf[..self.len]) }
        }
    }

    impl<const N: usize> fmt::Write for HeaplessStr<N> {
        fn write_str(&mut self, s: &str) -> fmt::Result {
            let bytes = s.as_bytes();
            if self.len + bytes.len() > N {
                return Err(fmt::Error);
            }
            self.buf[self.len..self.len + bytes.len()].copy_from_slice(bytes);
            self.len += bytes.len();
            Ok(())
        }
    }
}

/// Push a single number followed by a single ASCII space.
fn push_num_sp(out: &mut Vec<u8>, value: f32) {
    write_num(out, value);
    out.push(b' ');
}

/// Emit the Type 3 `d1` prologue: `wx 0 llx lly urx ury d1\n`.
///
/// `wx` is the glyph's horizontal advance; `wy` is always 0 for
/// horizontal-writing fonts. `(llx, lly, urx, ury)` is the glyph
/// bounding box in glyph-design-unit space.
pub fn emit_d1_prologue(out: &mut Vec<u8>, advance: f32, bbox: Bbox) {
    push_num_sp(out, advance);
    out.extend_from_slice(b"0 ");
    push_num_sp(out, bbox.xmin);
    push_num_sp(out, bbox.ymin);
    push_num_sp(out, bbox.xmax);
    push_num_sp(out, bbox.ymax);
    out.extend_from_slice(b"d1\n");
}

/// Emit the `f` (non-zero winding fill) epilogue.
pub fn emit_fill_epilogue(out: &mut Vec<u8>) {
    out.extend_from_slice(b"f\n");
}

/// Emit the PDF operator sequence corresponding to a PathOp slice
/// into `out`. Tracks the current pen so `QuadTo` can synthesise the
/// cubic's `p0`. The pen starts unset; `QuadTo` issued before any
/// `MoveTo` falls back to `(0, 0)` — that path is undefined in
/// well-formed sigilbuzz outlines so it is not a real concern, but
/// the fallback keeps the emitter total.
pub fn emit_path_ops(out: &mut Vec<u8>, ops: &[PathOp]) {
    let mut pen_x = 0.0_f32;
    let mut pen_y = 0.0_f32;
    for op in ops {
        match *op {
            PathOp::MoveTo { x, y } => {
                push_num_sp(out, x);
                push_num_sp(out, y);
                out.extend_from_slice(b"m\n");
                pen_x = x;
                pen_y = y;
            }
            PathOp::LineTo { x, y } => {
                push_num_sp(out, x);
                push_num_sp(out, y);
                out.extend_from_slice(b"l\n");
                pen_x = x;
                pen_y = y;
            }
            PathOp::QuadTo { cx, cy, x, y } => {
                // Degree-elevate the quadratic to a cubic:
                //   c1 = p0 + 2/3 * (c - p0)
                //   c2 = p2 + 2/3 * (c - p2)
                let two_thirds = 2.0_f32 / 3.0_f32;
                let c1x = pen_x + two_thirds * (cx - pen_x);
                let c1y = pen_y + two_thirds * (cy - pen_y);
                let c2x = x + two_thirds * (cx - x);
                let c2y = y + two_thirds * (cy - y);
                push_num_sp(out, c1x);
                push_num_sp(out, c1y);
                push_num_sp(out, c2x);
                push_num_sp(out, c2y);
                push_num_sp(out, x);
                push_num_sp(out, y);
                out.extend_from_slice(b"c\n");
                pen_x = x;
                pen_y = y;
            }
            PathOp::CubicTo {
                c1x,
                c1y,
                c2x,
                c2y,
                x,
                y,
            } => {
                push_num_sp(out, c1x);
                push_num_sp(out, c1y);
                push_num_sp(out, c2x);
                push_num_sp(out, c2y);
                push_num_sp(out, x);
                push_num_sp(out, y);
                out.extend_from_slice(b"c\n");
                pen_x = x;
                pen_y = y;
            }
            PathOp::Close => {
                out.extend_from_slice(b"h\n");
                // After `h` the pen is back at the contour's start.
                // We do not track that explicitly because well-formed
                // outlines start every new contour with a fresh
                // MoveTo, which resets the pen.
            }
        }
    }
}

/// Compute the bbox of an outline in glyph-design-unit space by
/// folding every endpoint and control point.
///
/// Control points are included because PDF stores them verbatim; a
/// curve's actual extent never exceeds its convex hull, so this is
/// a safe (and sometimes loose) overestimate that matches what most
/// PDF rasterisers expect from `FontBBox`.
pub fn outline_bbox(ops: &[PathOp]) -> Bbox {
    let mut bb = Bbox::empty();
    for op in ops {
        match *op {
            PathOp::MoveTo { x, y } | PathOp::LineTo { x, y } => bb.extend(x, y),
            PathOp::QuadTo { cx, cy, x, y } => {
                bb.extend(cx, cy);
                bb.extend(x, y);
            }
            PathOp::CubicTo {
                c1x,
                c1y,
                c2x,
                c2y,
                x,
                y,
            } => {
                bb.extend(c1x, c1y);
                bb.extend(c2x, c2y);
                bb.extend(x, y);
            }
            PathOp::Close => {}
        }
    }
    bb
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[u8]) -> &str {
        core::str::from_utf8(v).unwrap()
    }

    #[test]
    fn move_line_quad_close_emits_expected_stream() {
        // Walk a 4-segment path that exercises every operator,
        // including the quad-to-cubic conversion.
        let ops = [
            PathOp::MoveTo { x: 0.0, y: 0.0 },
            PathOp::LineTo { x: 30.0, y: 0.0 },
            // Quadratic with p0 = (30, 0), c = (60, 0), p2 = (60, 30):
            //   c1 = (30,0) + 2/3 * (30, 0)   = (50, 0)
            //   c2 = (60,30) + 2/3 * (0,-30)  = (60, 10)
            PathOp::QuadTo {
                cx: 60.0,
                cy: 0.0,
                x: 60.0,
                y: 30.0,
            },
            PathOp::Close,
        ];
        let mut out = Vec::new();
        emit_path_ops(&mut out, &ops);
        // Lock the byte stream verbatim so any future change to the
        // emitter has to update this fixture deliberately.
        let expected = "0 0 m\n30 0 l\n50 0 60 10 60 30 c\nh\n";
        assert_eq!(s(&out), expected);
    }

    #[test]
    fn quad_to_cubic_conversion_is_exact_at_endpoints() {
        // Independently lock down the formula on a non-axis-aligned
        // quadratic so the test does not silently absorb a sign flip.
        let ops = [
            PathOp::MoveTo { x: 10.0, y: 20.0 },
            PathOp::QuadTo {
                cx: 40.0,
                cy: 80.0,
                x: 70.0,
                y: 20.0,
            },
        ];
        let mut out = Vec::new();
        emit_path_ops(&mut out, &ops);
        // p0 = (10, 20), c = (40, 80), p2 = (70, 20)
        //   c1 = (10,20) + 2/3*(30, 60) = (30, 60)
        //   c2 = (70,20) + 2/3*(-30,60) = (50, 60)
        let expected = "10 20 m\n30 60 50 60 70 20 c\n";
        assert_eq!(s(&out), expected);
    }

    #[test]
    fn cubic_passes_through_unchanged() {
        let ops = [
            PathOp::MoveTo { x: 0.0, y: 0.0 },
            PathOp::CubicTo {
                c1x: 1.0,
                c1y: 2.0,
                c2x: 3.0,
                c2y: 4.0,
                x: 5.0,
                y: 6.0,
            },
        ];
        let mut out = Vec::new();
        emit_path_ops(&mut out, &ops);
        assert_eq!(s(&out), "0 0 m\n1 2 3 4 5 6 c\n");
    }

    #[test]
    fn d1_prologue_records_advance_and_bbox() {
        let mut out = Vec::new();
        emit_d1_prologue(
            &mut out,
            500.0,
            Bbox {
                xmin: 0.0,
                ymin: -100.0,
                xmax: 480.0,
                ymax: 700.0,
            },
        );
        assert_eq!(s(&out), "500 0 0 -100 480 700 d1\n");
    }

    #[test]
    fn fill_epilogue_is_single_f() {
        let mut out = Vec::new();
        emit_fill_epilogue(&mut out);
        assert_eq!(s(&out), "f\n");
    }

    #[test]
    fn outline_bbox_covers_endpoints_and_controls() {
        let ops = [
            PathOp::MoveTo { x: 0.0, y: 0.0 },
            PathOp::QuadTo {
                cx: 100.0,
                cy: 200.0,
                x: 50.0,
                y: 0.0,
            },
            PathOp::CubicTo {
                c1x: -10.0,
                c1y: 10.0,
                c2x: 60.0,
                c2y: 250.0,
                x: 30.0,
                y: 30.0,
            },
            PathOp::Close,
        ];
        let bb = outline_bbox(&ops);
        assert_eq!(bb.xmin, -10.0);
        assert_eq!(bb.ymin, 0.0);
        assert_eq!(bb.xmax, 100.0);
        assert_eq!(bb.ymax, 250.0);
    }

    #[test]
    fn empty_outline_yields_empty_bbox() {
        let bb = outline_bbox(&[]);
        assert!(bb.is_empty());
    }

    #[test]
    fn write_num_coerces_non_finite_to_zero() {
        // Issue #216: PDF numeric tokens cannot be NaN / inf — those
        // would fail to parse in every conforming reader. A pathological
        // glyph outline whose coords overflow under variation evaluation
        // could otherwise leak literal "NaN"/"inf"/"-inf" tokens into the
        // content stream.
        for &bad in &[f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut out = Vec::new();
            write_num(&mut out, bad);
            assert_eq!(s(&out), "0", "non-finite {bad} leaked: {:?}", s(&out));
        }
    }

    #[test]
    fn emit_path_ops_with_non_finite_coords_emits_only_finite_tokens() {
        let ops = [
            PathOp::MoveTo {
                x: f32::NAN,
                y: 0.0,
            },
            PathOp::LineTo {
                x: f32::INFINITY,
                y: f32::NEG_INFINITY,
            },
            PathOp::CubicTo {
                c1x: f32::NAN,
                c1y: 0.0,
                c2x: 0.0,
                c2y: 0.0,
                x: 0.0,
                y: 0.0,
            },
            PathOp::Close,
        ];
        let mut out = Vec::new();
        emit_path_ops(&mut out, &ops);
        let stream = s(&out);
        assert!(!stream.contains("NaN"), "NaN leaked: {stream:?}");
        assert!(!stream.contains("inf"), "inf leaked: {stream:?}");
    }
}
