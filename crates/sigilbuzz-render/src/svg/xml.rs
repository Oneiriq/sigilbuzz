//! Minimal XML support: the in-memory DOM node plus the hand-rolled
//! scanner that builds it.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::error::RenderError;

// =========================================================================
// XML tree
// =========================================================================

/// In-memory DOM. The document is small enough that this is cheap and
/// gives us free random access for `<use>` href resolution.
#[derive(Debug, Clone)]
pub(super) struct Node {
    pub(super) name: String,
    pub(super) attrs: Vec<(String, String)>,
    pub(super) children: Vec<Node>,
}

impl Node {
    pub(super) fn attr(&self, key: &str) -> Option<&str> {
        for (k, v) in &self.attrs {
            if attr_matches(k, key) {
                return Some(v.as_str());
            }
        }
        None
    }

    pub(super) fn id(&self) -> Option<&str> {
        self.attr("id")
    }
}

pub(super) fn attr_matches(actual: &str, target: &str) -> bool {
    if actual.eq_ignore_ascii_case(target) {
        return true;
    }
    if let Some(i) = actual.find(':') {
        return actual[i + 1..].eq_ignore_ascii_case(target);
    }
    false
}

pub(super) fn name_eq(a: &str, b: &str) -> bool {
    if a.eq_ignore_ascii_case(b) {
        return true;
    }
    if let Some(i) = a.find(':') {
        return a[i + 1..].eq_ignore_ascii_case(b);
    }
    false
}

// =========================================================================
// XML scanner -> DOM
// =========================================================================

pub(super) fn parse_xml(xml: &str) -> Result<Node, RenderError> {
    let mut p = XmlParser::new(xml);
    p.skip_prolog();
    let Some(tag) = p.next_tag() else {
        return Err(RenderError::Parse("svg root"));
    };
    if tag.kind == TagKind::Comment || tag.kind == TagKind::Decl {
        return parse_xml_after_prolog(&mut p);
    }
    if tag.kind != TagKind::Open && tag.kind != TagKind::SelfClose {
        return Err(RenderError::Parse("svg root"));
    }
    let mut node = Node {
        name: tag.name.into(),
        attrs: parse_attrs(tag.attrs),
        children: Vec::new(),
    };
    if tag.kind == TagKind::SelfClose {
        return Ok(node);
    }
    parse_children(&mut p, &mut node, 0)?;
    Ok(node)
}

fn parse_xml_after_prolog(p: &mut XmlParser<'_>) -> Result<Node, RenderError> {
    while let Some(tag) = p.next_tag() {
        match tag.kind {
            TagKind::Comment | TagKind::Decl => continue,
            TagKind::Open | TagKind::SelfClose => {
                let mut node = Node {
                    name: tag.name.into(),
                    attrs: parse_attrs(tag.attrs),
                    children: Vec::new(),
                };
                if tag.kind == TagKind::Open {
                    parse_children(p, &mut node, 0)?;
                }
                return Ok(node);
            }
            TagKind::Close => {
                return Err(RenderError::Parse("svg root"));
            }
        }
    }
    Err(RenderError::Parse("svg root"))
}

fn parse_children(p: &mut XmlParser<'_>, parent: &mut Node, depth: u32) -> Result<(), RenderError> {
    if depth > 256 {
        return Err(RenderError::Parse("svg nesting"));
    }
    while let Some(tag) = p.next_tag() {
        match tag.kind {
            TagKind::Comment | TagKind::Decl => continue,
            TagKind::Close => return Ok(()),
            TagKind::Open => {
                let mut child = Node {
                    name: tag.name.into(),
                    attrs: parse_attrs(tag.attrs),
                    children: Vec::new(),
                };
                parse_children(p, &mut child, depth + 1)?;
                parent.children.push(child);
            }
            TagKind::SelfClose => {
                parent.children.push(Node {
                    name: tag.name.into(),
                    attrs: parse_attrs(tag.attrs),
                    children: Vec::new(),
                });
            }
        }
    }
    Ok(())
}

fn parse_attrs(s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = s;
    loop {
        rest = rest.trim_start();
        if rest.is_empty() {
            break;
        }
        let Some(eq) = rest.find('=') else {
            break;
        };
        let key = rest[..eq].trim().to_string();
        let after = rest[eq + 1..].trim_start();
        let bytes = after.as_bytes();
        if bytes.is_empty() {
            break;
        }
        let q = bytes[0];
        let (val, next) = if q == b'"' || q == b'\'' {
            let body = &after[1..];
            let Some(end) = body.find(q as char) else {
                break;
            };
            (body[..end].to_string(), &body[end + 1..])
        } else {
            let end = after
                .find(|c: char| c.is_ascii_whitespace() || c == '>')
                .unwrap_or(after.len());
            (after[..end].to_string(), &after[end..])
        };
        out.push((key, val));
        rest = next;
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum TagKind {
    Open,
    SelfClose,
    Close,
    Comment,
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
            if let Some(stripped) = rest.strip_prefix("<?") {
                if let Some(end) = stripped.find("?>") {
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
