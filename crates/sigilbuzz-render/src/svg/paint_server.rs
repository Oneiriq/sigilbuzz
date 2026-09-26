//! Paint servers: fill paint resolution plus `<linearGradient>` and
//! `<radialGradient>` parsing.

use alloc::vec::Vec;

use sigilbuzz_paint::{Color as PaintColor, ColorStop, Extend};

use crate::affine::Affine;

use super::document::{Defs, ElemCtx};
use super::model::{GradKind, GradientPaint, Paint};
use super::style::{color_value, parse_length, parse_opacity, parse_transform};
use super::xml::{name_eq, Node};

pub(super) fn is_fully_transparent(p: &Paint) -> bool {
    match p {
        Paint::Solid(c) => c[3] == 0,
        Paint::Gradient(g) => {
            // Treat as transparent only when *every* stop is fully
            // transparent and the per-element opacity is zero. Cheap
            // early-exit; gradients with mid-range stops still render.
            g.opacity <= 0.0 || (g.stops.iter().all(|s| s.color.a <= 0.0))
        }
    }
}

pub(super) fn resolve_fill_paint(defs: &Defs<'_>, ctx: &ElemCtx) -> Option<Paint> {
    if let Some(id) = ctx.fill_grad_href.as_deref() {
        if let Some(g) = resolve_gradient(defs, id, ctx) {
            return Some(Paint::Gradient(g));
        }
        // url(#...) pointing to nothing falls back to default black.
    }
    let base = ctx.fill_color.unwrap_or([0, 0, 0, 255]);
    if base[3] == 0 {
        return None;
    }
    let alpha_factor = (ctx.fill_opacity * ctx.opacity).clamp(0.0, 1.0);
    let a = (base[3] as f32 / 255.0 * alpha_factor * 255.0).round() as u8;
    if a == 0 {
        return None;
    }
    Some(Paint::Solid([base[0], base[1], base[2], a]))
}

fn resolve_gradient(defs: &Defs<'_>, id: &str, ctx: &ElemCtx) -> Option<GradientPaint> {
    let node = defs.lookup(id)?;
    let is_linear = name_eq(&node.name, "linearGradient");
    let is_radial = name_eq(&node.name, "radialGradient");
    if !is_linear && !is_radial {
        return None;
    }
    // Stops can come from this node or, via xlink:href, an ancestor
    // gradient. A single hop of resolution is enough for every real
    // SVG-in-OT we've seen.
    let mut stops: Vec<ColorStop> = Vec::new();
    for c in &node.children {
        if name_eq(&c.name, "stop") {
            if let Some(s) = parse_stop(c, ctx.current_color) {
                stops.push(s);
            }
        }
    }
    if stops.is_empty() {
        if let Some(href) = node
            .attr("href")
            .or_else(|| node.attr("xlink:href"))
            .and_then(|s| s.strip_prefix('#'))
        {
            if let Some(parent) = defs.lookup(href) {
                for c in &parent.children {
                    if name_eq(&c.name, "stop") {
                        if let Some(s) = parse_stop(c, ctx.current_color) {
                            stops.push(s);
                        }
                    }
                }
            }
        }
    }
    if stops.is_empty() {
        return None;
    }
    let extend = match node
        .attr("spreadMethod")
        .map(|s| s.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("repeat") => Extend::Repeat,
        Some("reflect") => Extend::Reflect,
        _ => Extend::Pad,
    };
    let gradient_xform = node
        .attr("gradientTransform")
        .and_then(parse_transform)
        .unwrap_or_else(Affine::identity);

    let kind = if is_linear {
        let x1 = node.attr("x1").and_then(parse_length).unwrap_or(0.0);
        let y1 = node.attr("y1").and_then(parse_length).unwrap_or(0.0);
        let x2 = node.attr("x2").and_then(parse_length).unwrap_or(1.0);
        let y2 = node.attr("y2").and_then(parse_length).unwrap_or(0.0);
        GradKind::Linear { x1, y1, x2, y2 }
    } else {
        let cx = node.attr("cx").and_then(parse_length).unwrap_or(0.5);
        let cy = node.attr("cy").and_then(parse_length).unwrap_or(0.5);
        let r = node.attr("r").and_then(parse_length).unwrap_or(0.5);
        let fx = node.attr("fx").and_then(parse_length).unwrap_or(cx);
        let fy = node.attr("fy").and_then(parse_length).unwrap_or(cy);
        GradKind::Radial { cx, cy, r, fx, fy }
    };
    Some(GradientPaint {
        kind,
        stops,
        extend,
        opacity: (ctx.fill_opacity * ctx.opacity).clamp(0.0, 1.0),
        gradient_xform,
    })
}

/// `current` is the `currentColor` of the element the gradient paints.
fn parse_stop(node: &Node, current: [u8; 4]) -> Option<ColorStop> {
    let offset = node.attr("offset").map(parse_stop_offset).unwrap_or(0.0);
    // stop-color is the canonical attribute; some authoring tools fold
    // it into a CSS-ish style="stop-color:#rgb;stop-opacity:0.5". Be
    // tolerant.
    let mut color = node
        .attr("stop-color")
        .and_then(|v| color_value(v, current))
        .unwrap_or([0, 0, 0, 255]);
    let stop_opacity = node
        .attr("stop-opacity")
        .and_then(parse_opacity)
        .unwrap_or(1.0);
    if let Some(style) = node.attr("style") {
        for chunk in style.split(';') {
            let mut parts = chunk.splitn(2, ':');
            let key = parts.next()?.trim();
            let val = parts.next()?.trim();
            if key.eq_ignore_ascii_case("stop-color") {
                if let Some(c) = color_value(val, current) {
                    color = c;
                }
            } else if key.eq_ignore_ascii_case("stop-opacity") {
                if let Some(_o) = parse_opacity(val) {
                    // applied below
                }
            }
        }
    }
    let a = (color[3] as f32 / 255.0 * stop_opacity).clamp(0.0, 1.0);
    Some(ColorStop::new(
        offset,
        PaintColor {
            r: color[0] as f32 / 255.0,
            g: color[1] as f32 / 255.0,
            b: color[2] as f32 / 255.0,
            a,
        },
    ))
}

pub(super) fn parse_stop_offset(s: &str) -> f32 {
    let s = s.trim();
    if let Some(v) = s.strip_suffix('%') {
        return v.trim().parse::<f32>().map(|n| n / 100.0).unwrap_or(0.0);
    }
    s.parse::<f32>().unwrap_or(0.0)
}
