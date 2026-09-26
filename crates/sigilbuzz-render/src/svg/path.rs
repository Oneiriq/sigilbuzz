//! Path geometry: shape primitives lowered to path ops, plus the `d`
//! path-data parser.

use alloc::vec::Vec;

use sigilbuzz::tables::PathOp;

use crate::error::RenderError;

use super::style::parse_length;
use super::xml::Node;

// =========================================================================
// Shape primitives -> path
// =========================================================================

pub(super) fn rect_to_path(node: &Node) -> Vec<PathOp> {
    let x = node.attr("x").and_then(parse_length).unwrap_or(0.0);
    let y = node.attr("y").and_then(parse_length).unwrap_or(0.0);
    let w = node.attr("width").and_then(parse_length).unwrap_or(0.0);
    let h = node.attr("height").and_then(parse_length).unwrap_or(0.0);
    if w <= 0.0 || h <= 0.0 {
        return Vec::new();
    }
    let rx_attr = node.attr("rx").and_then(parse_length);
    let ry_attr = node.attr("ry").and_then(parse_length);
    let rx = match (rx_attr, ry_attr) {
        (Some(rx), _) => rx,
        (None, Some(ry)) => ry,
        (None, None) => 0.0,
    };
    let ry = match (rx_attr, ry_attr) {
        (_, Some(ry)) => ry,
        (Some(rx), None) => rx,
        (None, None) => 0.0,
    };
    let rx = rx.max(0.0).min(w * 0.5);
    let ry = ry.max(0.0).min(h * 0.5);

    let mut ops = Vec::with_capacity(if rx > 0.0 || ry > 0.0 { 12 } else { 6 });
    if rx > 0.0 && ry > 0.0 {
        // Kappa for cubic-circle approximation of a quarter ellipse.
        const K: f32 = 0.552_284_8;
        let kx = rx * K;
        let ky = ry * K;
        // Top edge: start at (x+rx, y) and go to (x+w-rx, y).
        ops.push(PathOp::MoveTo { x: x + rx, y });
        ops.push(PathOp::LineTo { x: x + w - rx, y });
        // Top-right corner.
        ops.push(PathOp::CubicTo {
            c1x: x + w - rx + kx,
            c1y: y,
            c2x: x + w,
            c2y: y + ry - ky,
            x: x + w,
            y: y + ry,
        });
        // Right edge.
        ops.push(PathOp::LineTo {
            x: x + w,
            y: y + h - ry,
        });
        // Bottom-right corner.
        ops.push(PathOp::CubicTo {
            c1x: x + w,
            c1y: y + h - ry + ky,
            c2x: x + w - rx + kx,
            c2y: y + h,
            x: x + w - rx,
            y: y + h,
        });
        // Bottom edge.
        ops.push(PathOp::LineTo {
            x: x + rx,
            y: y + h,
        });
        // Bottom-left corner.
        ops.push(PathOp::CubicTo {
            c1x: x + rx - kx,
            c1y: y + h,
            c2x: x,
            c2y: y + h - ry + ky,
            x,
            y: y + h - ry,
        });
        // Left edge.
        ops.push(PathOp::LineTo { x, y: y + ry });
        // Top-left corner.
        ops.push(PathOp::CubicTo {
            c1x: x,
            c1y: y + ry - ky,
            c2x: x + rx - kx,
            c2y: y,
            x: x + rx,
            y,
        });
        ops.push(PathOp::Close);
    } else {
        ops.push(PathOp::MoveTo { x, y });
        ops.push(PathOp::LineTo { x: x + w, y });
        ops.push(PathOp::LineTo { x: x + w, y: y + h });
        ops.push(PathOp::LineTo { x, y: y + h });
        ops.push(PathOp::Close);
    }
    ops
}

pub(super) fn circle_to_path(node: &Node) -> Vec<PathOp> {
    let cx = node.attr("cx").and_then(parse_length).unwrap_or(0.0);
    let cy = node.attr("cy").and_then(parse_length).unwrap_or(0.0);
    let r = node.attr("r").and_then(parse_length).unwrap_or(0.0);
    if r <= 0.0 {
        return Vec::new();
    }
    ellipse_path(cx, cy, r, r)
}

pub(super) fn ellipse_to_path(node: &Node) -> Vec<PathOp> {
    let cx = node.attr("cx").and_then(parse_length).unwrap_or(0.0);
    let cy = node.attr("cy").and_then(parse_length).unwrap_or(0.0);
    let rx = node.attr("rx").and_then(parse_length).unwrap_or(0.0);
    let ry = node.attr("ry").and_then(parse_length).unwrap_or(0.0);
    if rx <= 0.0 || ry <= 0.0 {
        return Vec::new();
    }
    ellipse_path(cx, cy, rx, ry)
}

/// Approximates a centered ellipse with four cubic Béziers using the
/// standard kappa = 0.552_284_8. Drawing direction is clockwise (the
/// rasterizer's non-zero winding handles either, but we stay
/// consistent with `<rect>`).
fn ellipse_path(cx: f32, cy: f32, rx: f32, ry: f32) -> Vec<PathOp> {
    const K: f32 = 0.552_284_8;
    let kx = rx * K;
    let ky = ry * K;
    alloc::vec![
        PathOp::MoveTo { x: cx + rx, y: cy },
        PathOp::CubicTo {
            c1x: cx + rx,
            c1y: cy + ky,
            c2x: cx + kx,
            c2y: cy + ry,
            x: cx,
            y: cy + ry,
        },
        PathOp::CubicTo {
            c1x: cx - kx,
            c1y: cy + ry,
            c2x: cx - rx,
            c2y: cy + ky,
            x: cx - rx,
            y: cy,
        },
        PathOp::CubicTo {
            c1x: cx - rx,
            c1y: cy - ky,
            c2x: cx - kx,
            c2y: cy - ry,
            x: cx,
            y: cy - ry,
        },
        PathOp::CubicTo {
            c1x: cx + kx,
            c1y: cy - ry,
            c2x: cx + rx,
            c2y: cy - ky,
            x: cx + rx,
            y: cy,
        },
        PathOp::Close,
    ]
}

/// Parses an SVG `points="x1,y1 x2,y2 ..."` list. The grammar accepts
/// any mix of whitespace and commas as separators (per SVG 1.1
/// §9.7.1). Trailing odd coordinates (a stray "x" with no matching "y")
/// are dropped silently. That's what every browser does in practice.
pub(super) fn parse_points_list(s: &str) -> Vec<(f32, f32)> {
    let mut out: Vec<(f32, f32)> = Vec::new();
    let mut nums: Vec<f32> = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Skip separators: whitespace and commas.
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b',') {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let start = i;
        if bytes[i] == b'+' || bytes[i] == b'-' {
            i += 1;
        }
        let mut saw_digit = false;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
            saw_digit = true;
        }
        if i < bytes.len() && bytes[i] == b'.' {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
                saw_digit = true;
            }
        }
        if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
            i += 1;
            if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
                i += 1;
            }
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
        }
        if !saw_digit {
            // Bail out on unrecognized garbage; what's parsed so far
            // stays.
            break;
        }
        if let Ok(s) = core::str::from_utf8(&bytes[start..i]) {
            if let Ok(n) = s.parse::<f32>() {
                nums.push(n);
            }
        }
    }
    let mut k = 0;
    while k + 1 < nums.len() {
        out.push((nums[k], nums[k + 1]));
        k += 2;
    }
    out
}

/// `<polygon points="...">`: closed shape, `MoveTo + LineTo* + Close`.
pub(super) fn polygon_to_path(node: &Node) -> Vec<PathOp> {
    let pts = node
        .attr("points")
        .map(parse_points_list)
        .unwrap_or_default();
    if pts.len() < 2 {
        return Vec::new();
    }
    let mut ops = Vec::with_capacity(pts.len() + 1);
    ops.push(PathOp::MoveTo {
        x: pts[0].0,
        y: pts[0].1,
    });
    for p in &pts[1..] {
        ops.push(PathOp::LineTo { x: p.0, y: p.1 });
    }
    ops.push(PathOp::Close);
    ops
}

/// `<polyline points="...">`: open shape, `MoveTo + LineTo*` (no Close).
pub(super) fn polyline_to_path(node: &Node) -> Vec<PathOp> {
    let pts = node
        .attr("points")
        .map(parse_points_list)
        .unwrap_or_default();
    if pts.len() < 2 {
        return Vec::new();
    }
    let mut ops = Vec::with_capacity(pts.len());
    ops.push(PathOp::MoveTo {
        x: pts[0].0,
        y: pts[0].1,
    });
    for p in &pts[1..] {
        ops.push(PathOp::LineTo { x: p.0, y: p.1 });
    }
    ops
}

/// `<line x1 y1 x2 y2>`: a single segment, `MoveTo + LineTo`.
pub(super) fn line_to_path(node: &Node) -> Vec<PathOp> {
    let x1 = node.attr("x1").and_then(parse_length).unwrap_or(0.0);
    let y1 = node.attr("y1").and_then(parse_length).unwrap_or(0.0);
    let x2 = node.attr("x2").and_then(parse_length).unwrap_or(0.0);
    let y2 = node.attr("y2").and_then(parse_length).unwrap_or(0.0);
    if (x1 - x2).abs() < 1e-6 && (y1 - y2).abs() < 1e-6 {
        return Vec::new();
    }
    alloc::vec![
        PathOp::MoveTo { x: x1, y: y1 },
        PathOp::LineTo { x: x2, y: y2 },
    ]
}

// =========================================================================
// Path-data parser (M/L/H/V/C/Q/Z)
// =========================================================================

pub(super) fn parse_path_d(s: &str) -> Result<Vec<PathOp>, RenderError> {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut cx = 0.0_f32;
    let mut cy = 0.0_f32;
    let mut sx = 0.0_f32;
    let mut sy = 0.0_f32;
    let mut have_sub = false;
    let mut last_cmd: Option<u8> = None;
    while i < bytes.len() {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b',') {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let c = bytes[i];
        let cmd = if c.is_ascii_alphabetic() {
            i += 1;
            last_cmd = Some(c);
            c
        } else {
            match last_cmd {
                Some(b'M') => b'L',
                Some(b'm') => b'l',
                // Closepath takes no arguments, so a number after it is
                // an error. Repeating it would consume nothing and loop
                // forever.
                Some(b'Z' | b'z') | None => return Err(RenderError::Parse("svg path d")),
                Some(prev) => prev,
            }
        };
        match cmd {
            b'M' | b'm' => {
                let (x, y) = read_pair(bytes, &mut i)?;
                let (ax, ay) = if cmd == b'M' {
                    (x, y)
                } else {
                    (cx + x, cy + y)
                };
                cx = ax;
                cy = ay;
                sx = ax;
                sy = ay;
                have_sub = true;
                out.push(PathOp::MoveTo { x: ax, y: ay });
            }
            b'L' | b'l' => {
                let (x, y) = read_pair(bytes, &mut i)?;
                let (ax, ay) = if cmd == b'L' {
                    (x, y)
                } else {
                    (cx + x, cy + y)
                };
                cx = ax;
                cy = ay;
                out.push(PathOp::LineTo { x: ax, y: ay });
            }
            b'H' | b'h' => {
                let x = read_num(bytes, &mut i)?;
                let ax = if cmd == b'H' { x } else { cx + x };
                cx = ax;
                out.push(PathOp::LineTo { x: ax, y: cy });
            }
            b'V' | b'v' => {
                let y = read_num(bytes, &mut i)?;
                let ay = if cmd == b'V' { y } else { cy + y };
                cy = ay;
                out.push(PathOp::LineTo { x: cx, y: ay });
            }
            b'C' | b'c' => {
                let (x1, y1) = read_pair(bytes, &mut i)?;
                let (x2, y2) = read_pair(bytes, &mut i)?;
                let (x, y) = read_pair(bytes, &mut i)?;
                let (a1x, a1y, a2x, a2y, ax, ay) = if cmd == b'C' {
                    (x1, y1, x2, y2, x, y)
                } else {
                    (cx + x1, cy + y1, cx + x2, cy + y2, cx + x, cy + y)
                };
                out.push(PathOp::CubicTo {
                    c1x: a1x,
                    c1y: a1y,
                    c2x: a2x,
                    c2y: a2y,
                    x: ax,
                    y: ay,
                });
                cx = ax;
                cy = ay;
            }
            b'Q' | b'q' => {
                let (x1, y1) = read_pair(bytes, &mut i)?;
                let (x, y) = read_pair(bytes, &mut i)?;
                let (a1x, a1y, ax, ay) = if cmd == b'Q' {
                    (x1, y1, x, y)
                } else {
                    (cx + x1, cy + y1, cx + x, cy + y)
                };
                out.push(PathOp::QuadTo {
                    cx: a1x,
                    cy: a1y,
                    x: ax,
                    y: ay,
                });
                cx = ax;
                cy = ay;
            }
            b'Z' | b'z' => {
                if have_sub {
                    out.push(PathOp::Close);
                    cx = sx;
                    cy = sy;
                }
            }
            _ => {
                return Err(RenderError::Parse("svg path d"));
            }
        }
    }
    Ok(out)
}

fn read_pair(bytes: &[u8], i: &mut usize) -> Result<(f32, f32), RenderError> {
    let x = read_num(bytes, i)?;
    let y = read_num(bytes, i)?;
    Ok((x, y))
}

fn read_num(bytes: &[u8], i: &mut usize) -> Result<f32, RenderError> {
    while *i < bytes.len() && (bytes[*i].is_ascii_whitespace() || bytes[*i] == b',') {
        *i += 1;
    }
    let start = *i;
    if *i < bytes.len() && (bytes[*i] == b'+' || bytes[*i] == b'-') {
        *i += 1;
    }
    let mut saw_digit = false;
    while *i < bytes.len() && bytes[*i].is_ascii_digit() {
        *i += 1;
        saw_digit = true;
    }
    if *i < bytes.len() && bytes[*i] == b'.' {
        *i += 1;
        while *i < bytes.len() && bytes[*i].is_ascii_digit() {
            *i += 1;
            saw_digit = true;
        }
    }
    if *i < bytes.len() && (bytes[*i] == b'e' || bytes[*i] == b'E') {
        *i += 1;
        if *i < bytes.len() && (bytes[*i] == b'+' || bytes[*i] == b'-') {
            *i += 1;
        }
        while *i < bytes.len() && bytes[*i].is_ascii_digit() {
            *i += 1;
        }
    }
    if !saw_digit {
        return Err(RenderError::Parse("svg path d"));
    }
    let s =
        core::str::from_utf8(&bytes[start..*i]).map_err(|_| RenderError::Parse("svg path d"))?;
    s.parse::<f32>()
        .map_err(|_| RenderError::Parse("svg path d"))
}
