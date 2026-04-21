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

pub mod avar;
pub mod cmap;
pub mod fvar;
pub mod gdef;
pub mod glyf;
pub mod gpos;
pub mod gsub;
pub mod head;
pub mod hhea;
pub mod hmtx;
pub mod hvar;
pub mod kern;
pub mod layout;
pub mod loca;
pub mod maxp;
pub mod parse;
pub mod variation_store;

pub use avar::Avar;
pub use cmap::Cmap;
pub use fvar::{Fvar, VariationAxis};
pub use gdef::{Gdef, GlyphClass};
pub use glyf::{Glyf, GlyphBounds};
pub use gpos::{
    Anchor, Gpos, MarkAttachment, MarkBasePos, MarkLigaPos, MarkMarkPos, PairPos, SinglePos,
    ValueRecord,
};
pub use gsub::{Alternate, ChainContext, Gsub, Ligature, Multiple, Single};
pub use head::{Head, IndexToLocFormat};
pub use hhea::Hhea;
pub use hmtx::Hmtx;
pub use hvar::Hvar;
pub use kern::KernTable;
pub use layout::{ClassDef, Coverage};
pub use loca::Loca;
pub use maxp::Maxp;
pub use parse::Reader;
pub use variation_store::ItemVariationStore;

/// Standard SFNT / OpenType table tags. These are the ones sigilbuzz
/// reaches for during shaping; more land as the corresponding parsers
/// come online.
pub mod tag {
    /// `cmap` — character to glyph index mapping.
    pub const CMAP: [u8; 4] = *b"cmap";
    /// `head` — font header.
    pub const HEAD: [u8; 4] = *b"head";
    /// `hhea` — horizontal header.
    pub const HHEA: [u8; 4] = *b"hhea";
    /// `hmtx` — horizontal metrics.
    pub const HMTX: [u8; 4] = *b"hmtx";
    /// `maxp` — maximum profile (glyph count, etc.).
    pub const MAXP: [u8; 4] = *b"maxp";
    /// `name` — naming table.
    pub const NAME: [u8; 4] = *b"name";
    /// `post` — PostScript information.
    pub const POST: [u8; 4] = *b"post";
    /// `loca` — index to location (TrueType outlines).
    pub const LOCA: [u8; 4] = *b"loca";
    /// `glyf` — glyph data (TrueType outlines).
    pub const GLYF: [u8; 4] = *b"glyf";
    /// `GSUB` — glyph substitution (ligatures, contextual alternates).
    pub const GSUB: [u8; 4] = *b"GSUB";
    /// `GPOS` — glyph positioning (kerning, mark attachment).
    pub const GPOS: [u8; 4] = *b"GPOS";
    /// `GDEF` — glyph definition (class, caret, mark attachment).
    pub const GDEF: [u8; 4] = *b"GDEF";
    /// `kern` — legacy kerning table.
    pub const KERN: [u8; 4] = *b"kern";
    /// `fvar` — font variations axes and named instances.
    pub const FVAR: [u8; 4] = *b"fvar";
    /// `avar` — axis variations remapping.
    pub const AVAR: [u8; 4] = *b"avar";
    /// `HVAR` — horizontal metrics variations.
    pub const HVAR: [u8; 4] = *b"HVAR";
}
