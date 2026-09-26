//! Presentation attributes: attribute inheritance plus the numeric,
//! color and transform parsers it relies on.

use alloc::vec::Vec;

use crate::affine::Affine;

use super::dash::parse_dasharray;
use super::document::{parse_url_ref, ElemCtx, LineCap, LineJoin};
use super::xml::{attr_matches, Node};

/// Computes the inherited [`ElemCtx`] for `node`, given `parent`.
pub(super) fn inherit_attrs(parent: &ElemCtx, node: &Node) -> ElemCtx {
    let mut ctx = parent.clone();
    ctx.nesting = parent.nesting.saturating_add(1);
    // `color` sets this element's `currentColor` before its own paints
    // read it.
    if let Some(c) = node
        .attr("color")
        .and_then(|v| color_value(v, parent.current_color))
    {
        ctx.current_color = c;
    }
    for (k, v) in &node.attrs {
        if attr_matches(k, "transform") {
            if let Some(t) = parse_transform(v) {
                ctx.xform = ctx.xform.compose(&t);
            }
        } else if attr_matches(k, "fill") {
            if let Some(href) = parse_url_ref(v) {
                ctx.fill_grad_href = Some(href);
                ctx.fill_color = None;
            } else if v.trim().eq_ignore_ascii_case("none") {
                ctx.fill_color = Some([0, 0, 0, 0]);
                ctx.fill_grad_href = None;
            } else if let Some(c) = color_value(v, ctx.current_color) {
                ctx.fill_color = Some(c);
                ctx.fill_grad_href = None;
            }
        } else if attr_matches(k, "fill-opacity") {
            if let Some(o) = parse_opacity(v) {
                ctx.fill_opacity = (ctx.fill_opacity * o).clamp(0.0, 1.0);
            }
        } else if attr_matches(k, "opacity") {
            if let Some(o) = parse_opacity(v) {
                ctx.opacity = (ctx.opacity * o).clamp(0.0, 1.0);
            }
        } else if attr_matches(k, "stroke") {
            if v.trim().eq_ignore_ascii_case("none") {
                ctx.stroke_color = None;
            } else if let Some(c) = color_value(v, ctx.current_color) {
                ctx.stroke_color = Some(c);
            }
        } else if attr_matches(k, "stroke-opacity") {
            if let Some(o) = parse_opacity(v) {
                ctx.stroke_opacity = (ctx.stroke_opacity * o).clamp(0.0, 1.0);
            }
        } else if attr_matches(k, "stroke-width") {
            if let Some(w) = parse_length(v) {
                if w >= 0.0 {
                    ctx.stroke_width = w;
                }
            }
        } else if attr_matches(k, "stroke-linecap") {
            ctx.stroke_linecap = match v.trim().to_ascii_lowercase().as_str() {
                "round" => LineCap::Round,
                "square" => LineCap::Square,
                _ => LineCap::Butt,
            };
        } else if attr_matches(k, "stroke-linejoin") {
            ctx.stroke_linejoin = match v.trim().to_ascii_lowercase().as_str() {
                "round" => LineJoin::Round,
                "bevel" => LineJoin::Bevel,
                _ => LineJoin::Miter,
            };
        } else if attr_matches(k, "stroke-dasharray") {
            ctx.stroke_dasharray = parse_dasharray(v);
        } else if attr_matches(k, "stroke-dashoffset") {
            if let Some(o) = parse_length(v) {
                ctx.stroke_dashoffset = o;
            }
        } else if attr_matches(k, "clip-path") {
            if let Some(href) = parse_url_ref(v) {
                ctx.clip_href = Some(href);
            }
        } else if attr_matches(k, "filter") {
            if let Some(href) = parse_url_ref(v) {
                ctx.filter_href = Some(href);
            }
        } else if attr_matches(k, "mask") {
            if let Some(href) = parse_url_ref(v) {
                ctx.mask_href = Some(href);
            }
        }
    }
    ctx
}

// =========================================================================
// Numeric / color / transform parsing
// =========================================================================

pub(super) fn parse_viewbox(s: &str) -> Option<(f32, f32, f32, f32)> {
    let mut it = s.split(|c: char| c.is_ascii_whitespace() || c == ',');
    let x = it.find(|t| !t.is_empty())?.parse::<f32>().ok()?;
    let y = it.find(|t| !t.is_empty())?.parse::<f32>().ok()?;
    let w = it.find(|t| !t.is_empty())?.parse::<f32>().ok()?;
    let h = it.find(|t| !t.is_empty())?.parse::<f32>().ok()?;
    Some((x, y, w, h))
}

pub(super) fn parse_length(s: &str) -> Option<f32> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let cut = s
        .char_indices()
        .find(|(_, c)| {
            !(c.is_ascii_digit() || *c == '.' || *c == '-' || *c == '+' || *c == 'e' || *c == 'E')
        })
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    s[..cut].parse::<f32>().ok()
}

pub(super) fn parse_opacity(s: &str) -> Option<f32> {
    let v = s.trim().parse::<f32>().ok()?;
    Some(v.clamp(0.0, 1.0))
}

/// A paint color that may be `currentColor`, which resolves to
/// `current`.
pub(super) fn color_value(s: &str, current: [u8; 4]) -> Option<[u8; 4]> {
    if s.trim().eq_ignore_ascii_case("currentColor") {
        return Some(current);
    }
    parse_color(s)
}

pub(super) fn parse_color(s: &str) -> Option<[u8; 4]> {
    let s = s.trim();
    if s.eq_ignore_ascii_case("none") {
        return None;
    }
    match s.to_ascii_lowercase().as_str() {
        "black" => return Some([0, 0, 0, 255]),
        "white" => return Some([255, 255, 255, 255]),
        "red" => return Some([255, 0, 0, 255]),
        "green" => return Some([0, 128, 0, 255]),
        "blue" => return Some([0, 0, 255, 255]),
        _ => {}
    }
    if let Some(rest) = s.strip_prefix('#') {
        if rest.len() == 6 {
            // `get` rather than slicing: six bytes of non-ASCII text can
            // put a byte offset inside a character.
            let r = u8::from_str_radix(rest.get(0..2)?, 16).ok()?;
            let g = u8::from_str_radix(rest.get(2..4)?, 16).ok()?;
            let b = u8::from_str_radix(rest.get(4..6)?, 16).ok()?;
            return Some([r, g, b, 255]);
        }
        if rest.len() == 3 {
            let nyb = |c: char| -> Option<u8> {
                let mut tmp = [0u8; 4];
                let s = c.encode_utf8(&mut tmp);
                u8::from_str_radix(s, 16).ok()
            };
            let mut chars = rest.chars();
            let r = nyb(chars.next()?)?;
            let g = nyb(chars.next()?)?;
            let b = nyb(chars.next()?)?;
            return Some([r * 17, g * 17, b * 17, 255]);
        }
        return None;
    }
    if let Some(inner) = s
        .strip_prefix("rgb(")
        .or_else(|| s.strip_prefix("RGB("))
        .and_then(|x| x.strip_suffix(')'))
    {
        let mut it = inner.split(|c: char| c == ',' || c.is_ascii_whitespace());
        let r = it.find(|t| !t.is_empty())?.parse::<f32>().ok()? as i32;
        let g = it.find(|t| !t.is_empty())?.parse::<f32>().ok()? as i32;
        let b = it.find(|t| !t.is_empty())?.parse::<f32>().ok()? as i32;
        return Some([
            r.clamp(0, 255) as u8,
            g.clamp(0, 255) as u8,
            b.clamp(0, 255) as u8,
            255,
        ]);
    }
    None
}

pub(super) fn parse_transform(s: &str) -> Option<Affine> {
    let mut acc = Affine::identity();
    let mut rest = s.trim();
    while !rest.is_empty() {
        let lparen = rest.find('(')?;
        let rparen_off = rest[lparen..].find(')')?;
        let name = rest[..lparen].trim();
        let body = &rest[lparen + 1..lparen + rparen_off];
        rest = rest[lparen + rparen_off + 1..]
            .trim_start_matches(|c: char| c.is_ascii_whitespace() || c == ',');
        let nums: Vec<f32> = body
            .split(|c: char| c == ',' || c.is_ascii_whitespace())
            .filter(|t| !t.is_empty())
            .map(|t| t.parse::<f32>().unwrap_or(0.0))
            .collect();
        let m = match name.to_ascii_lowercase().as_str() {
            "translate" => match nums.len() {
                0 => continue,
                1 => Affine::translate(nums[0], 0.0),
                _ => Affine::translate(nums[0], nums[1]),
            },
            "scale" => match nums.len() {
                0 => continue,
                1 => Affine::scale(nums[0], nums[0]),
                _ => Affine::scale(nums[0], nums[1]),
            },
            "matrix" if nums.len() >= 6 => Affine {
                xx: nums[0],
                yx: nums[1],
                xy: nums[2],
                yy: nums[3],
                dx: nums[4],
                dy: nums[5],
            },
            "rotate" => {
                let rad = nums.first().copied().unwrap_or(0.0).to_radians();
                if nums.len() >= 3 {
                    let cx = nums[1];
                    let cy = nums[2];
                    Affine::translate(cx, cy)
                        .compose(&Affine::rotate(rad))
                        .compose(&Affine::translate(-cx, -cy))
                } else {
                    Affine::rotate(rad)
                }
            }
            _ => continue,
        };
        acc = acc.compose(&m);
    }
    Some(acc)
}
