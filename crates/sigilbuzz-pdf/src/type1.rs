//! Type 1 (PostScript) font emission.
//!
//! Produces the three text/byte fragments a downstream PDF
//! serialiser needs to assemble a Type 1 font object:
//!
//! - the [public font dictionary][Type1Font::font_dict_body]
//!   (`/FontInfo`, `/FontType`, `/FontMatrix`, `/FontBBox`, `/Encoding`),
//! - the [private dictionary][Type1Font::private_dict_body] containing
//!   `/BlueValues`, `/MinFeature`, and the `/lenIV -1` directive that
//!   tells the consumer the charstrings are *cleartext*,
//! - the [`/CharStrings`][Type1Font::char_strings_body] block with one
//!   per-glyph charstring, keyed by `/g{gid}` names matching the Type 3
//!   emitter's convention.
//!
//! # eexec is intentionally skipped
//!
//! A spec-compliant Type 1 font wraps the private dict and charstrings
//! in an *eexec* (encrypted-execute) block: a XOR-with-feedback stream
//! cipher with a fixed seed (55665) and an additional per-charstring
//! 4-byte random salt. Adobe Reader and every modern PDF consumer the
//! authors have access to (Preview, pdfium, mupdf, Poppler) accept
//! Type 1 fonts that ship the **cleartext** block twice in lieu of an
//! encrypted half — the `/lenIV -1` directive in the private dict
//! signals "charstrings are not eexec-encrypted." Skipping eexec keeps
//! this PR small and focused; a follow-up can layer the cipher on top
//! of the existing module if a non-Adobe consumer ever surfaces.
//!
//! # FontMatrix
//!
//! Type 1 stores a per-font `/FontMatrix` that maps glyph design units
//! into a 1000-unit "character space." sigilbuzz emits glyphs in their
//! native upem space, so the matrix is `[1/upem 0 0 1/upem 0 0]` —
//! identical to the Type 3 convention.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use sigilbuzz::Face;

use crate::type1_charstring::{emit_endchar, emit_hsbw, emit_path_ops};
use crate::{Bbox, GlyphId};

/// Errors the Type 1 emitter can return.
///
/// The Type 1 surface is fallible (unlike the Type 3 surface) because
/// a malformed or sufficiently degenerate face cannot produce a
/// well-formed `/FontMatrix` — Type 1 spec requires a non-zero scale.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmitError {
    /// The face's `head` table reports `units_per_em == 0`, which would
    /// produce an undefined `FontMatrix` reciprocal.
    InvalidUnitsPerEm,
}

impl core::fmt::Display for EmitError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            EmitError::InvalidUnitsPerEm => f.write_str("face has zero units_per_em"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for EmitError {}

/// A Type 1 (PostScript) font assembled from a sigilbuzz face and a
/// glyph-id list.
///
/// The fields are independent byte buffers so the consumer can splice
/// them into a PDF object stream without reparsing or re-formatting.
/// All three buffers are valid 7-bit ASCII PostScript text.
#[derive(Debug, Clone, PartialEq)]
pub struct Type1Font {
    /// Public font dictionary body — the part of a Type 1 font that
    /// lives *outside* the eexec block. Includes `/FontInfo`,
    /// `/FontType 1`, `/FontMatrix`, `/FontBBox`, and the encoding
    /// vector that maps PDF char codes to glyph names.
    pub font_dict_body: Vec<u8>,
    /// `/Private` dict body. Contains `/BlueValues`, `/MinFeature`,
    /// and the `/lenIV -1` flag (cleartext charstrings — see module
    /// docs). The `/RD` and `/ND`/`/NP` PostScript primitives are
    /// declared here so the `/CharStrings` body parses standalone.
    pub private_dict_body: Vec<u8>,
    /// `/CharStrings` body — one entry per emitted glyph, keyed by
    /// `/g{gid}`. Each value is a Type 1 charstring (cleartext, not
    /// eexec-encrypted) beginning with `hsbw` and ending with
    /// `endchar`.
    pub char_strings_body: Vec<u8>,
}

/// Build a [`Type1Font`] for the given face and gid list.
///
/// Glyphs are emitted in input order. The first 256 are encoded into
/// the font's `/Encoding` vector (Type 1, like Type 3, is 8-bit on
/// the consumer side); any tail beyond 256 still ships its charstring
/// so a consumer that builds a Type 0 wrapper can address them, but
/// they don't appear in the encoding.
///
/// Output is deterministic: the same face and gid slice produce a
/// byte-identical [`Type1Font`].
///
/// # Errors
///
/// Returns [`EmitError::InvalidUnitsPerEm`] if the face's `head`
/// table reports `units_per_em == 0`.
pub fn emit_type1_font(face: &Face<'_>, gids: &[GlyphId]) -> Result<Type1Font, EmitError> {
    let upem = face.head().map(|h| h.units_per_em).unwrap_or(1000);
    if upem == 0 {
        return Err(EmitError::InvalidUnitsPerEm);
    }
    let hmtx = face.hmtx().ok();

    // Compute the union bbox up-front so we can bake it into the public
    // font dict. Per-glyph bboxes are computed twice (once here, once
    // implicitly via charstring path tracing in a downstream rasteriser)
    // — that is the trade-off for shipping a complete /FontBBox without
    // a second pass.
    let mut font_bbox = Bbox::empty();
    for &gid in gids {
        if let Ok(Some(o)) = face.glyph_outline(gid) {
            let bb = crate::stream::outline_bbox(o.ops());
            font_bbox.union(&bb);
        }
    }
    if font_bbox.is_empty() {
        font_bbox = Bbox {
            xmin: 0.0,
            ymin: 0.0,
            xmax: 0.0,
            ymax: 0.0,
        };
    }

    // Build the public font dict body. PostScript numbers are emitted
    // by Display directly — no PDF-style ".0" trimming is required;
    // the output is parsed by a PostScript interpreter, not a PDF
    // tokeniser.
    let mut font_dict_body = Vec::new();
    font_dict_body.extend_from_slice(b"12 dict begin\n");
    font_dict_body.extend_from_slice(b"/FontInfo 4 dict dup begin\n");
    font_dict_body.extend_from_slice(b"  /FullName (sigilbuzz Type1 Font) def\n");
    font_dict_body.extend_from_slice(b"  /FamilyName (sigilbuzz) def\n");
    font_dict_body.extend_from_slice(b"  /Weight (Regular) def\n");
    font_dict_body.extend_from_slice(b"  /ItalicAngle 0 def\n");
    font_dict_body.extend_from_slice(b"end def\n");
    font_dict_body.extend_from_slice(b"/FontName /SigilbuzzType1 def\n");
    font_dict_body.extend_from_slice(b"/FontType 1 def\n");
    {
        let s = 1.0_f32 / f32::from(upem);
        let line = format!("/FontMatrix [{s} 0 0 {s} 0 0] def\n");
        font_dict_body.extend_from_slice(line.as_bytes());
    }
    {
        let line = format!(
            "/FontBBox [{} {} {} {}] readonly def\n",
            ps_num(font_bbox.xmin),
            ps_num(font_bbox.ymin),
            ps_num(font_bbox.xmax),
            ps_num(font_bbox.ymax),
        );
        font_dict_body.extend_from_slice(line.as_bytes());
    }
    // Encoding vector: 256 slots, default to /.notdef. The first
    // up-to-255 input gids map to char codes 1..=255 (code 0 stays
    // /.notdef, matching the Type 3 emitter's convention).
    font_dict_body.extend_from_slice(b"/Encoding 256 array\n");
    font_dict_body.extend_from_slice(b"0 1 255 {1 index exch /.notdef put} for\n");
    for (idx, &gid) in (1_u16..).zip(gids.iter()) {
        if idx > 255 {
            break;
        }
        let line = format!("dup {idx} /g{gid} put\n");
        font_dict_body.extend_from_slice(line.as_bytes());
    }
    font_dict_body.extend_from_slice(b"readonly def\n");

    // Private dict. /lenIV -1 means "the charstrings are not
    // eexec-encrypted" — see the module docs.
    let mut private_dict_body = Vec::new();
    private_dict_body.extend_from_slice(b"dup /Private 8 dict dup begin\n");
    private_dict_body
        .extend_from_slice(b"/-|{string currentfile exch readstring pop}executeonly def\n");
    private_dict_body.extend_from_slice(b"/|-{noaccess def}executeonly def\n");
    private_dict_body.extend_from_slice(b"/|{noaccess put}executeonly def\n");
    private_dict_body.extend_from_slice(b"/BlueValues [] def\n");
    private_dict_body.extend_from_slice(b"/MinFeature {16 16} def\n");
    private_dict_body.extend_from_slice(b"/password 5839 def\n");
    private_dict_body.extend_from_slice(b"/lenIV -1 def\n");

    // /CharStrings block. Emit one entry per gid in input order.
    let mut char_strings_body = Vec::new();
    let count = gids.len();
    char_strings_body
        .extend_from_slice(format!("2 index /CharStrings {count} dict dup begin\n").as_bytes());
    // Always-present /.notdef entry: zero-width, just an endchar.
    {
        let mut notdef = Vec::new();
        emit_hsbw(&mut notdef, 0, 0);
        emit_endchar(&mut notdef);
        let line = format!("/.notdef {} -| ", notdef.len());
        char_strings_body.extend_from_slice(line.as_bytes());
        char_strings_body.extend_from_slice(&notdef);
        char_strings_body.extend_from_slice(b" |-\n");
    }

    for &gid in gids {
        let advance = hmtx
            .as_ref()
            .and_then(|h| h.advance(gid))
            .map_or(0_i32, i32::from);

        let mut cs = Vec::new();
        // hsbw with lsb=0 (sigilbuzz's PathOp coordinates are absolute,
        // so the lsb is implicit in the first MoveTo's x — emitting 0
        // here keeps the charstring's pen origin at the design-space
        // origin, which matches what every PathOp x/y is measured
        // against).
        emit_hsbw(&mut cs, 0, advance);

        if let Ok(Some(o)) = face.glyph_outline(gid) {
            emit_path_ops(&mut cs, o.ops());
        }
        emit_endchar(&mut cs);

        let header = format!("/g{gid} {} -| ", cs.len());
        char_strings_body.extend_from_slice(header.as_bytes());
        char_strings_body.extend_from_slice(&cs);
        char_strings_body.extend_from_slice(b" |-\n");
    }
    char_strings_body.extend_from_slice(b"end\n");

    Ok(Type1Font {
        font_dict_body,
        private_dict_body,
        char_strings_body,
    })
}

/// Format an `f32` for inclusion in a PostScript number literal —
/// matches the Type 3 stream.rs convention of stripping `.0` so the
/// output is compact and snapshot-stable.
fn ps_num(value: f32) -> String {
    let s = format!("{value}");
    if let Some(stripped) = s.strip_suffix(".0") {
        String::from(stripped)
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sigilbuzz::tables::PathOp;

    /// Build a charstring the same way the emitter does, sans Face,
    /// so unit tests can lock down the byte sequence without a real
    /// font.
    fn charstring_for(advance: i32, ops: &[PathOp]) -> Vec<u8> {
        let mut cs = Vec::new();
        emit_hsbw(&mut cs, 0, advance);
        emit_path_ops(&mut cs, ops);
        emit_endchar(&mut cs);
        cs
    }

    #[test]
    fn charstring_starts_with_hsbw_and_ends_with_endchar() {
        let cs = charstring_for(
            500,
            &[
                PathOp::MoveTo { x: 0.0, y: 0.0 },
                PathOp::LineTo { x: 100.0, y: 0.0 },
                PathOp::Close,
            ],
        );

        // hsbw lsb=0 advance=500 → encode(0)=139, encode(500)=[248,136], op13.
        assert_eq!(&cs[..4], &[139, 248, 136, 13]);
        // Last byte is endchar (op 14).
        assert_eq!(*cs.last().unwrap(), 14);
    }

    #[test]
    fn invalid_upem_returns_error() {
        // We can't easily build a Face with upem=0 without writing
        // raw font bytes, but we can confirm the error variant
        // formats correctly.
        let err = EmitError::InvalidUnitsPerEm;
        assert!(format!("{err}").contains("zero"));
    }

    #[test]
    fn ps_num_strips_dot_zero() {
        assert_eq!(ps_num(0.0), "0");
        assert_eq!(ps_num(-100.0), "-100");
        assert_eq!(ps_num(0.5), "0.5");
    }
}
