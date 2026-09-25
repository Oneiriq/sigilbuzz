//! SFNT / OpenType table parsers.
//!
//! Each table parser lives in its own sub-module and builds on the
//! big-endian [`parse::Reader`]. Parsers accept a byte slice (the table's
//! contents, already located in the font blob by [`crate::Face`]) and
//! return a strongly-typed view whose accessors never allocate.
//!
//! Table tags are represented as `[u8; 4]` throughout. Comparisons are
//! done against byte literals such as `b"cmap"` so every tag site is
//! unambiguous and `no_std`-friendly.

// Each per-table submodule below is `pub` so companion crates
// (sigilbuzz-subset, sigilbuzz-paint, sigilbuzz-render, ...) can reach
// the parser types they need by full path. The headline names
// re-exported at the module root via `pub use` (`Cmap`, `Glyf`,
// `Colr`, `PathOp`, `MultiVarStore`, `ItemVariationStore`, `Reader`,
// ...) are the *public* surface. Those carry the stability commitment
// in `docs/STABILITY.md`.
//
// Submodules marked `#[doc(hidden)]` below contain additional
// internal helper types and parser machinery that are `pub` only so
// sibling crates can link to them. Their shape may move between
// releases; consumers should depend on the curated `tables::Foo`
// re-export rather than the `tables::foo::Foo` path. Hiding the
// module keeps the re-exports visible while suppressing rustdoc for
// the deep paths.
//
// `colr`, `cpal`, and `outline` stay visible because consumer code
// (sigilbuzz-paint, sigilbuzz-svg, sigilbuzz-render) walks the full
// submodule surface: variant enums (`ColrPaint`, `ColorLine`,
// `PathOp`) and small helper types are part of the published API,
// not internals.
#[doc(hidden)]
pub mod ankr;
#[doc(hidden)]
pub mod avar;
#[doc(hidden)]
pub mod base;
#[doc(hidden)]
pub mod cbdt;
#[doc(hidden)]
pub mod cblc;
#[doc(hidden)]
pub mod cff;
#[doc(hidden)]
pub mod cff2;
#[doc(hidden)]
pub mod cmap;
pub mod colr;
pub mod cpal;
#[doc(hidden)]
pub mod ebdt;
#[doc(hidden)]
pub mod eblc;
#[doc(hidden)]
pub mod fvar;
#[doc(hidden)]
pub mod gdef;
#[doc(hidden)]
pub mod glyf;
#[doc(hidden)]
pub mod gpos;
#[doc(hidden)]
pub mod gsub;
#[doc(hidden)]
pub mod gvar;
#[doc(hidden)]
pub mod head;
#[doc(hidden)]
pub mod hhea;
#[doc(hidden)]
pub mod hmtx;
#[doc(hidden)]
pub mod hvar;
#[doc(hidden)]
pub mod kern;
#[doc(hidden)]
pub mod kerx;
#[doc(hidden)]
pub mod layout;
#[doc(hidden)]
pub mod loca;
#[doc(hidden)]
pub mod math;
#[doc(hidden)]
pub mod maxp;
#[doc(hidden)]
pub mod morx;
#[doc(hidden)]
pub mod multi_var_store;
#[doc(hidden)]
pub mod mvar;
#[doc(hidden)]
pub mod name;
pub mod outline;
#[doc(hidden)]
pub mod parse;
#[doc(hidden)]
pub mod sbix;
#[doc(hidden)]
pub mod svg_table;
#[doc(hidden)]
pub mod varc;
#[doc(hidden)]
pub mod variation_store;
#[doc(hidden)]
pub mod vhea;
#[doc(hidden)]
pub mod vmtx;
#[doc(hidden)]
pub mod vorg;
#[doc(hidden)]
pub mod vvar;

pub use ankr::Ankr;
pub use avar::Avar;
pub use base::{Base, BaseAxis, BaseScript};
pub use cbdt::{Cbdt, GlyphBitmap, GlyphBitmapMetrics};
pub use cblc::{
    BigGlyphMetrics, BitmapSize, CbdtLocation, Cblc, SbitLineMetrics, SmallGlyphMetrics,
};
pub use cff::Cff;
pub use cff2::Cff2;
pub use cmap::Cmap;
pub use colr::{Colr, ColrPaint};
pub use cpal::{Color, Cpal};
pub use ebdt::{BitPacking, Ebdt, EbdtBitmap, EbdtMetrics};
pub use eblc::Eblc;
pub use fvar::{Fvar, VariationAxis};
pub use gdef::{Gdef, GlyphClass};
pub use glyf::{Glyf, GlyphBounds};
pub use gpos::{
    Anchor, Gpos, MarkAttachment, MarkBasePos, MarkLigaPos, MarkMarkPos, PairPos, SinglePos,
    ValueRecord,
};
pub use gsub::{Alternate, ChainContext, Gsub, Ligature, Multiple, Single};
pub use gvar::{Gvar, PointDelta};
pub use head::{Head, IndexToLocFormat};
pub use hhea::Hhea;
pub use hmtx::Hmtx;
pub use hvar::Hvar;
pub use kern::KernTable;
pub use kerx::{Kerx, Kerx4Action};
pub use layout::{ClassDef, Coverage};
pub use loca::Loca;
pub use math::{
    GlyphAssembly, GlyphConstruction, GlyphPart, KernSide, Math, MathConstants, MathGlyphInfo,
    MathGlyphVariant, MathKern, MathValue, MathVariants,
};
pub use maxp::Maxp;
pub use morx::Morx;
pub use multi_var_store::{MultiVarStore, SparseAxisCoord, SparseRegion};
pub use mvar::Mvar;
pub use name::{Name, NameRecord};
pub use outline::{Outline, OutlineSink, PathOp};
pub use parse::Reader;
pub use sbix::{Sbix, SbixGlyph, SbixStrike};
pub use svg_table::{Svg, SvgDocument};
pub use varc::{Varc, VarcComponent, VarcComposite};
pub use variation_store::ItemVariationStore;
pub use vhea::Vhea;
pub use vmtx::Vmtx;
pub use vorg::Vorg;
pub use vvar::Vvar;

/// Standard SFNT / OpenType table tags. These are the ones sigilbuzz
/// reaches for during shaping; more land as the corresponding parsers
/// come online.
pub mod tag {
    /// `cmap`: character to glyph index mapping.
    pub const CMAP: [u8; 4] = *b"cmap";
    /// `head`: font header.
    pub const HEAD: [u8; 4] = *b"head";
    /// `hhea`: horizontal header.
    pub const HHEA: [u8; 4] = *b"hhea";
    /// `hmtx`: horizontal metrics.
    pub const HMTX: [u8; 4] = *b"hmtx";
    /// `maxp`: maximum profile (glyph count, etc.).
    pub const MAXP: [u8; 4] = *b"maxp";
    /// `name`: naming table.
    pub const NAME: [u8; 4] = *b"name";
    /// `post`: PostScript information.
    pub const POST: [u8; 4] = *b"post";
    /// `loca`: index to location (TrueType outlines).
    pub const LOCA: [u8; 4] = *b"loca";
    /// `glyf`: glyph data (TrueType outlines).
    pub const GLYF: [u8; 4] = *b"glyf";
    /// `GSUB`: glyph substitution (ligatures, contextual alternates).
    pub const GSUB: [u8; 4] = *b"GSUB";
    /// `GPOS`: glyph positioning (kerning, mark attachment).
    pub const GPOS: [u8; 4] = *b"GPOS";
    /// `GDEF`: glyph definition (class, caret, mark attachment).
    pub const GDEF: [u8; 4] = *b"GDEF";
    /// `kern`: legacy kerning table.
    pub const KERN: [u8; 4] = *b"kern";
    /// `fvar`: font variations axes and named instances.
    pub const FVAR: [u8; 4] = *b"fvar";
    /// `avar`: axis variations remapping.
    pub const AVAR: [u8; 4] = *b"avar";
    /// `HVAR`: horizontal metrics variations.
    pub const HVAR: [u8; 4] = *b"HVAR";
    /// `gvar`: glyph variations (per-point outline deltas).
    pub const GVAR: [u8; 4] = *b"gvar";
    /// `MVAR`: metrics variations (font-wide instance metrics).
    pub const MVAR: [u8; 4] = *b"MVAR";
    /// `VVAR`: vertical metrics variations (advance height, tsb).
    pub const VVAR: [u8; 4] = *b"VVAR";
    /// `vhea`: vertical header.
    pub const VHEA: [u8; 4] = *b"vhea";
    /// `vmtx`: vertical metrics.
    pub const VMTX: [u8; 4] = *b"vmtx";
    /// `VORG`: vertical origin.
    pub const VORG: [u8; 4] = *b"VORG";
    /// `COLR`: layered color glyph table.
    pub const COLR: [u8; 4] = *b"COLR";
    /// `CPAL`: color palette table.
    pub const CPAL: [u8; 4] = *b"CPAL";
    /// `CFF `: Compact Font Format 1 (PostScript charstring outlines).
    pub const CFF1: [u8; 4] = *b"CFF ";
    /// `CFF2`: CFF2 for OpenType variable fonts with PostScript outlines.
    pub const CFF2: [u8; 4] = *b"CFF2";
    /// `morx`: Apple Extended Glyph Metamorphosis (AAT).
    pub const MORX: [u8; 4] = *b"morx";
    /// `kerx`: Apple Extended Kerning (AAT).
    pub const KERX: [u8; 4] = *b"kerx";
    /// `ankr`: Apple Anchor Point table (AAT). Pairs with `kerx`
    /// format-4 action type 1 (anchor-point kerning).
    pub const ANKR: [u8; 4] = *b"ankr";
    /// `CBLC`: Color Bitmap Location (Google).
    pub const CBLC: [u8; 4] = *b"CBLC";
    /// `CBDT`: Color Bitmap Data (Google).
    pub const CBDT: [u8; 4] = *b"CBDT";
    /// `EBLC`: Embedded Bitmap Location (Microsoft, monochrome).
    pub const EBLC: [u8; 4] = *b"EBLC";
    /// `EBDT`: Embedded Bitmap Data (Microsoft, monochrome).
    pub const EBDT: [u8; 4] = *b"EBDT";
    /// `sbix`: Standard Bitmap Graphics (Apple).
    pub const SBIX: [u8; 4] = *b"sbix";
    /// `SVG `: OpenType SVG (inline SVG documents per glyph).
    /// Note the trailing space: OpenType tags are exactly four bytes.
    pub const SVG: [u8; 4] = *b"SVG ";
    /// `MATH`: OpenType math typography table. Carried by math
    /// fonts (STIX 2 Math, Latin Modern Math, Cambria Math, ...).
    pub const MATH: [u8; 4] = *b"MATH";
    /// `BASE`: baseline metrics for cross-script alignment. Adobe's
    /// flagship faces and a smattering of Noto / SIL designs ship
    /// this; the majority of fonts omit it.
    pub const BASE: [u8; 4] = *b"BASE";
    /// `VARC`: Variable Composite Glyphs. OpenType 1.10 / 2024
    /// extension that carries axis-driven shape transformations on
    /// composite-glyph components.
    pub const VARC: [u8; 4] = *b"VARC";
}
