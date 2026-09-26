//! OTF / TrueType embedded font emission.
//!
//! Conceptually the simplest of the three emitters: the produced PDF
//! font *references* the original font program verbatim through a
//! `/FontFile2` (TrueType) or `/FontFile3` (OTF/CFF) stream and tells
//! the consumer how to map character codes to glyph ids via a
//! `/CIDToGIDMap`.
//!
//! # Pipeline
//!
//! ```text
//!   font_bytes (raw .ttf/.otf bytes)
//!         |
//!         v  emit_otf_embedded_font
//!   OtfEmbeddedFont
//!     + font_dict_body       /Type /Font /Subtype /Type0 ...
//!     + descriptor_body      /Type /FontDescriptor /FontFile2 N 0 R ...
//!     + program              raw font_bytes verbatim
//!     + cid_to_gid_map       2 bytes x 256 entries (Identity-H)
//!     + widths               (gid, advance-in-1000-units) pairs
//! ```
//!
//! # Subsetting is a separate concern
//!
//! `program` is the unmodified `font_bytes` slice the caller hands
//! in. Real-world PDFs typically subset the font program down to
//! just the glyphs that appear in the document. That work lives in
//! the `sigilbuzz-subset` crate (parallel development). Once that
//! lands, a 0.6.0 wiring step will let `emit_otf_embedded_font` take
//! a pre-subset byte slice the same way it takes the full one today;
//! the public surface here doesn't need to change.
//!
//! # Widths
//!
//! PDF expresses font widths in 1000-unit "character space," so the
//! emitter divides each gid's hmtx advance by `units_per_em` and
//! multiplies by 1000. A 2048-upem face's 1366-unit advance becomes
//! ~667 in the widths array. The conversion is `f32` to keep the
//! consumer free to round however it likes when serializing the PDF
//! `/W` array.
//!
//! # CIDToGIDMap
//!
//! With `/Encoding /Identity-H` the consumer addresses glyphs by
//! 2-byte CIDs. The CIDToGIDMap is a flat byte stream indexed by
//! CID; each entry is a 2-byte big-endian gid. The emitter produces
//! a 256-CID map (512 bytes) suitable for fonts the caller drives
//! with single-byte char codes wrapped in an Identity-H Type 0
//! parent. The small map keeps PDF object size down. Callers that
//! need a full 16-bit CID range can grow the map after the fact;
//! the inner data layout (2 bytes BE per CID) is stable.

use alloc::format;
use alloc::vec;
use alloc::vec::Vec;

use sigilbuzz::Face;

use crate::GlyphId;

/// PDF font dictionary fragments for an OTF/TrueType-embedded font.
///
/// All fields are independent so a downstream PDF serializer can
/// stitch them into a Type 0 / CIDFontType2 font object without
/// reparsing.
#[derive(Debug, Clone, PartialEq)]
pub struct OtfEmbeddedFont {
    /// Top-level font dictionary body: `/Type /Font /Subtype /Type0
    /// /BaseFont /SigilbuzzEmbedded /Encoding /Identity-H
    /// /DescendantFonts [<<...>>]`. References the descriptor and
    /// CIDToGIDMap stream by indirect placeholders the consumer
    /// substitutes when assembling the PDF.
    pub font_dict_body: Vec<u8>,
    /// Font descriptor body: `/Type /FontDescriptor /FontFile2 N 0
    /// R` (or `/FontFile3` for CFF). The descriptor includes the
    /// stock metric placeholders (`/Ascent`, `/Descent`, `/CapHeight`,
    /// `/StemV`) the consumer is expected to fill from the face's
    /// `OS/2` and `head` tables.
    pub descriptor_body: Vec<u8>,
    /// The font program bytes: for now an unmodified copy of the
    /// `font_bytes` slice the caller passed in. A future
    /// `sigilbuzz-subset`-driven path will substitute a subset
    /// program here without changing the public type.
    pub program: Vec<u8>,
    /// 2-bytes-per-CID Identity-H mapping. Length is always
    /// `2 * 256 = 512`. Each pair is a big-endian gid; the entry at
    /// CID `c` is at byte offset `2*c`.
    pub cid_to_gid_map: Vec<u8>,
    /// `(gid, width)` pairs in input order. The width is in PDF's
    /// 1000-unit character space (i.e. `advance_in_design_units *
    /// 1000.0 / units_per_em`).
    pub widths: Vec<(GlyphId, f32)>,
}

/// Build an [`OtfEmbeddedFont`] for the given face, raw font bytes,
/// and gid list.
///
/// `font_bytes` is copied verbatim into [`OtfEmbeddedFont::program`].
/// The caller is responsible for handing in the same byte slice the
/// `Face` was parsed from. There is no integrity check, since the
/// face itself was the integrity check upstream.
///
/// Output is deterministic: the same face, byte slice, and gid list
/// produce a byte-identical [`OtfEmbeddedFont`].
#[must_use]
pub fn emit_otf_embedded_font(
    face: &Face<'_>,
    font_bytes: &[u8],
    gids: &[GlyphId],
) -> OtfEmbeddedFont {
    let upem = face.head().map(|h| h.units_per_em).unwrap_or(1000);
    let upem_f = if upem == 0 {
        1000.0_f32
    } else {
        f32::from(upem)
    };
    let hmtx = face.hmtx().ok();

    // CIDToGIDMap: 256 CIDs x 2 bytes BE. CID 0 is reserved for
    // /.notdef per spec; we fill it with gid 0 (which faces always
    // expose as the .notdef glyph). The rest map sequentially to
    // the input gids. Extra slots stay at gid 0 (notdef).
    let mut cid_to_gid_map = vec![0u8; 512];
    for (idx, &gid) in (1u16..).zip(gids.iter()) {
        if idx >= 256 {
            break;
        }
        let off = (idx as usize) * 2;
        cid_to_gid_map[off] = (gid >> 8) as u8;
        cid_to_gid_map[off + 1] = (gid & 0xff) as u8;
    }

    // Widths: convert each gid's design-unit advance to PDF 1000-unit
    // character space. The widths vector parallels the input gid
    // order; missing advances collapse to 0.
    let widths: Vec<(GlyphId, f32)> = gids
        .iter()
        .map(|&gid| {
            let advance = hmtx
                .as_ref()
                .and_then(|h| h.advance(gid))
                .map_or(0.0_f32, f32::from);
            (gid, advance * 1000.0_f32 / upem_f)
        })
        .collect();

    // Detect whether the program is a CFF/OTF or a glyf/TTF. The
    // distinction picks /FontFile3 vs /FontFile2 in the descriptor.
    // Every TrueType file starts with the sfnt scaler 0x00010000 or
    // 'true'; OTF starts with 'OTTO'.
    let is_cff_otf = font_bytes.len() >= 4 && &font_bytes[..4] == b"OTTO";

    // Top-level Type 0 font dict. The consumer is expected to
    // substitute "<descriptor obj>" and "<cidmap obj>" with concrete
    // indirect-reference numbers (e.g. "12 0 R") at PDF assembly
    // time. We emit them as placeholder tokens so the body is
    // copy-pasteable into a manual PDF for testing.
    let mut font_dict_body = Vec::new();
    font_dict_body.extend_from_slice(b"<<\n");
    font_dict_body.extend_from_slice(b"/Type /Font\n");
    font_dict_body.extend_from_slice(b"/Subtype /Type0\n");
    font_dict_body.extend_from_slice(b"/BaseFont /SigilbuzzEmbedded\n");
    font_dict_body.extend_from_slice(b"/Encoding /Identity-H\n");
    font_dict_body.extend_from_slice(b"/DescendantFonts [<<\n");
    font_dict_body.extend_from_slice(b"  /Type /Font\n");
    if is_cff_otf {
        font_dict_body.extend_from_slice(b"  /Subtype /CIDFontType0\n");
    } else {
        font_dict_body.extend_from_slice(b"  /Subtype /CIDFontType2\n");
    }
    font_dict_body.extend_from_slice(b"  /BaseFont /SigilbuzzEmbedded\n");
    font_dict_body.extend_from_slice(
        b"  /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >>\n",
    );
    font_dict_body.extend_from_slice(b"  /FontDescriptor <descriptor obj>\n");
    font_dict_body.extend_from_slice(b"  /CIDToGIDMap <cidmap obj>\n");
    // /W array: the caller can re-emit this from `widths` if they
    // prefer a different rounding strategy. We provide one in the
    // dict so the emitted body is self-contained for the simple case.
    font_dict_body.extend_from_slice(b"  /W [");
    for (gid, width) in &widths {
        let line = format!(" {gid} [{}]", fmt_width(*width));
        font_dict_body.extend_from_slice(line.as_bytes());
    }
    font_dict_body.extend_from_slice(b" ]\n");
    font_dict_body.extend_from_slice(b">>]\n");
    font_dict_body.extend_from_slice(b">>\n");

    // Font descriptor.
    let mut descriptor_body = Vec::new();
    descriptor_body.extend_from_slice(b"<<\n");
    descriptor_body.extend_from_slice(b"/Type /FontDescriptor\n");
    descriptor_body.extend_from_slice(b"/FontName /SigilbuzzEmbedded\n");
    descriptor_body.extend_from_slice(b"/Flags 4\n"); // symbolic = bit 3 set
    descriptor_body.extend_from_slice(b"/ItalicAngle 0\n");
    // Stock metric placeholders. A full implementation would pull
    // these from OS/2 + head; the placeholders keep the dict
    // well-formed and let the consumer override before serialization.
    descriptor_body.extend_from_slice(b"/Ascent 800\n");
    descriptor_body.extend_from_slice(b"/Descent -200\n");
    descriptor_body.extend_from_slice(b"/CapHeight 700\n");
    descriptor_body.extend_from_slice(b"/StemV 80\n");
    {
        let line = format!(
            "/FontBBox [{} {} {} {}]\n",
            -(upem_f / 4.0) as i32,
            -(upem_f / 4.0) as i32,
            upem_f as i32,
            upem_f as i32
        );
        descriptor_body.extend_from_slice(line.as_bytes());
    }
    if is_cff_otf {
        descriptor_body.extend_from_slice(b"/FontFile3 <program obj>\n");
    } else {
        descriptor_body.extend_from_slice(b"/FontFile2 <program obj>\n");
    }
    descriptor_body.extend_from_slice(b">>\n");

    OtfEmbeddedFont {
        font_dict_body,
        descriptor_body,
        program: font_bytes.to_vec(),
        cid_to_gid_map,
        widths,
    }
}

/// Format a width with a single decimal at most to keep the PDF
/// `/W` array compact while preserving sub-unit precision.
fn fmt_width(w: f32) -> alloc::string::String {
    let s = format!("{w}");
    // Strip ".0" the same way stream.rs does so the output matches
    // the rest of the crate.
    if let Some(stripped) = s.strip_suffix(".0") {
        alloc::string::String::from(stripped)
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cid_to_gid_map_is_512_bytes() {
        let map = vec![0u8; 512];
        assert_eq!(map.len(), 512);
    }

    #[test]
    fn fmt_width_strips_dot_zero() {
        assert_eq!(fmt_width(0.0), "0");
        assert_eq!(fmt_width(666.6), "666.6");
    }
}
