//! sigilbuzz-subset — font subsetter for sigilbuzz.
//!
//! Given a [`Face`] and a set of glyph ids to keep, produces a smaller
//! font containing only those glyphs (plus any glyphs they reference
//! transitively through composites and ligatures). All retained tables
//! are rewritten so old glyph-id references resolve against the new,
//! compacted glyph order.
//!
//! # Pipeline
//!
//! ```text
//!   Face + SubsetInput
//!        │
//!        ▼
//!   closure walk     ── expand `gids` to include composite and
//!        │              ligature components
//!        ▼
//!   gid remap        ── new gid 0..N in old-order; gid 0 always kept
//!        │
//!        ▼
//!   table emission   ── cmap / glyf+loca / hmtx+hhea / maxp / head /
//!        │              name / OS/2 / post
//!        ▼
//!   SFNT rebuild     ── new directory, table-level checksums,
//!        │              head.checkSumAdjustment
//!        ▼
//!   SubsetOutput { bytes, gid_map }
//! ```
//!
//! # Scope
//!
//! This is the first pass and intentionally narrow:
//!
//! - **Subsetted**: `cmap` (fmt 4 rebuilt), `glyf` + `loca`, `hmtx` +
//!   `hhea`, `maxp`, `head`, `post` (forced to format 3).
//! - **Preserved as-is**: `name`, `OS/2`.
//! - **Dropped** (when [`SubsetInput::drop_unhandled`] is true, the
//!   default): `GDEF`, `GSUB`, `GPOS`, `kern`, `vhea`, `vmtx`, `VORG`,
//!   `HVAR`, `gvar`, `COLR`, `CPAL`, `morx`, `kerx`, `fvar`, `avar`.
//!   When the flag is false, encountering any of these surfaces a
//!   [`SubsetError::Unsupported`] result.
//! - **Errors cleanly**: `CFF` / `CFF2` (subroutine renumbering is
//!   significantly more involved and lands in a future release).
//!
//! The deferred-table list is the agenda for 0.6.0+. Variable-font
//! subsetting (`gvar` / `HVAR`), CFF subsetting, and full GSUB / GPOS
//! gid remap are the headline items.
//!
//! # Quick start
//!
//! ```no_run
//! use sigilbuzz::{Blob, Face};
//! use sigilbuzz_subset::{subset, SubsetInput};
//!
//! let blob = Blob::from_path("./MyFont.ttf").unwrap();
//! let face = Face::parse_bytes(blob.as_bytes(), 0).unwrap();
//! let input = SubsetInput {
//!     gids: vec![0, 36, 37, 38], // .notdef + 'A' + 'B' + 'C'
//!     retain_hints: false,
//!     drop_unhandled: true,
//! };
//! let out = subset(&face, &input).unwrap();
//! std::fs::write("./MyFont.subset.ttf", &out.bytes).unwrap();
//! ```
//!
//! # Determinism
//!
//! Output is byte-deterministic for a given input face + gid set.
//! No `HashMap` iteration touches the output; gids walk in sorted
//! order, table directories are written sorted by tag.

#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

extern crate alloc;

use alloc::vec::Vec;

use sigilbuzz::tables::tag;
use sigilbuzz::Face;

mod closure;
mod cmap;
mod glyf;
mod hmtx;
mod sfnt;
mod util;

pub use closure::compute_closure;

/// Crate version, matching `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Glyph index alias. sigilbuzz uses raw `u16` glyph ids throughout.
pub type GlyphId = u16;

/// Subset configuration.
#[derive(Debug, Clone)]
pub struct SubsetInput {
    /// Set of input glyph ids to keep. Composite components and ligature
    /// pieces will be added automatically by the dependency walker.
    /// Glyph 0 (`.notdef`) is always retained even when omitted.
    pub gids: Vec<GlyphId>,
    /// If true, retain hinting / instructions. Defaults to false.
    /// Currently honoured for the simple-glyph path of `glyf`; CFF
    /// subsetting is unimplemented entirely.
    pub retain_hints: bool,
    /// If true, drop tables that don't have a subset implementation
    /// (the default). If false, encountering an unsupported table
    /// surfaces [`SubsetError::Unsupported`].
    pub drop_unhandled: bool,
}

impl Default for SubsetInput {
    fn default() -> Self {
        Self {
            gids: Vec::new(),
            retain_hints: false,
            drop_unhandled: true,
        }
    }
}

/// Resulting subset.
#[derive(Debug, Clone)]
pub struct SubsetOutput {
    /// New font binary (a complete SFNT).
    pub bytes: Vec<u8>,
    /// Mapping from old gid to new gid for every kept glyph, sorted
    /// by old gid. Glyph 0 always maps to glyph 0.
    pub gid_map: Vec<(GlyphId, GlyphId)>,
}

/// Errors that can stop a subset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubsetError {
    /// Subset gids were empty after closure (caller passed nothing,
    /// not even `.notdef`). The subsetter implicitly retains gid 0,
    /// so this only fires when the caller's slice is genuinely empty
    /// and `drop_unhandled` is false.
    EmptyGidSet,
    /// A gid past the source font's `numGlyphs` was supplied.
    GidOutOfRange {
        /// The offending gid.
        gid: GlyphId,
        /// The source font's glyph count.
        num_glyphs: u16,
    },
    /// The source font uses a feature sigilbuzz-subset cannot yet
    /// process — typically CFF / CFF2 outlines or, with
    /// `drop_unhandled = false`, any of the deferred tables.
    Unsupported(&'static str),
    /// A sigilbuzz parse error bubbled up while inspecting the
    /// source font.
    Parse(sigilbuzz::Error),
    /// Tables required by the SFNT spec were missing from the
    /// source font (e.g. `head` or `maxp`).
    MissingTable([u8; 4]),
}

impl core::fmt::Display for SubsetError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::EmptyGidSet => f.write_str("subset gid set is empty"),
            Self::GidOutOfRange { gid, num_glyphs } => write!(
                f,
                "subset gid {gid} is past source font's numGlyphs={num_glyphs}",
            ),
            Self::Unsupported(c) => write!(f, "subset unsupported: {c}"),
            Self::Parse(e) => write!(f, "subset parse error: {e:?}"),
            Self::MissingTable(t) => write!(
                f,
                "subset source missing required table: {:?}",
                core::str::from_utf8(t).unwrap_or("????"),
            ),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for SubsetError {}

impl From<sigilbuzz::Error> for SubsetError {
    fn from(e: sigilbuzz::Error) -> Self {
        match e {
            sigilbuzz::Error::MissingTable { tag } => Self::MissingTable(tag),
            other => Self::Parse(other),
        }
    }
}

/// Subset `face` according to `input`. Returns the new font bytes
/// plus a gid remap.
pub fn subset(face: &Face<'_>, input: &SubsetInput) -> Result<SubsetOutput, SubsetError> {
    // Source must be a TrueType-outlined font: presence of CFF /
    // CFF2 means PostScript outlines and we don't subset those yet.
    if face.record(tag::CFF1).is_some() || face.record(tag::CFF2).is_some() {
        return Err(SubsetError::Unsupported(
            "CFF subsetting not yet implemented",
        ));
    }

    let maxp = face.maxp()?;
    let num_glyphs = maxp.num_glyphs;

    // Validate caller-supplied gids before we touch anything else.
    for &g in &input.gids {
        if g >= num_glyphs {
            return Err(SubsetError::GidOutOfRange { gid: g, num_glyphs });
        }
    }

    // Strict mode rejects an empty user-supplied gid list — in
    // permissive mode we silently keep just .notdef.
    if input.gids.is_empty() && !input.drop_unhandled {
        return Err(SubsetError::EmptyGidSet);
    }

    // Closure walk: expand to include composite components in glyf.
    // Gid 0 is always retained per the SFNT convention.
    let kept = closure::compute_closure(face, &input.gids)?;

    // Build the old->new map. New gids are 0..N in old-order, with
    // gid 0 always at slot 0.
    let mut gid_map: Vec<(GlyphId, GlyphId)> = kept
        .iter()
        .enumerate()
        .map(|(new, &old)| (old, new as u16))
        .collect();
    gid_map.sort_by_key(|(old, _)| *old);

    let new_num_glyphs: u16 = kept.len() as u16;

    // Per-table subsetting. Each helper returns the new bytes for
    // its table, or None if the table should not appear in the
    // output. Errors propagate up.
    let head_bytes = face.table_bytes(tag::HEAD).map_err(SubsetError::from)?;
    let mut head_out = head_bytes.to_vec();

    let glyf_loca = glyf::subset_glyf_loca(face, &kept, input.retain_hints)?;
    // glyf::subset_glyf_loca picks the loca format that fits; reflect
    // it back into head before emission.
    util::write_index_to_loc_format(&mut head_out, glyf_loca.long_loca);

    let cmap_out = cmap::subset_cmap(face, &gid_map)?;
    let hmtx_out = hmtx::subset_hmtx(face, &kept)?;

    // hhea bytes are passed through with numberOfHMetrics rewritten to
    // hmtx_out.number_of_h_metrics.
    let hhea_bytes = face.table_bytes(tag::HHEA).map_err(SubsetError::from)?;
    let mut hhea_out = hhea_bytes.to_vec();
    util::write_hhea_metrics_count(&mut hhea_out, hmtx_out.number_of_h_metrics)?;

    // maxp: rewrite numGlyphs, leave the rest untouched. v1.0 carries
    // a maxComponent depth tail we conservatively leave at the source
    // values — they stay valid because the closure walker never
    // adds a *deeper* composite than the source font already had.
    let maxp_bytes = face.table_bytes(tag::MAXP).map_err(SubsetError::from)?;
    let mut maxp_out = maxp_bytes.to_vec();
    util::write_maxp_num_glyphs(&mut maxp_out, new_num_glyphs)?;

    // post: emit format 3 unconditionally — the subsetter does not
    // attempt to preserve PostScript glyph names. This matches what
    // hb-subset's --no-glyph-names mode produces and is well-defined
    // by the spec.
    let post_out = util::synthesize_post_format_3(face)?;

    // Optional pass-throughs: name, OS/2.
    let name_out = face
        .table_bytes(tag::NAME)
        .map(|b| b.to_vec())
        .map_err(SubsetError::from)?;
    let os2_out = face.table_bytes(*b"OS/2").ok().map(<[u8]>::to_vec);

    // Assemble. SFNT directory is sorted by tag. We track which tables
    // landed in the output so the closure / gid_map result is honest.
    let mut tables: Vec<([u8; 4], Vec<u8>)> = alloc::vec![
        (tag::HEAD, head_out),
        (tag::HHEA, hhea_out),
        (tag::MAXP, maxp_out),
        (tag::HMTX, hmtx_out.bytes),
        (tag::CMAP, cmap_out),
        (tag::LOCA, glyf_loca.loca),
        (tag::GLYF, glyf_loca.glyf),
        (tag::NAME, name_out),
        (tag::POST, post_out),
    ];
    if let Some(os2) = os2_out {
        tables.push((*b"OS/2", os2));
    }

    // Walk every other table the source carries and decide.
    for rec in face.records() {
        // Skip tables we already emitted.
        if tables.iter().any(|(t, _)| *t == rec.tag) {
            continue;
        }
        // Source tables we already errored on (CFF/CFF2) cannot
        // appear here — we returned early above.
        if !input.drop_unhandled {
            // Strict mode: any table without an implementation aborts.
            // GSUB / GPOS / GDEF / kern / vhea / vmtx / VORG / HVAR /
            // gvar / COLR / CPAL / morx / kerx / fvar / avar — none
            // of these are subset-aware in 0.5.0.
            return Err(SubsetError::Unsupported(
                "table not yet handled by sigilbuzz-subset; pass drop_unhandled=true",
            ));
        }
        // Permissive mode: silently drop. We don't preserve them
        // verbatim because their gid references would be stale.
    }

    // Build the new font.
    let bytes = sfnt::build(face.sfnt_version(), &tables);

    Ok(SubsetOutput {
        bytes,
        gid_map: gid_map.into_iter().collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;

    #[test]
    fn input_default_drops_unhandled() {
        let i = SubsetInput::default();
        assert!(i.drop_unhandled);
        assert!(!i.retain_hints);
        assert!(i.gids.is_empty());
    }

    #[test]
    fn version_is_string() {
        assert!(!VERSION.is_empty());
    }

    #[test]
    fn error_display_renders() {
        let e = SubsetError::EmptyGidSet;
        assert_eq!(format!("{e}"), "subset gid set is empty");
        let e = SubsetError::GidOutOfRange {
            gid: 9,
            num_glyphs: 4,
        };
        assert!(format!("{e}").contains("9"));
        let e = SubsetError::Unsupported("foo");
        assert!(format!("{e}").contains("foo"));
        let e = SubsetError::MissingTable(*b"glyf");
        assert!(format!("{e}").contains("glyf"));
    }
}
