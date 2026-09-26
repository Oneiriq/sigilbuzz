//! `sigilbuzz-svg`: SVG serializer for sigilbuzz glyph outlines and
//! COLRv1 color glyphs.
//!
//! sigilbuzz exposes glyph outlines as a flat
//! [`PathOp`] stream and (via the
//! `sigilbuzz-paint` companion) a flat [`DrawCmd`](sigilbuzz_paint::DrawCmd)
//! stream for color glyphs. This crate turns either of those into a
//! self-contained `<svg>` element ready to drop into a document, a
//! preview tool, or a font-debug page.
//!
//! The output is plain text: there is no XML library on the write
//! path. SVG path data, gradient stops, and transform matrices are all
//! emitted with `core::fmt` formatting and a single
//! deterministic-precision policy ([`PRECISION`] decimals). Same input
//! always produces the same byte sequence.
//!
//! ## Outline-only mode
//!
//! With `--no-default-features` (or `default-features = false`) the
//! crate compiles without the `color` feature and depends only on
//! sigilbuzz. The COLRv1 paths in this module are conditionally
//! compiled out, so you pay nothing for color support you do not use.
//!
//! ## Sweep gradients
//!
//! SVG 1.1 has no native sweep / conic gradient. The COLRv1 evaluator
//! still emits one for `PaintSweepGradient`, so this crate degrades
//! it to an SVG `<linearGradient>` running through the sweep center
//! along the bisector of the start and end angles, with a comment in
//! the output noting the substitution.
//! That keeps the SVG well-formed in every viewer; consumers that
//! require true sweep rendering should drive `sigilbuzz-paint`
//! directly into a renderer that supports it.
//!
//! ```no_run
//! use sigilbuzz::Face;
//! use sigilbuzz_svg::glyph_to_svg;
//!
//! # fn demo(face: &Face<'_>) {
//! if let Some(svg) = glyph_to_svg(face, 42) {
//!     // svg is a complete `<svg ...>...</svg>` document
//!     let _ = svg;
//! }
//! # }
//! ```

#![cfg_attr(not(feature = "std"), no_std)]
#![warn(missing_docs)]
#![forbid(unsafe_op_in_unsafe_fn)]

extern crate alloc;

use alloc::format;
use alloc::string::String;

use sigilbuzz::tables::PathOp;
use sigilbuzz::Face;

#[cfg(feature = "color")]
mod color;

#[cfg(feature = "color")]
pub use color::glyph_to_svg_color;

/// Glyph-id alias mirroring sigilbuzz's on-disk u16.
pub type GlyphId = u16;

/// Normalized variation coordinate alias. sigilbuzz uses `f32` for the
/// shaper's coord slice; the SVG crate keeps the same shape so a
/// caller can hand its `&[f32]` straight through.
pub type F2Dot14 = f32;

/// Decimal places used when emitting design-unit coordinates. Three
/// digits is well below the sub-pixel threshold at any practical
/// rendering size and keeps SVG diff-friendly.
pub const PRECISION: usize = 3;

/// Margin (in design units) added around the glyph bbox when computing
/// the SVG `viewBox`. Stops the rendered glyph from kissing the
/// viewBox edge in preview tools.
pub const VIEWBOX_MARGIN: f32 = 32.0;

// =========================================================================
// Public outline API
// =========================================================================

/// Serializes the static outline of `gid` into a complete SVG
/// document. Returns `None` when the glyph has no outline (whitespace
/// or out-of-range gid) or the font carries no outline table the core
/// crate can read.
///
/// The returned string is a self-contained `<svg>` element with a
/// `viewBox` sized to the glyph's bounding box plus [`VIEWBOX_MARGIN`]
/// on every side. The outline is emitted as a single
/// `<path d="..."/>` filled in black; OpenType design-unit space is
/// flipped on the Y axis (SVG's Y points down, OpenType's points up)
/// via a `transform="matrix(1 0 0 -1 0 H)"` on the wrapper `<g>`.
#[must_use]
pub fn glyph_to_svg(face: &Face<'_>, gid: GlyphId) -> Option<String> {
    glyph_to_svg_at_coords(face, gid, &[])
}

/// Variable-font flavor of [`glyph_to_svg`]. `coords` is the
/// normalized axis vector. Pass an empty slice for the static
/// outline (equivalent to [`glyph_to_svg`]).
#[must_use]
pub fn glyph_to_svg_at_coords(face: &Face<'_>, gid: GlyphId, coords: &[F2Dot14]) -> Option<String> {
    let outline = face.glyph_outline_at_coords(gid, coords).ok().flatten()?;
    if outline.is_empty() {
        return None;
    }
    let bbox = path_bbox(outline.ops())?;
    let path_d = path_data(outline.ops());
    Some(render_outline_svg(&path_d, bbox))
}

// =========================================================================
// Path-data emission
// =========================================================================

/// Converts a `PathOp` stream into an SVG `d=` attribute payload.
///
/// The mapping is the canonical one from the SVG 1.1 path grammar:
/// `MoveTo` -> `M`, `LineTo` -> `L`, `QuadTo` -> `Q`, `CubicTo` -> `C`,
/// `Close` -> `Z`. Each op is space-prefixed so the output is a
/// well-formed path data string when concatenated.
#[must_use]
pub fn path_data(ops: &[PathOp]) -> String {
    let mut out = String::new();
    for (i, op) in ops.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        match *op {
            PathOp::MoveTo { x, y } => append_cmd(&mut out, 'M', &[x, y]),
            PathOp::LineTo { x, y } => append_cmd(&mut out, 'L', &[x, y]),
            PathOp::QuadTo { cx, cy, x, y } => append_cmd(&mut out, 'Q', &[cx, cy, x, y]),
            PathOp::CubicTo {
                c1x,
                c1y,
                c2x,
                c2y,
                x,
                y,
            } => append_cmd(&mut out, 'C', &[c1x, c1y, c2x, c2y, x, y]),
            PathOp::Close => out.push('Z'),
        }
    }
    out
}

/// Appends `cmd` followed by each coordinate, one space before each.
/// SVG also accepts no space between the command letter and the first
/// coordinate. The space is kept for readability.
fn append_cmd(out: &mut String, cmd: char, coords: &[f32]) {
    out.push(cmd);
    for &c in coords {
        out.push(' ');
        push_num(out, c);
    }
}

/// Formats a float into `out` with a stable [`PRECISION`] decimal
/// places, stripping trailing zeros and the trailing `.` when the
/// value is integral. Determinism: a fixed precision plus the same
/// trim policy means the same input always produces the same bytes.
///
/// Non-finite inputs (NaN and the infinities) are coerced to `0` so
/// the emitted SVG stays well-formed: `format!("{NaN:.3}")`
/// round-trips to the literal string `"NaN"` and `format!("{inf:.3}")`
/// to `"inf"`, neither of which is a valid SVG numeric token. A
/// pathological glyph outline (e.g. a CFF charstring whose blend
/// evaluation overflows under extreme variation coords) would
/// otherwise leak those tokens into the document and corrupt
/// downstream parsers.
pub(crate) fn push_num(out: &mut String, v: f32) {
    if !v.is_finite() {
        out.push('0');
        return;
    }
    // Round to PRECISION decimals first, then format. Avoids the
    // "1.4999999..." artefacts that show up when f32 fed directly to
    // `{:.3}` rounds inconsistently across platforms. We also collapse
    // `-0` to `0` here so signed zero arithmetic doesn't bleed into
    // the deterministic output.
    let scale = 10_f32.powi(PRECISION as i32);
    let scaled = v * scale;
    // Above about 3.4e35 the scaled value overflows to infinity. An
    // f32 that large is an integer already, so it needs no rounding.
    let mut rounded = if scaled.is_finite() {
        scaled.round() / scale
    } else {
        v
    };
    if rounded == 0.0 {
        rounded = 0.0;
    }
    let s = format!("{rounded:.PRECISION$}");
    let trimmed = trim_zeros(&s);
    if trimmed.is_empty() || trimmed == "-" || trimmed == "-0" {
        out.push('0');
    } else {
        out.push_str(trimmed);
    }
}

fn trim_zeros(s: &str) -> &str {
    if !s.contains('.') {
        return s;
    }
    let trimmed = s.trim_end_matches('0');
    trimmed.trim_end_matches('.')
}

// =========================================================================
// Bounding box + viewBox
// =========================================================================

/// Computes the axis-aligned bounding box of an op stream. Returns
/// `None` if the stream contains no point-bearing op (e.g. only a
/// `Close`). Control points contribute to the bbox so the viewBox
/// always contains the full geometry, including arc bulges.
#[must_use]
pub fn path_bbox(ops: &[PathOp]) -> Option<(f32, f32, f32, f32)> {
    let mut min_x = f32::INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    let mut any = false;
    let mut grow = |x: f32, y: f32| {
        if x < min_x {
            min_x = x;
        }
        if x > max_x {
            max_x = x;
        }
        if y < min_y {
            min_y = y;
        }
        if y > max_y {
            max_y = y;
        }
    };
    for op in ops {
        match *op {
            PathOp::MoveTo { x, y } | PathOp::LineTo { x, y } => {
                any = true;
                grow(x, y);
            }
            PathOp::QuadTo { cx, cy, x, y } => {
                any = true;
                grow(cx, cy);
                grow(x, y);
            }
            PathOp::CubicTo {
                c1x,
                c1y,
                c2x,
                c2y,
                x,
                y,
            } => {
                any = true;
                grow(c1x, c1y);
                grow(c2x, c2y);
                grow(x, y);
            }
            PathOp::Close => {}
        }
    }
    any.then_some((min_x, min_y, max_x, max_y))
}

fn render_outline_svg(path_d: &str, bbox: (f32, f32, f32, f32)) -> String {
    let (min_x, min_y, max_x, max_y) = bbox;
    let vx = min_x - VIEWBOX_MARGIN;
    let vy = min_y - VIEWBOX_MARGIN;
    let vw = (max_x - min_x) + 2.0 * VIEWBOX_MARGIN;
    let vh = (max_y - min_y) + 2.0 * VIEWBOX_MARGIN;
    // Y-flip via wrapper g: SVG y-axis points down, OpenType up. We
    // anchor the flip so the glyph's design-unit y maps to (vy + vh)
    // - y in SVG coords; that way the viewBox stays in the same
    // numeric range as the glyph's bbox + margin.
    let flip_offset = vy + vh + vy; // == 2*vy + vh, matches matrix(1 0 0 -1 0 ...)
    let mut out = String::new();
    out.push_str(r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox=""#);
    push_num(&mut out, vx);
    out.push(' ');
    push_num(&mut out, vy);
    out.push(' ');
    push_num(&mut out, vw);
    out.push(' ');
    push_num(&mut out, vh);
    out.push_str(r#"">"#);
    out.push_str(r#"<g transform="matrix(1 0 0 -1 0 "#);
    push_num(&mut out, flip_offset);
    out.push_str(r#")">"#);
    out.push_str(r#"<path d=""#);
    out.push_str(path_d);
    out.push_str(r#"" fill="black"/>"#);
    out.push_str("</g></svg>");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_data_handles_each_op_kind() {
        let ops = [
            PathOp::MoveTo { x: 0.0, y: 0.0 },
            PathOp::LineTo { x: 10.0, y: 0.0 },
            PathOp::QuadTo {
                cx: 15.0,
                cy: 5.0,
                x: 10.0,
                y: 10.0,
            },
            PathOp::CubicTo {
                c1x: 8.0,
                c1y: 12.0,
                c2x: 4.0,
                c2y: 12.0,
                x: 0.0,
                y: 10.0,
            },
            PathOp::Close,
        ];
        let d = path_data(&ops);
        assert!(d.starts_with("M 0 0"));
        assert!(d.contains("L 10 0"));
        assert!(d.contains("Q 15 5 10 10"));
        assert!(d.contains("C 8 12 4 12 0 10"));
        assert!(d.ends_with('Z'));
    }

    #[test]
    fn push_num_strips_trailing_zeros() {
        let mut s = String::new();
        push_num(&mut s, 1.5);
        assert_eq!(s, "1.5");
        let mut s = String::new();
        push_num(&mut s, 1.0);
        assert_eq!(s, "1");
        let mut s = String::new();
        push_num(&mut s, -0.0);
        assert_eq!(s, "0");
    }

    #[test]
    fn push_num_clamps_precision() {
        // 0.123456 rounds to 0.123 at PRECISION = 3.
        let mut s = String::new();
        push_num(&mut s, 0.123_456);
        assert_eq!(s, "0.123");
    }

    #[test]
    fn bbox_includes_control_points() {
        let ops = [
            PathOp::MoveTo { x: 0.0, y: 0.0 },
            PathOp::QuadTo {
                cx: 100.0,
                cy: 200.0,
                x: 10.0,
                y: 0.0,
            },
            PathOp::Close,
        ];
        let (mnx, mny, mxx, mxy) = path_bbox(&ops).unwrap();
        assert_eq!(mnx, 0.0);
        assert_eq!(mny, 0.0);
        assert_eq!(mxx, 100.0);
        assert_eq!(mxy, 200.0);
    }

    #[test]
    fn empty_op_stream_has_no_bbox() {
        assert!(path_bbox(&[]).is_none());
    }

    #[test]
    fn render_outline_emits_well_formed_svg() {
        let svg = render_outline_svg("M 0 0 L 10 0 Z", (0.0, 0.0, 10.0, 10.0));
        assert!(svg.starts_with("<svg "));
        assert!(svg.ends_with("</svg>"));
        assert!(svg.contains("viewBox=\""));
        assert!(svg.contains("<path d=\"M 0 0 L 10 0 Z\""));
    }

    #[test]
    fn deterministic_output() {
        // Same input twice -> identical bytes.
        let svg1 = render_outline_svg("M 0 0 Z", (0.0, 0.0, 5.0, 5.0));
        let svg2 = render_outline_svg("M 0 0 Z", (0.0, 0.0, 5.0, 5.0));
        assert_eq!(svg1, svg2);
    }

    #[test]
    fn push_num_coerces_non_finite_to_zero() {
        // Issue #216: a pathological glyph outline (e.g. CFF charstring
        // whose blend evaluation overflows under extreme variation
        // coords) could leak NaN or infinity into the float formatter, which
        // round-trips them as the literal strings "NaN" / "inf" / "-inf"
        // (none of which is a valid SVG numeric token). The emitter
        // must coerce non-finite values to 0 so the document stays
        // well-formed.
        for &bad in &[f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut s = String::new();
            push_num(&mut s, bad);
            assert_eq!(s, "0", "non-finite {bad} leaked: {s:?}");
        }
    }

    #[test]
    fn push_num_keeps_huge_finite_values_finite() {
        // Scaling by 10^PRECISION overflows to infinity for values
        // above about 3.4e35. That used to leak "inf" into the output.
        // A COLRv1 glyph with nested PaintScale or PaintTransform
        // records reaches such values in its transform matrix.
        for v in [f32::MAX, -f32::MAX, 1.0e36] {
            let mut s = String::new();
            push_num(&mut s, v);
            assert!(!s.contains("inf"), "inf leaked for {v}: {s:?}");
            assert!(
                s.parse::<f32>().is_ok_and(|p| p == v),
                "{v} did not round-trip: {s:?}"
            );
        }
    }

    #[test]
    fn path_data_with_non_finite_coords_emits_only_finite_tokens() {
        let ops = [
            PathOp::MoveTo {
                x: f32::NAN,
                y: 0.0,
            },
            PathOp::LineTo {
                x: f32::INFINITY,
                y: f32::NEG_INFINITY,
            },
            PathOp::QuadTo {
                cx: f32::NAN,
                cy: 1.0,
                x: 2.0,
                y: 3.0,
            },
            PathOp::CubicTo {
                c1x: f32::INFINITY,
                c1y: 0.0,
                c2x: 0.0,
                c2y: 0.0,
                x: 0.0,
                y: 0.0,
            },
            PathOp::Close,
        ];
        let d = path_data(&ops);
        assert!(!d.contains("NaN"), "NaN leaked: {d:?}");
        assert!(!d.contains("inf"), "inf leaked: {d:?}");
        // The SVG must remain well-formed: verify only valid path
        // command letters and digits / spaces / minus / dot show up.
        for ch in d.chars() {
            assert!(
                ch.is_ascii_digit()
                    || ch.is_ascii_whitespace()
                    || matches!(ch, 'M' | 'L' | 'Q' | 'C' | 'Z' | '-' | '.'),
                "unexpected char {ch:?} in path data {d:?}"
            );
        }
    }
}
