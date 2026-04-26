//! SVG-in-OT rasterization.
//!
//! The OpenType `SVG ` table maps glyph ids to inline SVG documents
//! (Apple/Adobe extension supported by HarfBuzz). sigilbuzz exposes the
//! raw payload bytes via [`sigilbuzz::Face::svg_document`]; this module
//! turns that XML into a [`crate::ColorPixmap`].
//!
//! ## Bounded subset
//!
//! Real-world SVG-in-OT documents (Twitter Color Emoji, Mozilla Firefox
//! OS Emoji, designer fonts produced by `nanoemoji` / `fonttools`) use
//! a small fraction of the full SVG 1.1 grammar. We deliberately
//! implement only that fraction:
//!
//! - `<svg>` with `viewBox` / `width` / `height` attributes.
//! - `<g>` with optional `transform=` (the `translate(...)`,
//!   `scale(...)`, `rotate(...)`, and `matrix(...)` forms).
//! - `<path>` with `d=` containing M/L/H/V/C/Q/Z commands and their
//!   relative variants.
//! - `fill="#RRGGBB"`, `fill="#RGB"`, `fill="rgb(...)"`, named colours
//!   (`black` / `white` / `red` / `green` / `blue`), and `fill="none"`.
//!   `fill-opacity="..."` modulates alpha.
//!
//! Anything outside that list — strokes, gradients, filters, `<use>`,
//! animations, `clipPath`, masks — is ignored. The parser tolerates
//! them (skipping the offending element/attribute) so a font that
//! includes a `<linearGradient>` for one glyph still renders the
//! others correctly.
//!
//! ## Pipeline
//!
//! ```text
//!   Face.svg_document(gid)         → SvgDocument { data, gzipped }
//!     │
//!     │ gzipped → RenderError::SvgGzipped (no gzip dep here)
//!     ▼
//!   parse_document(xml)            → SvgDoc { viewbox, fills }
//!     │
//!     │ each fill: { ops: PathOp[], color: [u8;4], local_xform }
//!     ▼
//!   for each fill:
//!     flatten(ops × world_xform)   → Segment[]
//!     raster(segments)             → Pixmap (alpha mask)
//!     blit(mask × color → out)     → ColorPixmap
//! ```
//!
//! No XML library on the read path — the parser is a hand-rolled tag
//! walker. Coordinates are decimal numbers parsed with `f32::from_str`.

use alloc::vec::Vec;

use sigilbuzz::tables::PathOp;
use sigilbuzz::Face;

use crate::affine::Affine;
use crate::error::RenderError;
use crate::flatten::flatten;
use crate::pixmap::{ColorPixmap, Pixmap};
use crate::raster::rasterize as raster;
use crate::rasterizer::Rasterizer;

/// Maximum recursion depth for nested `<g>` elements. Real fonts stay
/// under 4; we cap at 32 to keep malicious payloads from blowing the
/// stack.
const MAX_GROUP_DEPTH: u32 = 32;

/// Maximum number of fills a single document may emit. Same rationale
/// as `MAX_GROUP_DEPTH`: a hostile SVG could otherwise expand to
/// gigabytes of work. 4096 is well above what real fonts produce.
const MAX_FILLS: usize = 4096;

/// One filled sub-path collected from the document. `ops` is in the
/// document's intrinsic coordinate space — the rasterizer composes the
/// SVG-to-pixel transform on top.
#[derive(Debug, Clone)]
struct Fill {
    ops: Vec<PathOp>,
    color: [u8; 4],
    /// Composed transform from the element's nested `<g transform=...>`
    /// stack, in document coordinates. The world transform (document →
    /// pixel) is applied on top at rasterize time.
    xform: Affine,
}

/// Parsed SVG document metadata.
#[derive(Debug, Clone)]
struct SvgDoc {
    /// Width / height of the document's coordinate box, taken from
    /// `viewBox` if present, then `width` / `height`, defaulting to
    /// 1000 if neither is given.
    view_w: f32,
    view_h: f32,
    /// `viewBox` origin (x, y). Defaults to (0, 0).
    view_x: f32,
    view_y: f32,
    /// Collected fills in document order (back-to-front paint order).
    fills: Vec<Fill>,
}

impl Rasterizer {
    /// Rasterizes the SVG document for `gid` from the font's `SVG`
    /// table, returning a premultiplied RGBA [`ColorPixmap`].
    ///
    /// `size_pt` is the rendering size in pixels — the SVG document's
    /// viewBox is mapped onto a `size_pt × size_pt` square. If the
    /// viewBox is non-square, the rendered bitmap preserves the
    /// document's aspect ratio (the longer axis maps to `size_pt`).
    ///
    /// `coords` is accepted for API symmetry with
    /// [`Self::rasterize_glyph`] and [`Self::rasterize_colrv0_glyph`]
    /// but currently has no effect — SVG-in-OT documents are static
    /// (no axis tagging), and HarfBuzz / CoreText behave the same way.
    ///
    /// # Errors
    /// - [`RenderError::SvgNotFound`] when `gid` has no SVG record.
    /// - [`RenderError::SvgGzipped`] when the payload is gzip-compressed
    ///   (sigilbuzz-render does not ship a gzip dep; consumers should
    ///   decompress and feed back via a future bytes-based entry point).
    /// - [`RenderError::Parse`] for unrecoverable XML / path-data
    ///   errors.
    /// - [`RenderError::BadSize`] when `size_pt` is non-finite or
    ///   non-positive.
    pub fn rasterize_svg_glyph(
        &self,
        face: &Face<'_>,
        gid: u16,
        size_pt: f32,
        _coords: &[f32],
    ) -> Result<ColorPixmap, RenderError> {
        if !size_pt.is_finite() || size_pt <= 0.0 {
            return Err(RenderError::BadSize(size_pt));
        }
        let doc_record = face
            .svg_document(gid)
            .map_err(|_| RenderError::Parse("svg"))?
            .ok_or(RenderError::SvgNotFound(gid))?;
        if doc_record.gzipped {
            return Err(RenderError::SvgGzipped);
        }
        let xml = core::str::from_utf8(doc_record.data).map_err(|_| RenderError::Parse("svg"))?;
        let doc = parse_document(xml)?;

        if doc.view_w <= 0.0 || doc.view_h <= 0.0 {
            return Err(RenderError::Parse("svg viewBox"));
        }
        // Map document → pixel space: scale the viewBox onto a
        // size_pt × size_pt square, preserving aspect ratio. SVG y
        // points down (same as bitmap), so no Y flip — unlike the
        // outline renderer, which flips OpenType design-units.
        let s = (size_pt / doc.view_w).min(size_pt / doc.view_h);
        let world = Affine {
            xx: s,
            yx: 0.0,
            xy: 0.0,
            yy: s,
            dx: -doc.view_x * s,
            dy: -doc.view_y * s,
        };

        let width = (doc.view_w * s).round().max(1.0) as u32;
        let height = (doc.view_h * s).round().max(1.0) as u32;
        let mut out = ColorPixmap::new(width, height);

        let tol = self.flattening_tolerance();
        for fill in &doc.fills {
            let xf = world.compose(&fill.xform);
            let segs = flatten(fill.ops.iter().copied(), &xf, tol);
            if segs.is_empty() {
                continue;
            }
            let mask = raster(&segs);
            if mask.pixmap.is_empty() {
                continue;
            }
            blit(
                &mut out,
                &mask.pixmap,
                mask.origin_x,
                mask.origin_y,
                fill.color,
            );
        }
        Ok(out)
    }
}

// =========================================================================
// Origin-aware blit
// =========================================================================

/// Blits `mask × color` into `dst`, where `(ox, oy)` is the device-space
/// origin of the mask. Pixels outside the destination are clipped.
/// Source-over with premultiplied destination, matching `colrv0`'s
/// `blit_layer`.
fn blit(dst: &mut ColorPixmap, mask: &Pixmap, ox: i32, oy: i32, color: [u8; 4]) {
    if dst.is_empty() || mask.is_empty() {
        return;
    }
    let dw = dst.width as i32;
    let dh = dst.height as i32;
    let cr = color[0] as u32;
    let cg = color[1] as u32;
    let cb = color[2] as u32;
    let ca = color[3] as u32;
    for my in 0..mask.height as i32 {
        let py = oy + my;
        if py < 0 || py >= dh {
            continue;
        }
        for mx in 0..mask.width as i32 {
            let px = ox + mx;
            if px < 0 || px >= dw {
                continue;
            }
            let m = mask.get(mx as u32, my as u32) as u32;
            if m == 0 {
                continue;
            }
            let sa = (ca * m + 127) / 255;
            if sa == 0 {
                continue;
            }
            let sr = (cr * sa + 127) / 255;
            let sg = (cg * sa + 127) / 255;
            let sb = (cb * sa + 127) / 255;
            let idx = (py as usize * dst.width as usize + px as usize) * 4;
            let dr = dst.data[idx] as u32;
            let dg = dst.data[idx + 1] as u32;
            let db = dst.data[idx + 2] as u32;
            let da = dst.data[idx + 3] as u32;
            let inv = 255 - sa;
            dst.data[idx] = (sr + (dr * inv + 127) / 255) as u8;
            dst.data[idx + 1] = (sg + (dg * inv + 127) / 255) as u8;
            dst.data[idx + 2] = (sb + (db * inv + 127) / 255) as u8;
            dst.data[idx + 3] = (sa + (da * inv + 127) / 255) as u8;
        }
    }
}

// =========================================================================
// XML walker
// =========================================================================

fn parse_document(xml: &str) -> Result<SvgDoc, RenderError> {
    let mut p = XmlParser::new(xml);
    p.skip_prolog();
    let root = p.next_tag().ok_or(RenderError::Parse("svg root"))?;
    if root.kind != TagKind::Open || !name_is(root.name, "svg") {
        return Err(RenderError::Parse("svg root"));
    }

    let mut doc = SvgDoc {
        view_w: 1000.0,
        view_h: 1000.0,
        view_x: 0.0,
        view_y: 0.0,
        fills: Vec::new(),
    };

    let mut vb_seen = false;
    for (k, v) in attrs(root.attrs) {
        if k.eq_ignore_ascii_case("viewBox") {
            if let Some((x, y, w, h)) = parse_viewbox(v) {
                doc.view_x = x;
                doc.view_y = y;
                doc.view_w = w;
                doc.view_h = h;
                vb_seen = true;
            }
        } else if !vb_seen && k.eq_ignore_ascii_case("width") {
            if let Some(w) = parse_length(v) {
                doc.view_w = w;
            }
        } else if !vb_seen && k.eq_ignore_ascii_case("height") {
            if let Some(h) = parse_length(v) {
                doc.view_h = h;
            }
        }
    }

    if root.kind == TagKind::SelfClose {
        return Ok(doc);
    }

    let ctx = ElemCtx {
        xform: Affine::identity(),
        fill: [0, 0, 0, 255], // SVG default fill is opaque black
    };
    walk_children(&mut p, &mut doc, &ctx, 0)?;
    Ok(doc)
}

#[derive(Debug, Clone, Copy)]
struct ElemCtx {
    xform: Affine,
    fill: [u8; 4],
}

fn walk_children(
    p: &mut XmlParser<'_>,
    doc: &mut SvgDoc,
    ctx: &ElemCtx,
    depth: u32,
) -> Result<(), RenderError> {
    if depth > MAX_GROUP_DEPTH {
        return Err(RenderError::Parse("svg nesting"));
    }
    while let Some(tag) = p.next_tag() {
        match tag.kind {
            TagKind::Close => return Ok(()),
            TagKind::Comment | TagKind::Decl => continue,
            TagKind::Open | TagKind::SelfClose => {}
        }
        let self_close = tag.kind == TagKind::SelfClose;
        let mut child_ctx = *ctx;
        for (k, v) in attrs(tag.attrs) {
            if k.eq_ignore_ascii_case("transform") {
                if let Some(t) = parse_transform(v) {
                    child_ctx.xform = child_ctx.xform.compose(&t);
                }
            } else if k.eq_ignore_ascii_case("fill") {
                if let Some(c) = parse_color(v) {
                    // Preserve any fill-opacity already applied via
                    // CSS-like inheritance on this element by carrying
                    // forward the alpha factor only.
                    let alpha_factor = child_ctx.fill[3] as f32 / 255.0;
                    child_ctx.fill = [c[0], c[1], c[2], (c[3] as f32 * alpha_factor).round() as u8];
                } else if v.trim().eq_ignore_ascii_case("none") {
                    child_ctx.fill[3] = 0;
                }
            } else if k.eq_ignore_ascii_case("fill-opacity") || k.eq_ignore_ascii_case("opacity") {
                // SVG `opacity` strictly multiplies the rendered
                // element (not just its fill), but in our bounded
                // subset we only fill — so collapsing both attributes
                // to the same path is correct.
                if let Some(o) = parse_opacity(v) {
                    let a = (child_ctx.fill[3] as f32 * o).round().clamp(0.0, 255.0) as u8;
                    child_ctx.fill[3] = a;
                }
            }
        }

        if name_is(tag.name, "path") {
            if let Some(d) = attr_value(tag.attrs, "d") {
                let ops = parse_path_d(d)?;
                if !ops.is_empty() && child_ctx.fill[3] > 0 {
                    if doc.fills.len() >= MAX_FILLS {
                        return Err(RenderError::Parse("svg fill cap"));
                    }
                    doc.fills.push(Fill {
                        ops,
                        color: child_ctx.fill,
                        xform: child_ctx.xform,
                    });
                }
            }
            // `<path>` is normally self-closing; tolerate either form.
            if !self_close {
                skip_element_body(p)?;
            }
        } else if name_is(tag.name, "g") {
            if !self_close {
                walk_children(p, doc, &child_ctx, depth + 1)?;
            }
        } else if !self_close {
            // Unknown element body: skip it without parsing.
            skip_element_body(p)?;
        }
    }
    Ok(())
}

fn skip_element_body(p: &mut XmlParser<'_>) -> Result<(), RenderError> {
    let mut depth: u32 = 1;
    while let Some(tag) = p.next_tag() {
        match tag.kind {
            TagKind::Open => {
                depth += 1;
            }
            TagKind::SelfClose => {}
            TagKind::Close => {
                depth -= 1;
                if depth == 0 {
                    return Ok(());
                }
            }
            TagKind::Comment | TagKind::Decl => {}
        }
    }
    // Truncated input — tolerate so partial fills already collected
    // can still render.
    Ok(())
}

// =========================================================================
// Tiny XML scanner
// =========================================================================

#[derive(Debug, Clone, Copy, PartialEq)]
enum TagKind {
    /// `<name ...>`
    Open,
    /// `<name .../>`
    SelfClose,
    /// `</name>`
    Close,
    /// `<!-- ... -->`
    Comment,
    /// `<?xml ...?>` or `<!DOCTYPE ...>`
    Decl,
}

#[derive(Debug, Clone, Copy)]
struct Tag<'a> {
    kind: TagKind,
    name: &'a str,
    attrs: &'a str,
}

struct XmlParser<'a> {
    src: &'a str,
    pos: usize,
}

impl<'a> XmlParser<'a> {
    fn new(src: &'a str) -> Self {
        Self { src, pos: 0 }
    }

    fn skip_prolog(&mut self) {
        loop {
            self.skip_ws();
            let rest = &self.src[self.pos..];
            if let Some(stripped_start) = rest.strip_prefix("<?") {
                if let Some(end) = stripped_start.find("?>") {
                    self.pos += 2 + end + 2;
                    continue;
                }
                self.pos = self.src.len();
                return;
            }
            if rest.starts_with("<!--") {
                if let Some(end) = rest.find("-->") {
                    self.pos += end + 3;
                    continue;
                }
                self.pos = self.src.len();
                return;
            }
            if rest.starts_with("<!") {
                if let Some(end) = rest.find('>') {
                    self.pos += end + 1;
                    continue;
                }
                self.pos = self.src.len();
                return;
            }
            return;
        }
    }

    fn skip_ws(&mut self) {
        let bytes = self.src.as_bytes();
        while self.pos < bytes.len() && bytes[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
    }

    fn next_tag(&mut self) -> Option<Tag<'a>> {
        let bytes = self.src.as_bytes();
        // Skip text content between tags.
        while self.pos < bytes.len() && bytes[self.pos] != b'<' {
            self.pos += 1;
        }
        if self.pos >= bytes.len() {
            return None;
        }
        let rest = &self.src[self.pos..];
        if rest.starts_with("<!--") {
            if let Some(end) = rest.find("-->") {
                self.pos += end + 3;
                return Some(Tag {
                    kind: TagKind::Comment,
                    name: "",
                    attrs: "",
                });
            }
            self.pos = self.src.len();
            return None;
        }
        if rest.starts_with("<?") || rest.starts_with("<!") {
            if let Some(end) = rest.find('>') {
                self.pos += end + 1;
                return Some(Tag {
                    kind: TagKind::Decl,
                    name: "",
                    attrs: "",
                });
            }
            self.pos = self.src.len();
            return None;
        }
        let close = rest.find('>')?;
        let inner = &rest[1..close];
        self.pos += close + 1;

        if let Some(stripped) = inner.strip_prefix('/') {
            let name = stripped.split_ascii_whitespace().next().unwrap_or("");
            return Some(Tag {
                kind: TagKind::Close,
                name,
                attrs: "",
            });
        }
        let (kind, body) = if let Some(stripped) = inner.strip_suffix('/') {
            (TagKind::SelfClose, stripped)
        } else {
            (TagKind::Open, inner)
        };
        let body = body.trim();
        let (name, attrs) = match body.find(|c: char| c.is_ascii_whitespace()) {
            Some(i) => (&body[..i], body[i..].trim()),
            None => (body, ""),
        };
        Some(Tag { kind, name, attrs })
    }
}

fn name_is(name: &str, target: &str) -> bool {
    if name.eq_ignore_ascii_case(target) {
        return true;
    }
    if let Some(i) = name.find(':') {
        return name[i + 1..].eq_ignore_ascii_case(target);
    }
    false
}

// =========================================================================
// Attribute parsing
// =========================================================================

fn attrs(attrs: &str) -> AttrIter<'_> {
    AttrIter { rest: attrs }
}

struct AttrIter<'a> {
    rest: &'a str,
}

impl<'a> Iterator for AttrIter<'a> {
    type Item = (&'a str, &'a str);
    fn next(&mut self) -> Option<Self::Item> {
        self.rest = self.rest.trim_start();
        if self.rest.is_empty() {
            return None;
        }
        let eq = self.rest.find('=')?;
        let key = self.rest[..eq].trim();
        let after = self.rest[eq + 1..].trim_start();
        let bytes = after.as_bytes();
        if bytes.is_empty() {
            self.rest = "";
            return None;
        }
        let q = bytes[0];
        if q == b'"' || q == b'\'' {
            let body = &after[1..];
            let end = body.find(q as char)?;
            let val = &body[..end];
            self.rest = &body[end + 1..];
            Some((key, val))
        } else {
            let end = after
                .find(|c: char| c.is_ascii_whitespace() || c == '>')
                .unwrap_or(after.len());
            let val = &after[..end];
            self.rest = &after[end..];
            Some((key, val))
        }
    }
}

fn attr_value<'a>(attr_str: &'a str, key: &str) -> Option<&'a str> {
    for (k, v) in attrs(attr_str) {
        if k.eq_ignore_ascii_case(key) {
            return Some(v);
        }
    }
    None
}

// =========================================================================
// Numeric / colour / transform parsing
// =========================================================================

fn parse_viewbox(s: &str) -> Option<(f32, f32, f32, f32)> {
    let mut it = s.split(|c: char| c.is_ascii_whitespace() || c == ',');
    let x = it.find(|t| !t.is_empty())?.parse::<f32>().ok()?;
    let y = it.find(|t| !t.is_empty())?.parse::<f32>().ok()?;
    let w = it.find(|t| !t.is_empty())?.parse::<f32>().ok()?;
    let h = it.find(|t| !t.is_empty())?.parse::<f32>().ok()?;
    Some((x, y, w, h))
}

fn parse_length(s: &str) -> Option<f32> {
    let s = s.trim();
    let cut = s
        .char_indices()
        .find(|(_, c)| !(c.is_ascii_digit() || *c == '.' || *c == '-' || *c == '+'))
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    s[..cut].parse::<f32>().ok()
}

fn parse_opacity(s: &str) -> Option<f32> {
    let v = s.trim().parse::<f32>().ok()?;
    Some(v.clamp(0.0, 1.0))
}

/// Parses a colour value. Returns straight (un-premultiplied) RGBA.
fn parse_color(s: &str) -> Option<[u8; 4]> {
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
            let r = u8::from_str_radix(&rest[0..2], 16).ok()?;
            let g = u8::from_str_radix(&rest[2..4], 16).ok()?;
            let b = u8::from_str_radix(&rest[4..6], 16).ok()?;
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

fn parse_transform(s: &str) -> Option<Affine> {
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

// =========================================================================
// Path-data parser
// =========================================================================

/// Parses SVG `<path d="...">` data into the sigilbuzz `PathOp` stream.
/// Supports M/L/H/V/C/Q/Z and their relative variants. `S`, `T`, `A`
/// are not handled (rare in SVG-in-OT and a follow-up can lift them
/// in).
fn parse_path_d(s: &str) -> Result<Vec<PathOp>, RenderError> {
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
                // After M/m, repeated coord pairs are implicit L/l.
                Some(b'M') => b'L',
                Some(b'm') => b'l',
                Some(prev) => prev,
                None => return Err(RenderError::Parse("svg path d")),
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

// =========================================================================
// Tests
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_color_hex_long() {
        assert_eq!(parse_color("#FF8800"), Some([0xff, 0x88, 0x00, 255]));
        assert_eq!(parse_color("#000000"), Some([0, 0, 0, 255]));
    }

    #[test]
    fn parse_color_hex_short_doubles_each_nybble() {
        assert_eq!(parse_color("#f80"), Some([0xff, 0x88, 0x00, 255]));
    }

    #[test]
    fn parse_color_rgb_clamps_oversize() {
        assert_eq!(parse_color("rgb(300, -2, 128)"), Some([255, 0, 128, 255]));
    }

    #[test]
    fn parse_color_named_and_none() {
        assert_eq!(parse_color("black"), Some([0, 0, 0, 255]));
        assert_eq!(parse_color("WHITE"), Some([255, 255, 255, 255]));
        assert_eq!(parse_color("none"), None);
    }

    #[test]
    fn parse_viewbox_four_numbers() {
        assert_eq!(parse_viewbox("0 0 100 200"), Some((0.0, 0.0, 100.0, 200.0)));
        assert_eq!(
            parse_viewbox("-5, -5, 10, 10"),
            Some((-5.0, -5.0, 10.0, 10.0))
        );
    }

    #[test]
    fn parse_transform_translate_scale_matrix() {
        let t = parse_transform("translate(10, 20)").unwrap();
        assert!((t.dx - 10.0).abs() < 1e-5 && (t.dy - 20.0).abs() < 1e-5);
        let s = parse_transform("scale(2)").unwrap();
        assert!((s.xx - 2.0).abs() < 1e-5 && (s.yy - 2.0).abs() < 1e-5);
        let m = parse_transform("matrix(1 0 0 -1 0 100)").unwrap();
        assert!((m.yy + 1.0).abs() < 1e-5);
        assert!((m.dy - 100.0).abs() < 1e-5);
    }

    #[test]
    fn parse_transform_chains_left_to_right() {
        // translate then scale: a point (1, 0) should become (12, 0).
        let xf = parse_transform("translate(10, 0) scale(2)").unwrap();
        let (x, y) = xf.apply(1.0, 0.0);
        assert!((x - 12.0).abs() < 1e-5);
        assert!(y.abs() < 1e-5);
    }

    #[test]
    fn path_d_parses_absolute_mlz() {
        let ops = parse_path_d("M 0 0 L 10 0 L 10 10 L 0 10 Z").unwrap();
        assert!(matches!(ops[0], PathOp::MoveTo { x, y } if x == 0.0 && y == 0.0));
        assert!(matches!(ops[3], PathOp::LineTo { x, y } if x == 0.0 && y == 10.0));
        assert!(matches!(ops[4], PathOp::Close));
    }

    #[test]
    fn path_d_handles_relative_commands() {
        let abs = parse_path_d("M 0 0 L 10 0 L 10 10 L 0 10 Z").unwrap();
        let rel = parse_path_d("m 0 0 l 10 0 l 0 10 l -10 0 z").unwrap();
        let to_xy = |op: &PathOp| match *op {
            PathOp::MoveTo { x, y } | PathOp::LineTo { x, y } => Some((x, y)),
            _ => None,
        };
        let abs_pts: Vec<_> = abs.iter().filter_map(to_xy).collect();
        let rel_pts: Vec<_> = rel.iter().filter_map(to_xy).collect();
        assert_eq!(abs_pts, rel_pts);
    }

    #[test]
    fn path_d_handles_implicit_repetition() {
        let ops = parse_path_d("M 0 0 10 0 10 10 0 10 Z").unwrap();
        assert_eq!(ops.len(), 5);
        assert!(matches!(ops[1], PathOp::LineTo { x, y } if x == 10.0 && y == 0.0));
    }

    #[test]
    fn path_d_handles_curves() {
        let ops = parse_path_d("M 0 0 C 0 10 10 10 10 0 Q 5 -5 0 0 Z").unwrap();
        assert!(matches!(ops[1], PathOp::CubicTo { .. }));
        assert!(matches!(ops[2], PathOp::QuadTo { .. }));
        assert!(matches!(ops[3], PathOp::Close));
    }

    #[test]
    fn path_d_handles_h_and_v() {
        let ops = parse_path_d("M 1 2 H 5 V 7 Z").unwrap();
        assert!(matches!(ops[1], PathOp::LineTo { x, y } if x == 5.0 && y == 2.0));
        assert!(matches!(ops[2], PathOp::LineTo { x, y } if x == 5.0 && y == 7.0));
    }

    #[test]
    fn path_d_handles_compact_negative_numbers() {
        // No separator between sign and digits — common SVG output.
        let ops = parse_path_d("M0 0L10-5L-3 .5Z").unwrap();
        assert_eq!(ops.len(), 4);
        assert!(
            matches!(ops[2], PathOp::LineTo { x, y } if (x + 3.0).abs() < 1e-3 && (y - 0.5).abs() < 1e-3)
        );
    }

    #[test]
    fn xml_parser_walks_attributes() {
        let xml = r##"<svg viewBox="0 0 100 100" xmlns="http://www.w3.org/2000/svg">
            <path d="M 0 0 L 100 0 L 100 100 L 0 100 Z" fill="#FF0000"/>
        </svg>"##;
        let doc = parse_document(xml).unwrap();
        assert_eq!(doc.view_w, 100.0);
        assert_eq!(doc.view_h, 100.0);
        assert_eq!(doc.fills.len(), 1);
        assert_eq!(doc.fills[0].color, [0xff, 0, 0, 255]);
    }

    #[test]
    fn group_transform_composes_onto_path() {
        let xml = r#"<svg viewBox="0 0 100 100">
            <g transform="scale(2)">
                <path d="M 0 0 L 10 0 L 10 10 Z" fill="black"/>
            </g>
        </svg>"#;
        let doc = parse_document(xml).unwrap();
        assert_eq!(doc.fills.len(), 1);
        let xf = doc.fills[0].xform;
        assert!((xf.xx - 2.0).abs() < 1e-5);
        assert!((xf.yy - 2.0).abs() < 1e-5);
    }

    #[test]
    fn unknown_elements_are_skipped_not_failed() {
        let xml = r#"<svg viewBox="0 0 10 10">
            <defs><linearGradient id="g"><stop offset="0"/></linearGradient></defs>
            <path d="M 0 0 L 10 0 L 10 10 Z" fill="black"/>
        </svg>"#;
        let doc = parse_document(xml).unwrap();
        assert_eq!(doc.fills.len(), 1);
    }

    #[test]
    fn fill_none_suppresses_the_path() {
        let xml = r#"<svg viewBox="0 0 10 10">
            <path d="M 0 0 L 10 0 L 10 10 Z" fill="none"/>
        </svg>"#;
        let doc = parse_document(xml).unwrap();
        assert!(doc.fills.is_empty());
    }

    #[test]
    fn missing_root_svg_is_an_error() {
        assert!(parse_document("<not-svg/>").is_err());
    }
}
