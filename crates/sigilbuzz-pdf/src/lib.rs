//! sigilbuzz-pdf — PDF font emitters for sigilbuzz outlines.
//!
//! PDF supports several font flavours: Type 1 (PostScript), TrueType
//! / OpenType embedded variants, and Type 3 — "user-defined fonts"
//! whose glyphs are described as ordinary PDF content streams. This
//! crate ships emitters for all three:
//!
//! - [`emit_type3_font`]: every `MoveTo` / `LineTo` / curve becomes a
//!   PDF drawing operator emitted into a `CharProc` content stream.
//! - [`emit_type1_font`]: per-glyph Type 1 charstrings, cleartext —
//!   eexec encryption is intentionally skipped (see the `Type1Font`
//!   docs for the rationale).
//! - [`emit_otf_embedded_font`]: a PDF font dictionary referencing
//!   the original font bytes verbatim plus a CIDToGIDMap.
//!
//! sigilbuzz-pdf produces the [`Type3Font`] data structure; the
//! consumer is responsible for serialising it into a PDF document.
//! That separation keeps this crate dependency-free — no `lopdf`,
//! no `printpdf`. Output is deterministic: same `Face` + same gid
//! list yields a byte-identical [`Type3Font`].
//!
//! # Pipeline
//!
//! ```text
//!   Face                       Type3Font
//!     │                         ┌─────────────────────────────┐
//!     │ glyph_outline(gid)      │ bbox: Bbox                  │
//!     ▼                         │ matrix: [f32; 6]            │
//!   Outline (PathOps)           │ char_procs: Vec<CharProc>   │
//!     │                         │ encoding: Vec<(u8, String)> │
//!     │ PathOp → PDF op         │ widths:   Vec<f32>          │
//!     ▼                         └─────────────────────────────┘
//!   CharProc.body bytes
//! ```
//!
//! # Quick start
//!
//! ```no_run
//! use sigilbuzz::{Blob, Face};
//! use sigilbuzz_pdf::emit_type3_font;
//!
//! let blob = Blob::from_path("./MyFont.ttf").unwrap();
//! let face = Face::parse_bytes(blob.as_bytes(), 0).unwrap();
//! let font = emit_type3_font(&face, &[36, 37, 38]);
//! // serialise font.bbox / font.matrix / font.char_procs into a PDF.
//! # let _ = font;
//! ```
//!
//! # Why Type 3
//!
//! Type 3 sidesteps every PDF-side font-embedding subtlety: there is
//! no `cmap` to translate, no `head.indexToLocFormat` to preserve,
//! no subsetting to perform. The trade-off is that Type 3 fonts are
//! not hinted and are typically rendered via the PDF content stream's
//! own raster path — fine for archival, signage, and headline use,
//! less ideal for body text at small sizes. Type 1 / OTF-embedded
//! variants can land in a future release if demand materialises.
//!
//! # No-std
//!
//! `sigilbuzz-pdf` builds with `--no-default-features`. It uses
//! `alloc::vec::Vec` and `alloc::string::String`; the emitter never
//! requires `std`.
//!
//! [`PathOp`]: sigilbuzz::tables::PathOp

#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use sigilbuzz::Face;

mod stream;
mod type1;
mod type1_charstring;

pub use stream::{emit_d1_prologue, emit_fill_epilogue, emit_path_ops, outline_bbox};
pub use type1::{emit_type1_font, EmitError, Type1Font};

/// Crate version, matching `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Build a Type 3 `FontMatrix` row-vector tuple `[a b c d e f]` for a
/// face whose glyph coordinates live in `units_per_em` design units.
///
/// PDF's `FontMatrix` maps glyph-space coordinates to text space (1
/// unit = 1 typographic point at use time, after the text-state
/// matrix scale). The standard convention for font-design-unit
/// fonts is `[1/upem 0 0 1/upem 0 0]` — uniform scale, no shear, no
/// translation. sigilbuzz emits glyphs in their native upem space
/// so this matrix is the same for every CharProc in the font.
#[must_use]
pub fn font_matrix(units_per_em: u16) -> [f32; 6] {
    let s = 1.0_f32 / f32::from(units_per_em);
    [s, 0.0, 0.0, s, 0.0, 0.0]
}

/// Build a [`Type3Font`] for the given face and glyph-id list.
///
/// Each gid is mapped to:
///
/// - PDF char code, assigned sequentially starting at 1 — char code
///   0 is reserved for `.notdef` so it is left out of the encoding.
/// - PDF name, formatted as `g{gid}` (for example `g42`).
/// - CharProc body: `wx 0 llx lly urx ury d1\n` prologue, the
///   PathOp -> PDF operator stream, then `f\n` (non-zero winding
///   fill) epilogue. Glyphs whose outline cannot be retrieved or
///   whose `glyph_outline` returns `None` (whitespace, `.notdef`)
///   produce a body containing only the d1 prologue and the fill
///   epilogue, which is the conventional PDF way to encode a
///   visible-only-by-its-advance glyph.
/// - Width: the gid's `hmtx` advance, in glyph-design-unit space.
///   Faces without an `hmtx` table or with an out-of-range gid
///   contribute width 0.
///
/// The font's `bbox` is the union of every included CharProc bbox;
/// the `matrix` is `[1/upem 0 0 1/upem 0 0]`. Gids beyond char code
/// 255 are silently skipped — Type 3 encodings are 8-bit only.
///
/// Output is deterministic: the same face and gid slice produce a
/// byte-identical [`Type3Font`].
#[must_use]
pub fn emit_type3_font(face: &Face<'_>, gids: &[GlyphId]) -> Type3Font {
    // Look up the upem and hmtx once. Failures collapse to a
    // 1000-upem default and absent advance widths — neither
    // condition is normal for a real font, but the public surface
    // is total so a malformed face does not panic.
    let upem = face.head().map(|h| h.units_per_em).unwrap_or(1000);
    let hmtx = face.hmtx().ok();

    let mut char_procs = Vec::with_capacity(gids.len().min(255));
    let mut encoding = Vec::with_capacity(gids.len().min(255));
    let mut widths = Vec::with_capacity(gids.len().min(255));
    let mut font_bbox = Bbox::empty();

    for (idx, &gid) in (1_u16..).zip(gids.iter()) {
        if idx > 255 {
            break;
        }
        let code = idx as u8;

        let name = format!("g{gid}");
        let advance = hmtx
            .as_ref()
            .and_then(|h| h.advance(gid))
            .map_or(0.0_f32, f32::from);

        // Outline + bbox. Errors and `None` collapse to an empty
        // outline and a zeroed bbox — Type 3 still requires a d1
        // prologue and a body, so we synthesise an empty drawing
        // program.
        let ops_owned;
        let ops: &[sigilbuzz::tables::PathOp] = match face.glyph_outline(gid) {
            Ok(Some(o)) => {
                ops_owned = o;
                ops_owned.ops()
            }
            _ => &[],
        };

        let bbox = if ops.is_empty() {
            Bbox {
                xmin: 0.0,
                ymin: 0.0,
                xmax: 0.0,
                ymax: 0.0,
            }
        } else {
            outline_bbox(ops)
        };

        let mut body = Vec::new();
        emit_d1_prologue(&mut body, advance, bbox);
        emit_path_ops(&mut body, ops);
        emit_fill_epilogue(&mut body);

        font_bbox.union(&bbox);
        char_procs.push(CharProc {
            name: name.clone(),
            width: advance,
            bbox,
            body,
        });
        encoding.push((code, name));
        widths.push(advance);
    }

    if font_bbox.is_empty() {
        // No glyphs encoded, or every glyph collapsed to (0,0,0,0).
        // Hand the consumer a well-formed zero bbox rather than
        // infinity sentinels.
        font_bbox = Bbox {
            xmin: 0.0,
            ymin: 0.0,
            xmax: 0.0,
            ymax: 0.0,
        };
    }

    Type3Font {
        bbox: font_bbox,
        matrix: font_matrix(upem),
        char_procs,
        encoding,
        widths,
    }
}

/// Glyph index alias. sigilbuzz uses raw `u16` glyph ids throughout;
/// this alias makes the public PDF surface read as "glyph id" rather
/// than "some sixteen-bit number".
pub type GlyphId = u16;

/// Axis-aligned bounding box in glyph-design-unit space.
///
/// Coordinates follow OpenType convention (y-up). The PDF
/// `FontMatrix` is responsible for transforming this space into
/// PDF-text space at use time, so [`Bbox`] does not need to be
/// flipped before emission.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bbox {
    /// Minimum x.
    pub xmin: f32,
    /// Minimum y.
    pub ymin: f32,
    /// Maximum x.
    pub xmax: f32,
    /// Maximum y.
    pub ymax: f32,
}

impl Bbox {
    /// Empty bbox suitable as an accumulator seed. Both extents are
    /// set such that the first [`Bbox::extend`] always wins on every
    /// coordinate.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            xmin: f32::INFINITY,
            ymin: f32::INFINITY,
            xmax: f32::NEG_INFINITY,
            ymax: f32::NEG_INFINITY,
        }
    }

    /// Returns true when no points have been folded in yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        !(self.xmin <= self.xmax && self.ymin <= self.ymax)
    }

    /// Folds a single point into the bbox.
    pub fn extend(&mut self, x: f32, y: f32) {
        if x < self.xmin {
            self.xmin = x;
        }
        if y < self.ymin {
            self.ymin = y;
        }
        if x > self.xmax {
            self.xmax = x;
        }
        if y > self.ymax {
            self.ymax = y;
        }
    }

    /// Folds another bbox into this one.
    pub fn union(&mut self, other: &Bbox) {
        if other.is_empty() {
            return;
        }
        self.extend(other.xmin, other.ymin);
        self.extend(other.xmax, other.ymax);
    }
}

impl Default for Bbox {
    fn default() -> Self {
        Self::empty()
    }
}

/// One Type 3 `CharProc` content stream.
///
/// A `CharProc` is an entry in the Type 3 font's `/CharProcs`
/// dictionary. It records the per-glyph drawing program plus the
/// metrics the PDF renderer needs to advance the cursor and clip
/// against the glyph bbox.
#[derive(Debug, Clone, PartialEq)]
pub struct CharProc {
    /// PDF name used to key this CharProc in the font's `/CharProcs`
    /// dictionary. The name is also what the encoding's
    /// differences array points at.
    pub name: String,
    /// Horizontal advance width, in the same glyph-design-unit space
    /// as [`CharProc::bbox`]. The font's `FontMatrix` scales this
    /// into PDF text space.
    pub width: f32,
    /// Per-glyph bounding box in glyph-design-unit space.
    pub bbox: Bbox,
    /// PDF content-stream bytes — drawing operators ready to drop
    /// into the `/CharProcs` stream object verbatim.
    pub body: Vec<u8>,
}

/// A Type 3 font assembled from a sigilbuzz [`Face`] and a list of
/// glyph ids.
///
/// The fields are deliberately laid out so a downstream PDF
/// serialiser can pick each one up and write the corresponding PDF
/// dict entry without further bookkeeping:
///
/// | Field          | PDF dict entry             |
/// |----------------|----------------------------|
/// | `bbox`         | `/FontBBox [xmin ymin xmax ymax]` |
/// | `matrix`       | `/FontMatrix [a b c d e f]`       |
/// | `char_procs`   | `/CharProcs << name stream >>`    |
/// | `encoding`     | `/Encoding /Differences [...]`    |
/// | `widths`       | `/Widths [...]`                   |
///
/// [`Face`]: sigilbuzz::Face
#[derive(Debug, Clone, PartialEq)]
pub struct Type3Font {
    /// `FontBBox` in glyph-design-unit space — union of every
    /// included glyph's bbox.
    pub bbox: Bbox,
    /// `FontMatrix` mapping glyph design units to PDF text space.
    /// For a 1000-upem face this is the identity `1/1000`; for
    /// 2048-upem it is `1/2048`. PDF treats `FontMatrix` as a 3x3
    /// affine stored as the row-vector tuple `[a b c d e f]`.
    pub matrix: [f32; 6],
    /// One content stream per included glyph id, in input order.
    pub char_procs: Vec<CharProc>,
    /// Encoding map: PDF char code (1..=255) -> CharProc name. The
    /// emitter assigns codes sequentially starting at 1, leaving 0
    /// free for `.notdef`.
    pub encoding: Vec<(u8, String)>,
    /// Per-encoded-glyph advance widths in glyph-design-unit space.
    /// `widths.len() == char_procs.len()` — `FontMatrix` scales each
    /// entry into PDF text-space units at render time.
    pub widths: Vec<f32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_matrix_for_1000_upem_face() {
        let m = font_matrix(1000);
        assert_eq!(m, [0.001, 0.0, 0.0, 0.001, 0.0, 0.0]);
    }

    #[test]
    fn font_matrix_for_2048_upem_face() {
        let m = font_matrix(2048);
        let expected = 1.0_f32 / 2048.0_f32;
        assert_eq!(m, [expected, 0.0, 0.0, expected, 0.0, 0.0]);
    }

    #[test]
    fn bbox_extend_and_union_round_trip() {
        let mut a = Bbox::empty();
        assert!(a.is_empty());
        a.extend(10.0, -5.0);
        a.extend(40.0, 30.0);
        assert_eq!(a.xmin, 10.0);
        assert_eq!(a.ymin, -5.0);
        assert_eq!(a.xmax, 40.0);
        assert_eq!(a.ymax, 30.0);

        let mut b = Bbox::empty();
        b.extend(0.0, 100.0);
        a.union(&b);
        assert_eq!(a.xmin, 0.0);
        assert_eq!(a.ymax, 100.0);

        // Unioning an empty bbox is a no-op.
        let snapshot = a;
        a.union(&Bbox::empty());
        assert_eq!(a, snapshot);
    }
}
