//! Type 1 CharString number encoding and PathOp emission.
//!
//! Type 1 charstrings (Adobe Type 1 Font Format, "Black Book", chapter
//! 6) are a stream of tagged bytes. Numbers are pushed onto an
//! operand stack with a single-byte form for small integers and
//! multi-byte forms for larger values; operators consume those
//! operands and emit drawing commands.
//!
//! # Number encoding
//!
//! - `-107..=107`  -> one byte: `v + 139`. So `0` is `139`, `-107` is
//!   `32`, `107` is `246`.
//! - `108..=1131`  -> two bytes: `[247..=250, w]` where the value is
//!   `(hi - 247) * 256 + lo + 108`.
//! - `-1131..=-108` -> two bytes: `[251..=254, w]` where the value is
//!   `-((hi - 251) * 256 + lo + 108)`.
//! - anything outside `-1131..=1131` -> five-byte form: `255`
//!   followed by a 32-bit big-endian two's-complement integer.
//!
//! Negatives are handled symmetrically with positives: the small
//! single-byte range is signed (-107..=107 around 139); the two-byte
//! "large positive" form `247..=250` has a mirror "large negative"
//! form `251..=254` whose value is the negation of the same bias
//! formula. Anything outside the two-byte range falls through to the
//! five-byte form, which is a straight 32-bit two's-complement load.
//!
//! # Operators we emit
//!
//! | Type 1 op   | Tag | Use                                  |
//! |-------------|-----|--------------------------------------|
//! | `hsbw`      |  13 | Sidebearing + advance width          |
//! | `vmoveto`   |   4 | dy moveto (when dx == 0)             |
//! | `hmoveto`   |  22 | dx moveto (when dy == 0)             |
//! | `rmoveto`   |  21 | dx dy moveto                         |
//! | `vlineto`   |   7 | dy lineto                            |
//! | `hlineto`   |   6 | dx lineto                            |
//! | `rlineto`   |   5 | dx dy lineto                         |
//! | `rrcurveto` |   8 | dx1 dy1 dx2 dy2 dx3 dy3 cubic         |
//! | `closepath` |   9 | Close current contour                |
//! | `endchar`   |  14 | Finish charstring                    |
//!
//! Type 1 paths are *relative*: every move/line/curve operand is the
//! delta from the current pen, not the absolute target. The emitter
//! tracks the pen and converts sigilbuzz's absolute [`PathOp`] values
//! to deltas. Curve emission always uses `rrcurveto` (8). The H/V
//! variants exist as size optimizations but require additional flat
//! tangent checks. The emitter does not use them. It favors simple,
//! readable charstrings over the smallest encoding.

use alloc::vec::Vec;

use sigilbuzz::tables::PathOp;

/// Encode a signed integer onto the operand stack using the smallest
/// Type 1 number form that fits.
///
/// Type 1 charstrings have no native float type. Sub-unit precision
/// is expressed via the `div` operator (12 12). For PDF font use we
/// follow the convention of rounding to the nearest integer in
/// glyph-design-unit space; sub-unit drift is invisible at typical
/// PDF viewing scales and avoids dragging the `div` machinery into
/// the emitter for negligible gain.
pub fn encode_number(out: &mut Vec<u8>, value: i32) {
    if (-107..=107).contains(&value) {
        // Single-byte form, biased by 139.
        out.push((value + 139) as u8);
    } else if (108..=1131).contains(&value) {
        // Two-byte large positive: 247..=250 and a low byte.
        let v = value - 108;
        let hi = 247 + (v / 256) as u8;
        let lo = (v % 256) as u8;
        out.push(hi);
        out.push(lo);
    } else if (-1131..=-108).contains(&value) {
        // Two-byte large negative: mirror of the large-positive form.
        let v = -value - 108;
        let hi = 251 + (v / 256) as u8;
        let lo = (v % 256) as u8;
        out.push(hi);
        out.push(lo);
    } else {
        // Five-byte form: 255 followed by a 32-bit big-endian two's
        // complement integer.
        out.push(255);
        out.extend_from_slice(&value.to_be_bytes());
    }
}

/// Round an `f32` glyph coordinate to the nearest `i32` for Type 1
/// emission. Subunit precision is intentionally discarded. See
/// [`encode_number`] for why.
///
/// The final `as` cast saturates: NaN becomes 0 and values beyond the
/// `i32` range clamp to `i32::MIN` or `i32::MAX`.
fn round_i32(v: f32) -> i32 {
    // round_ties_even would be marginally nicer but is unstable in
    // no_std without a feature flag; plain `round` is good enough at
    // glyph-unit scale.
    let r = if v >= 0.0 { v + 0.5 } else { v - 0.5 };
    r as i32
}

/// `hsbw` (op 13): set the left sidebearing and advance width for
/// the glyph.
///
/// Type 1 requires every charstring to begin with a width-setting
/// operator. `hsbw lsb wx` declares horizontal sidebearing `lsb` and
/// horizontal advance `wx`; vertical writing modes use `sbw` (12 7),
/// which the PDF Type 1 surface here doesn't need.
pub fn emit_hsbw(out: &mut Vec<u8>, lsb: i32, advance: i32) {
    encode_number(out, lsb);
    encode_number(out, advance);
    out.push(13);
}

/// `endchar` (op 14): terminate the charstring.
pub fn emit_endchar(out: &mut Vec<u8>) {
    out.push(14);
}

/// Emit Type 1 charstring bytes for an absolute-coordinate PathOp
/// slice, relative-encoded against a pen that starts at the origin
/// (the implicit position after `hsbw`).
///
/// The caller is responsible for emitting `hsbw` *before* this
/// function and `endchar` *after*. This split keeps the per-glyph
/// width and sidebearing decisions out of the path-conversion step.
///
/// Path conversion rules:
///
/// - `MoveTo` collapses to `hmoveto` if `dy == 0`, `vmoveto` if
///   `dx == 0`, otherwise `rmoveto`.
/// - `LineTo` uses the same H / V / R selection.
/// - `QuadTo` is degree-elevated to a cubic with the same formula
///   the Type 3 emitter uses, then emitted as `rrcurveto`.
/// - `CubicTo` is emitted directly as `rrcurveto`.
/// - `Close` is emitted as `closepath`.
pub fn emit_path_ops(out: &mut Vec<u8>, ops: &[PathOp]) {
    let mut pen_x = 0.0_f32;
    let mut pen_y = 0.0_f32;
    for op in ops {
        match *op {
            PathOp::MoveTo { x, y } => {
                emit_move(out, x - pen_x, y - pen_y);
                pen_x = x;
                pen_y = y;
            }
            PathOp::LineTo { x, y } => {
                emit_line(out, x - pen_x, y - pen_y);
                pen_x = x;
                pen_y = y;
            }
            PathOp::QuadTo { cx, cy, x, y } => {
                let two_thirds = 2.0_f32 / 3.0_f32;
                let c1x = pen_x + two_thirds * (cx - pen_x);
                let c1y = pen_y + two_thirds * (cy - pen_y);
                let c2x = x + two_thirds * (cx - x);
                let c2y = y + two_thirds * (cy - y);
                emit_rrcurveto(
                    out,
                    c1x - pen_x,
                    c1y - pen_y,
                    c2x - c1x,
                    c2y - c1y,
                    x - c2x,
                    y - c2y,
                );
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
                emit_rrcurveto(
                    out,
                    c1x - pen_x,
                    c1y - pen_y,
                    c2x - c1x,
                    c2y - c1y,
                    x - c2x,
                    y - c2y,
                );
                pen_x = x;
                pen_y = y;
            }
            PathOp::Close => {
                // Emit closepath. Pen is implicitly back at the
                // contour start, but sigilbuzz outlines always
                // re-open with a MoveTo so we leave the tracked pen
                // alone. The next op's delta is computed against
                // where we last were and a following MoveTo will
                // overwrite both.
                out.push(9);
            }
        }
    }
}

fn emit_move(out: &mut Vec<u8>, dx: f32, dy: f32) {
    let idx = round_i32(dx);
    let idy = round_i32(dy);
    if idx == 0 {
        encode_number(out, idy);
        out.push(4); // vmoveto
    } else if idy == 0 {
        encode_number(out, idx);
        out.push(22); // hmoveto
    } else {
        encode_number(out, idx);
        encode_number(out, idy);
        out.push(21); // rmoveto
    }
}

fn emit_line(out: &mut Vec<u8>, dx: f32, dy: f32) {
    let idx = round_i32(dx);
    let idy = round_i32(dy);
    if idx == 0 {
        encode_number(out, idy);
        out.push(7); // vlineto
    } else if idy == 0 {
        encode_number(out, idx);
        out.push(6); // hlineto
    } else {
        encode_number(out, idx);
        encode_number(out, idy);
        out.push(5); // rlineto
    }
}

fn emit_rrcurveto(out: &mut Vec<u8>, dx1: f32, dy1: f32, dx2: f32, dy2: f32, dx3: f32, dy3: f32) {
    encode_number(out, round_i32(dx1));
    encode_number(out, round_i32(dy1));
    encode_number(out, round_i32(dx2));
    encode_number(out, round_i32(dy2));
    encode_number(out, round_i32(dx3));
    encode_number(out, round_i32(dy3));
    out.push(8); // rrcurveto
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_positive_uses_single_byte() {
        // 0 -> 139, 100 -> 239, 107 -> 246.
        let mut out = Vec::new();
        encode_number(&mut out, 0);
        encode_number(&mut out, 100);
        encode_number(&mut out, 107);
        assert_eq!(out, [139, 239, 246]);
    }

    #[test]
    fn small_negative_uses_single_byte() {
        // -107 -> 32, -1 -> 138.
        let mut out = Vec::new();
        encode_number(&mut out, -107);
        encode_number(&mut out, -1);
        assert_eq!(out, [32, 138]);
    }

    #[test]
    fn medium_positive_uses_two_bytes() {
        // 108 -> 247 0, 1131 -> 250 255.
        let mut out = Vec::new();
        encode_number(&mut out, 108);
        assert_eq!(out, [247, 0]);
        out.clear();
        encode_number(&mut out, 1131);
        assert_eq!(out, [250, 255]);
        out.clear();
        // 500 - 108 = 392; 392 / 256 = 1, 392 % 256 = 136. So
        // hi = 247 + 1 = 248, lo = 136.
        encode_number(&mut out, 500);
        assert_eq!(out, [248, 136]);
    }

    #[test]
    fn medium_negative_uses_two_bytes() {
        let mut out = Vec::new();
        encode_number(&mut out, -108);
        assert_eq!(out, [251, 0]);
        out.clear();
        encode_number(&mut out, -1131);
        assert_eq!(out, [254, 255]);
    }

    #[test]
    fn large_value_uses_five_bytes() {
        // 5000 is outside [-1131, 1131], must fall through to the
        // 5-byte form: 255 followed by big-endian 5000.
        let mut out = Vec::new();
        encode_number(&mut out, 5000);
        assert_eq!(out, [255, 0x00, 0x00, 0x13, 0x88]);
        out.clear();
        encode_number(&mut out, -5000);
        // -5000 in i32 BE is 0xFFFFEC78.
        assert_eq!(out, [255, 0xFF, 0xFF, 0xEC, 0x78]);
    }

    #[test]
    fn pathops_roundtrip_through_charstring() {
        // Walk a small shape that hits every emitter branch.
        let ops = [
            PathOp::MoveTo { x: 100.0, y: 0.0 },
            PathOp::LineTo { x: 200.0, y: 0.0 },
            PathOp::LineTo { x: 200.0, y: 50.0 },
            PathOp::QuadTo {
                cx: 200.0,
                cy: 100.0,
                x: 150.0,
                y: 100.0,
            },
            PathOp::CubicTo {
                c1x: 100.0,
                c1y: 100.0,
                c2x: 100.0,
                c2y: 50.0,
                x: 100.0,
                y: 0.0,
            },
            PathOp::Close,
        ];
        let mut out = Vec::new();
        emit_path_ops(&mut out, &ops);

        // Verify by walking expected bytes. Comments document the
        // delta and op for each block:
        //   MoveTo 100,0  -> dy=0  hmoveto:  239, 22
        //   LineTo 200,0  -> dx=100 dy=0    hlineto: 239, 6
        //   LineTo 200,50 -> dx=0 dy=50     vlineto: 189, 7
        //   QuadTo (c=(200,100), p2=(150,100)) from p0=(200,50)
        //     c1 = (200,83.33) rounded (200,83); c2 = (183.33,100) rounded (183,100)
        //     deltas: (0, 33) (-17, 17) (-33, 0)  rrcurveto = op8
        //   CubicTo from (150,100) -> c1(100,100) c2(100,50) end(100,0)
        //     deltas: (-50, 0) (0, -50) (0, -50)  rrcurveto = op8
        //   Close -> op9
        let expected: [u8; 21] = [
            239, 22, // hmoveto 100
            239, 6, // hlineto 100
            189, 7, // vlineto 50
            139, 172, 122, 156, 106, 139, 8, // rrcurveto for the quad
            89, 139, 139, 89, 139, 89, 8, // rrcurveto for the cubic
            9, // closepath
        ];

        assert_eq!(out, expected);
    }

    #[test]
    fn hsbw_emits_lsb_then_advance_then_op() {
        let mut out = Vec::new();
        emit_hsbw(&mut out, 50, 500);
        // 50 -> 189, 500 -> [248, 136] (see medium-positive test), op 13.
        assert_eq!(out, [189, 248, 136, 13]);
    }

    #[test]
    fn endchar_is_single_byte_14() {
        let mut out = Vec::new();
        emit_endchar(&mut out);
        assert_eq!(out, [14]);
    }
}
