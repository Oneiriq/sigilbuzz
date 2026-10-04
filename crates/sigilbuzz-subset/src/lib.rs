//! sigilbuzz-subset: font subsetter and instancer for sigilbuzz.
//!
//! Given a [`Face`] and a set of glyph IDs to keep, [`subset`] produces a
//! smaller font containing only those glyphs, plus any glyphs they pull
//! in through composites and ligatures. Every table that survives is
//! rewritten so glyph references point at the new, compacted glyph order.
//! [`instance()`] bakes variable-font axis coordinates into a static font or
//! pins some axes and keeps the rest.
//!
//! # Pipeline
//!
//! ```text
//!   Face + SubsetInput
//!        |
//!        v
//!   closure walk     -- expand `gids` to include composite and
//!        |              ligature components
//!        v
//!   gid remap        -- new gid 0..N in old order, gid 0 always kept
//!        |
//!        v
//!   table emission   -- rebuild or pass through each table
//!        |
//!        v
//!   SFNT rebuild     -- new directory, table checksums,
//!        |              head.checkSumAdjustment
//!        v
//!   SubsetOutput { bytes, gid_map, warnings }
//! ```
//!
//! # Malformed data
//!
//! A malformed layout structure (a GDEF list or entry, a GSUB or GPOS
//! lookup or subtable, a Device table, an anchor), vertical metrics
//! table (`vhea`, `vmtx`, `VORG`, `VVAR`), `BASE`, or `STAT` is left
//! out of the output, the way HarfBuzz's sanitizer neuters it, instead
//! of failing the subset. Every piece left out this way is reported in
//! [`SubsetOutput::warnings`] with its table, byte offset and reason.
//!
//! [`instance()`] treats the vertical metrics tables the same way. A
//! `vhea` or `vmtx` it cannot read is left out with its partner, a
//! `VVAR` it cannot read is left out without its deltas being applied,
//! and a malformed `VORG` is left out. So is a malformed GDEF piece or
//! FeatureVariations record it rebuilds, a `BASE` whose variations it
//! cannot apply, and, in a partial instance, an `avar`, `HVAR`, `VVAR`
//! or `MVAR` it cannot rebuild. Each is reported in
//! [`InstancedOutput::warnings`]. Tables an instance passes through
//! unchanged are not read, so they are neither checked nor reported.
//!
//! # Work budgets
//!
//! A small font can describe far more work than its size: shared
//! subroutines, composite trees, tuples that each infer every point of
//! a glyph, records that share a variation row or a condition set. Each
//! costly part of [`instance()`] charges a budget scaled to the size of
//! the data it reads, with a fixed floor for small fonts:
//!
//! - the `glyf` bake: 8 units per byte of `glyf` and `gvar`, plus 2^22;
//! - the partial `gvar` rewrite: 8 units of inference and 16 decoded
//!   deltas per byte of `gvar`, plus 2^20 each;
//! - each variation store's rows: 8 units per byte of the store, plus
//!   2^20;
//! - CFF2 charstrings: 64 tokens per byte of `CFF2`, plus 2^22;
//! - layout walks (GPOS, GDEF carets, `BASE`, FeatureVariations): 2^24.
//!
//! Past a budget the instance degrades and warns rather than failing
//! (see [`InstancedOutput::warnings`] for what each one leaves), except
//! for CFF2 charstrings, which fail the instance. Real fonts use at
//! most about 12.5% of any budget, so none of this applies to them.
//!
//! # What happens to each table
//!
//! For TrueType (`glyf`) fonts:
//!
//! - Rebuilt: `cmap` (as format 4), `glyf` and `loca`, `hmtx` and `hhea`,
//!   `maxp`, `head`, and `post` (as format 3, without glyph names).
//! - Vertical metrics: `vmtx` is rebuilt for the kept glyphs with
//!   `vhea.numberOfLongVerMetrics` patched to match, and `VORG` keeps
//!   its default and the kept glyphs' entries, renumbered. A malformed
//!   `vhea`, `vmtx` or `VORG` (or a `vhea` or `vmtx` without its
//!   partner) is left out and reported in [`SubsetOutput::warnings`].
//! - Passed through: `name`, `OS/2`, and `STAT`, which names no glyphs.
//! - `BASE`: kept, with the reference glyph of each format 2
//!   `BaseCoord` renumbered. A coordinate whose reference glyph is not
//!   kept becomes format 1 with the same value. A `BASE` that cannot be
//!   walked is left out and reported in [`SubsetOutput::warnings`].
//!   `BASE` is an OpenType layout table, so
//!   [`SubsetInput::retain_layout`] set to `false` drops it with the
//!   others.
//! - Layout (`GSUB`, `GPOS`, `GDEF`): kept verbatim when every glyph
//!   survives, rewritten at the byte level when glyph IDs change. Set
//!   [`SubsetInput::retain_layout`] to `false` to drop them. A rebuilt
//!   mark attachment or PairPos format 1 subtable that outgrows its
//!   16-bit offsets is split into several; any other subtable that
//!   does fails the subset with [`SubsetError::Unsupported`] rather
//!   than wrap an offset.
//!   FeatureVariations (GSUB and GPOS 1.1) are kept, their feature and
//!   lookup indices remapped with the rest.
//! - Variations: `fvar` and `avar` pass through. `gvar` is rebuilt with one
//!   entry per kept glyph, `HVAR` and `VVAR` around fresh
//!   `DeltaSetIndexMap`s and a deduplicated `ItemVariationStore`, and
//!   `VARC` around the kept composites. `VVAR` keeps every map the
//!   source has (advance height, top and bottom side bearings, and the
//!   vertical origin) and goes with `vmtx`: a subset without `vmtx`
//!   has no `VVAR`. A `VVAR` left out that way, or one that cannot be
//!   rebuilt, is reported in the warnings. The GDEF
//!   `ItemVariationStore` that GPOS kerning, anchors, and ligature
//!   carets vary through is carried verbatim. Set
//!   [`SubsetInput::retain_variations`] to `false` to drop them (and
//!   `MVAR`) and get a static subset at the default instance.
//! - Dropped when [`SubsetInput::drop_unhandled`] is true (the default):
//!   every table without a subset implementation. That is `kern`,
//!   `kerx`, `morx`, and the other AAT tables; `COLR` and `CPAL`; `MVAR`
//!   and `cvar`; the bitmap tables (`CBDT`, `CBLC`, `EBDT`, `EBLC`,
//!   `EBSC`, `sbix`) and `SVG `; `MATH` and `JSTF`; `gasp`, `hdmx`,
//!   `LTSH`, `VDMX`, and `DSIG`; and any table not named above. With the
//!   flag off, any of these returns [`SubsetError::Unsupported`].
//!
//! For CFF and CFF2 fonts:
//!
//! - A kept `CFF ` glyph whose charstring ends in the seac form of
//!   `endchar` keeps the base and accent glyphs it draws.
//! - If every glyph survives, the font passes through and only the SFNT
//!   directory is rebuilt. Every table is copied unchanged except
//!   `kern`, `kerx`, and `morx`, and the layout tables (`BASE`
//!   included) and variation tables (`MVAR` included) that
//!   [`SubsetInput::retain_layout`] and
//!   [`SubsetInput::retain_variations`] drop. Tables the rest of this
//!   list drops as unhandled, such as `COLR`, `CPAL`, `MVAR`, and
//!   `DSIG`, stay, since no glyph id changes, though a `DSIG` signature
//!   no longer matches the rebuilt file. Nothing is read, so nothing is
//!   reported in [`SubsetOutput::warnings`].
//! - Otherwise the CFF or CFF2 table is rebuilt around the kept glyphs.
//!   That covers non-CID and CID-keyed CFF (FDArray and FDSelect) as well
//!   as CFF2, with subroutines renumbered and unused ones dropped. `cmap`,
//!   `hmtx`, `hhea`, `maxp`, and `post` are rebuilt too. Vertical metrics,
//!   `BASE`, `STAT`, layout and variation tables, and the drop list
//!   follow the same rules as for `glyf` fonts. A `CFF2` table keeps
//!   its own variation data.
//!
//! The byte-level building blocks ([`encode_index`], [`encode_dict_int`],
//! [`emit_charset_auto`], [`emit_encoding_auto`], [`emit_fd_select_auto`],
//! [`renumber_charstring`], [`emit_classdef`], [`emit_coverage_from_glyphs`],
//! [`emit_coverage_from_pairs`]) are public so other tools can reuse them.
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
//!     retain_layout: true,
//!     retain_variations: true,
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

mod avar;
mod base;
mod cff;
mod cff2;
mod classdef;
mod closure;
mod cmap;
mod coverage;
mod device;
mod feature_variations;
mod fvar;
mod gdef;
mod glyf;
mod gpos;
mod gpos_var;
mod gsub;
mod gvar;
mod gvar_partial;
mod hmtx;
mod hvar;
mod instance;
mod layout;
mod lookup_list;
mod offset16;
mod read;
mod sfnt;
mod util;
mod varc;
mod variation_store;
mod vmtx;
mod vorg;
mod warnings;

pub use cff::subset_non_identity as subset_cff1_non_identity;
pub use cff::{
    compute_kept_subrs, emit_charset_auto, emit_charset_format0, emit_charset_format2,
    emit_encoding_auto, emit_encoding_format0, emit_encoding_format1, emit_fd_select_auto,
    emit_fd_select_format0, emit_fd_select_format3, encode_dict_int,
    encode_dict_offset_placeholder, encode_index, encode_int_operand, parse_fd_select,
    patch_dict_offset, renumber_charstring, renumber_subr_call, scan_subr_calls, subr_bias,
    SubrCall, SubrKind,
};
pub use cff2::subset_non_identity as subset_cff2_non_identity;
pub use classdef::emit_classdef;
pub use closure::compute_closure;
pub use coverage::{emit_coverage_from_glyphs, emit_coverage_from_pairs};
pub use instance::{instance, AxisPin, F2Dot14, InstanceInput, InstancedOutput};
pub use warnings::SubsetWarning;

use warnings::Warnings;

/// Crate version, matching `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Glyph index alias. sigilbuzz uses raw `u16` glyph ids throughout.
pub type GlyphId = u16;

/// TrueType hinting tables that [`SubsetInput::retain_hints`] keeps or
/// drops along with the glyph instructions.
const HINTING_TABLES: [[u8; 4]; 3] = [*b"cvt ", *b"fpgm", *b"prep"];

/// Layout tables that [`SubsetInput::retain_layout`] keeps or drops:
/// the OpenType layout tables `GSUB`, `GPOS`, `GDEF` and `BASE`.
const LAYOUT_TABLES: [[u8; 4]; 4] = [tag::GSUB, tag::GPOS, tag::GDEF, tag::BASE];

/// Variable-font tables that [`SubsetInput::retain_variations`] keeps
/// or drops. `MVAR` has no subset implementation: only the CFF identity
/// passthrough keeps it, and the other paths drop it as unhandled (see
/// `check_unhandled_tables`).
const VARIATION_TABLES: [[u8; 4]; 7] = [
    tag::FVAR,
    tag::AVAR,
    tag::GVAR,
    tag::HVAR,
    tag::VVAR,
    tag::VARC,
    tag::MVAR,
];

/// Tables the subset always keeps unless they are malformed, when it
/// leaves them out with a warning: the vertical metrics, `BASE` (which
/// [`SubsetInput::retain_layout`] can drop too), and `STAT`. Strict
/// mode does not reject one that is missing from the output.
const KEPT_UNLESS_MALFORMED: [[u8; 4]; 5] =
    [tag::VHEA, tag::VMTX, tag::VORG, tag::BASE, base::STAT];

/// Subset configuration.
#[derive(Debug, Clone)]
pub struct SubsetInput {
    /// Set of input glyph ids to keep. Composite components and ligature
    /// pieces will be added automatically by the dependency walker.
    /// Glyph 0 (`.notdef`) is always retained even when omitted.
    pub gids: Vec<GlyphId>,
    /// If true, retain TrueType hinting: the instructions of simple
    /// and composite glyphs in `glyf`, plus the `cvt `, `fpgm`, and
    /// `prep` tables they depend on. Defaults to false, which strips
    /// the instructions and drops those tables. CFF and CFF2
    /// charstrings keep their hint operators either way.
    pub retain_hints: bool,
    /// If true, drop tables that don't have a subset implementation
    /// (the default). If false, encountering an unsupported table
    /// surfaces [`SubsetError::Unsupported`].
    pub drop_unhandled: bool,
    /// If true (the default), retain the OpenType layout tables
    /// (`GSUB`, `GPOS`, `GDEF`, and `BASE`), for `glyf`, CFF, and CFF2
    /// fonts alike. They pass through verbatim when every glyph
    /// survives and are rewritten for the new glyph ids otherwise. When
    /// false, the layout tables are dropped. `STAT` and the vertical
    /// metrics tables (`vhea`, `vmtx`, `VORG`) are not layout tables
    /// and stay either way. Callers that explicitly want a hint-free,
    /// layout-free subset (e.g. embedded PDF font streams) should set
    /// this to false.
    pub retain_layout: bool,
    /// If true (the default), retain variable-font tables (`fvar`,
    /// `avar`, `gvar`, `HVAR`, `VVAR`, `VARC`) so the resulting subset
    /// still varies under axis coordinates. `fvar` and `avar` are
    /// passed through verbatim; `gvar` is rebuilt with one entry per
    /// kept gid; `HVAR` and `VVAR` are rebuilt around fresh
    /// `DeltaSetIndexMap`s plus a deduped `ItemVariationStore`. When
    /// false, every variable-font table is dropped, `MVAR` included,
    /// and strict mode (`drop_unhandled` false) no longer rejects an
    /// `MVAR` it would otherwise have no way to keep. The resulting
    /// subset behaves as a static font pinned to the source's default
    /// instance. A `CFF2` table keeps its own variation store either
    /// way, since it is part of the outline data.
    pub retain_variations: bool,
}

impl Default for SubsetInput {
    fn default() -> Self {
        Self {
            gids: Vec::new(),
            retain_hints: false,
            drop_unhandled: true,
            retain_layout: true,
            retain_variations: true,
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
    /// Pieces of the source font left out of the subset because they
    /// could not be read, sorted by table and offset. Empty for a well
    /// formed font. At most 65,536 are kept. See [`SubsetWarning`].
    pub warnings: Vec<SubsetWarning>,
}

/// Errors that can stop a subset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubsetError {
    /// Subset gids were empty after closure (caller passed nothing,
    /// not even `.notdef`). The subsetter implicitly retains gid 0,
    /// so this only fires when the caller's slice is empty
    /// and `drop_unhandled` is false.
    EmptyGidSet,
    /// A gid past the source font's `numGlyphs` was supplied. Also
    /// reported for gid 0 when the source font has no glyphs at all.
    GidOutOfRange {
        /// The offending gid.
        gid: GlyphId,
        /// The source font's glyph count.
        num_glyphs: u16,
    },
    /// The source font uses a feature sigilbuzz-subset cannot yet
    /// process, or data it cannot rewrite safely. Examples: a table
    /// without a subset implementation when `drop_unhandled` is false,
    /// a composite glyph that names a glyph past `numGlyphs`, or tables
    /// that share data so heavily that the rebuilt copy would explode
    /// in size.
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
    // CFF / CFF2 sources route through a dedicated path. The byte-level
    // emitter primitives (CharStrings INDEX rebuild, Subr renumber via
    // bias-adjusted operand rewrite, charset / Encoding / FDSelect
    // format auto-pick, deferred-offset Top DICT patching, Private DICT
    // relocation, FDArray INDEX rebuild with FD renumber) live in
    // [`cff`] and [`cff2`]; the cross-cutting orchestration consumes
    // them under a non-identity gid map for both non-CID and CID-keyed
    // CFF1 plus CFF2.
    //
    // The dispatch handles three cases:
    //
    // 1. The kept gid set after closure is the source font's identity
    //    (every gid kept). The CFF / CFF2 table and its dependencies
    //    are passed through verbatim and the surrounding sfnt directory
    //    is rebuilt around them. This mirrors the
    //    [`layout::Decision::Preserve`] strategy for GSUB / GPOS / GDEF.
    // 2. CFF1 non-identity (non-CID and CID-keyed) routes through
    //    [`cff_non_identity`] which calls [`cff::subset_non_identity`].
    // 3. CFF2 non-identity routes through the same [`cff_non_identity`],
    //    which calls [`cff2::subset_non_identity`] for it.
    //
    // All three honor `retain_layout`, `retain_variations`, and
    // `drop_unhandled` the way the `glyf` path does.
    let has_cff1 = face.record(tag::CFF1).is_some();
    let has_cff2 = face.record(tag::CFF2).is_some();
    if has_cff1 || has_cff2 {
        let cff_num_glyphs = face.maxp()?.num_glyphs;
        // Validate gids before we touch closure / passthrough.
        for &g in &input.gids {
            if g >= cff_num_glyphs {
                return Err(SubsetError::GidOutOfRange {
                    gid: g,
                    num_glyphs: cff_num_glyphs,
                });
            }
        }
        if input.gids.is_empty() && !input.drop_unhandled {
            return Err(SubsetError::EmptyGidSet);
        }
        let kept = closure::compute_closure(face, &input.gids)?;
        let identity = kept.len() == cff_num_glyphs as usize
            && kept.iter().enumerate().all(|(i, &g)| g as usize == i);
        if identity {
            return cff_passthrough(face, &kept, input);
        }
        // CFF2 non-identity mirrors the CID-keyed CFF1 flow with
        // CFF2-specific elisions (no String INDEX, no Encoding/charset,
        // single inline Top DICT). VariationStore rides through verbatim.
        return cff_non_identity(face, &kept, input, has_cff1);
    }

    let maxp = face.maxp()?;
    let num_glyphs = maxp.num_glyphs;

    // Validate caller-supplied gids before we touch anything else.
    for &g in &input.gids {
        if g >= num_glyphs {
            return Err(SubsetError::GidOutOfRange { gid: g, num_glyphs });
        }
    }

    // Strict mode rejects an empty user-supplied gid list. In
    // permissive mode we silently keep just .notdef.
    if input.gids.is_empty() && !input.drop_unhandled {
        return Err(SubsetError::EmptyGidSet);
    }

    // Closure walk: expand to include composite components, layout
    // partners, and VARC components. Gid 0 is always retained per the
    // SFNT convention.
    let kept = closure::compute_closure(face, &input.gids)?;

    // Build the old->new map. New gids are 0..N in old-order, with
    // gid 0 always at slot 0. The closure only returns gids below
    // `num_glyphs`, so every new gid and the count fit in u16.
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
    // values. They stay valid because the closure walker never
    // adds a *deeper* composite than the source font already had.
    let maxp_bytes = face.table_bytes(tag::MAXP).map_err(SubsetError::from)?;
    let mut maxp_out = maxp_bytes.to_vec();
    util::write_maxp_num_glyphs(&mut maxp_out, new_num_glyphs)?;

    // post: emit format 3 unconditionally. The subsetter does not
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

    let warnings = Warnings::default();
    push_vertical_tables(face, &kept, &gid_map, &warnings, &mut tables);
    push_base_and_stat(face, &gid_map, input, &warnings, &mut tables);
    push_layout_and_variation_tables(face, &kept, &gid_map, input, &warnings, &mut tables)?;

    // TrueType hinting tables. Kept glyph instructions call functions
    // from `fpgm`, run after `prep`, and read `cvt `. None of the three
    // names a glyph id, so they pass through verbatim when the
    // instructions are kept, and are dropped with them otherwise.
    if input.retain_hints {
        for t in HINTING_TABLES {
            if face.record(t).is_some() {
                let bytes = face.table_bytes(t).map_err(SubsetError::from)?;
                tables.push((t, bytes.to_vec()));
            }
        }
    }

    check_unhandled_tables(face, &tables, input)?;

    // Build the new font.
    let bytes = sfnt::build(face.sfnt_version(), &tables);

    Ok(SubsetOutput {
        bytes,
        gid_map: gid_map.into_iter().collect(),
        warnings: warnings.into_sorted(),
    })
}

/// Appends the rebuilt vertical metrics tables (`vhea`, `vmtx`, and
/// `VORG`) for the kept glyphs. Shared by the `glyf`, CFF, and CFF2
/// paths. A table that is malformed, or a `vhea` or `vmtx` without its
/// partner, is left out and recorded in `warnings`; it never fails the
/// subset.
fn push_vertical_tables(
    face: &Face<'_>,
    kept: &[GlyphId],
    gid_map: &[(GlyphId, GlyphId)],
    warnings: &Warnings,
    tables: &mut Vec<([u8; 4], Vec<u8>)>,
) {
    if let Some(metrics) = vmtx::subset_vertical_metrics(face, kept, warnings) {
        tables.push((tag::VHEA, metrics.vhea));
        tables.push((tag::VMTX, metrics.vmtx));
    }
    if let Some(vorg) = vorg::subset_vorg(face, gid_map, warnings) {
        tables.push((tag::VORG, vorg));
    }
}

/// Appends `BASE`, with the glyph ids of its format 2 coordinates
/// renumbered, unless [`SubsetInput::retain_layout`] drops it, and
/// `STAT`, which names no glyphs and passes through. Shared by the
/// `glyf`, CFF, and CFF2 paths. A `BASE` that cannot be walked, or a
/// `STAT` that cannot be read, is left out and recorded in `warnings`;
/// neither fails the subset.
fn push_base_and_stat(
    face: &Face<'_>,
    gid_map: &[(GlyphId, GlyphId)],
    input: &SubsetInput,
    warnings: &Warnings,
    tables: &mut Vec<([u8; 4], Vec<u8>)>,
) {
    if input.retain_layout {
        if let Some(b) = base::subset_base(face, gid_map, warnings) {
            tables.push((tag::BASE, b));
        }
    }
    if let Some(b) = base::subset_stat(face, warnings) {
        tables.push((base::STAT, b));
    }
}

/// Appends the layout and variable-font tables the subset keeps, as
/// [`SubsetInput::retain_layout`] and [`SubsetInput::retain_variations`]
/// ask. Shared by the `glyf`, CFF, and CFF2 paths: every table here is
/// keyed by glyph id, not by outline format. Malformed layout pieces
/// left out of the rewrite are recorded in `warnings`.
fn push_layout_and_variation_tables(
    face: &Face<'_>,
    kept: &[GlyphId],
    gid_map: &[(GlyphId, GlyphId)],
    input: &SubsetInput,
    warnings: &Warnings,
    tables: &mut Vec<([u8; 4], Vec<u8>)>,
) -> Result<(), SubsetError> {
    // Layout tables (GSUB / GPOS / GDEF). Identity gid map: pass the
    // source bytes through verbatim. Non-identity: per-lookup-type
    // rewriters in `crate::gsub` / `crate::gpos` / `crate::gdef`
    // produce fresh bytes; lookup types without a rewriter drop and
    // the drop cascade propagates the loss up. See `layout::decide`.
    let plan = layout::decide(face, kept, input, warnings)?;
    for (t, decision) in [
        (tag::GDEF, plan.gdef),
        (tag::GSUB, plan.gsub),
        (tag::GPOS, plan.gpos),
    ] {
        match decision {
            layout::Decision::Preserve => {
                let bytes = face.table_bytes(t).map_err(SubsetError::from)?;
                tables.push((t, bytes.to_vec()));
            }
            layout::Decision::Rewrite(b) => tables.push((t, b)),
            layout::Decision::Drop => {}
        }
    }

    // Variable-font tables. fvar/avar pass through verbatim;
    // gvar/HVAR/VVAR are rebuilt around the new gid namespace. When
    // `retain_variations` is false we drop them all and the subset
    // becomes a static-instance font.
    if !input.retain_variations {
        return Ok(());
    }
    if let Some(b) = fvar::subset_fvar(face)? {
        tables.push((tag::FVAR, b));
    }
    if let Some(b) = avar::subset_avar(face)? {
        tables.push((tag::AVAR, b));
    }
    if let Some(b) = gvar::subset_gvar(face, kept)? {
        tables.push((tag::GVAR, b));
    }
    if let Some(b) = hvar::subset_hvar(face, kept)? {
        tables.push((tag::HVAR, b));
    }
    // VVAR varies `vmtx` (and `VORG`), so it stays only next to the
    // `vmtx` that `push_vertical_tables` emitted.
    if tables.iter().any(|(t, _)| *t == tag::VMTX) {
        if let Some(b) = hvar::subset_vvar(face, kept, warnings) {
            tables.push((tag::VVAR, b));
        }
    } else if face.record(tag::VVAR).is_some() {
        warnings.push(
            tag::VVAR,
            0,
            "VVAR without a vmtx table to vary",
            "the whole table",
        );
    }
    // VARC: re-emit when the source carries the table. Coverage
    // entries renumber per the new gid map and component records
    // are rewritten to point at the new gid namespace; the
    // MultiVarStore is pruned to the entries the kept records use. The
    // core parse only validates the table: `subset_varc` walks the raw
    // bytes itself.
    if face.varc()?.is_some() {
        let varc_bytes = face.table_bytes(tag::VARC).map_err(SubsetError::from)?;
        let lookup = |old: GlyphId| -> Option<GlyphId> {
            let i = gid_map.binary_search_by_key(&old, |(o, _)| *o).ok()?;
            gid_map.get(i).map(|&(_, new)| new)
        };
        if let Some(b) = varc::subset_varc(varc_bytes, kept, &lookup)? {
            tables.push((tag::VARC, b));
        }
    }
    Ok(())
}

/// In strict mode (`drop_unhandled` false), fails on any source table
/// the subset did not emit. Layout, variable-font, and hinting tables
/// are exempt: their own flags decide whether they stay, so dropping
/// them is intended. `MVAR` is the exception: it has no subset
/// implementation, so it is exempt only when `retain_variations` drops
/// it. The vertical metrics tables, `BASE` and `STAT` are exempt too:
/// the subset always keeps them unless they are malformed, and then
/// reports them in its warnings. In permissive mode the rest are
/// dropped, because their glyph id references would be stale.
fn check_unhandled_tables(
    face: &Face<'_>,
    tables: &[([u8; 4], Vec<u8>)],
    input: &SubsetInput,
) -> Result<(), SubsetError> {
    if input.drop_unhandled {
        return Ok(());
    }
    for rec in face.records() {
        let emitted = tables.iter().any(|(t, _)| *t == rec.tag);
        let variation_exempt = VARIATION_TABLES.contains(&rec.tag)
            && (rec.tag != tag::MVAR || !input.retain_variations);
        let exempt = LAYOUT_TABLES.contains(&rec.tag)
            || variation_exempt
            || HINTING_TABLES.contains(&rec.tag)
            || KEPT_UNLESS_MALFORMED.contains(&rec.tag);
        if !emitted && !exempt {
            // kern / COLR / CPAL / morx / kerx / MVAR and others have
            // no subset implementation.
            return Err(SubsetError::Unsupported(
                "table not yet handled by sigilbuzz-subset; pass drop_unhandled=true",
            ));
        }
    }
    Ok(())
}

/// CFF1 and CFF2 non-identity orchestration.
///
/// Rebuilds the `CFF ` table with [`cff::subset_non_identity`], or the
/// `CFF2` table with [`cff2::subset_non_identity`], around the kept-gid
/// set. A CFF2 table carries its own variation store, which rides
/// through inside the rebuilt table. The fresh SFNT directory around
/// it rebuilds `cmap`, `hmtx` / `hhea`, `vmtx` / `vhea`, `VORG`,
/// `maxp`, and `post` for the new gid namespace, passes `head`, `name`,
/// and `OS/2` through, and keeps the layout and variable-font tables
/// that `input` asks for, as the `glyf` path does.
fn cff_non_identity(
    face: &Face<'_>,
    kept: &[GlyphId],
    input: &SubsetInput,
    has_cff1: bool,
) -> Result<SubsetOutput, SubsetError> {
    // Build the gid_map.
    let mut gid_map: Vec<(GlyphId, GlyphId)> = kept
        .iter()
        .enumerate()
        .map(|(new, &old)| (old, new as u16))
        .collect();
    gid_map.sort_by_key(|(old, _)| *old);
    let new_num_glyphs = kept.len() as u16;

    // Rebuild the CFF or CFF2 table.
    let (cff_tag, new_cff) = if has_cff1 {
        let bytes = face.table_bytes(tag::CFF1).map_err(SubsetError::from)?;
        (tag::CFF1, cff::subset_non_identity(bytes, kept)?)
    } else {
        let bytes = face.table_bytes(tag::CFF2).map_err(SubsetError::from)?;
        (tag::CFF2, cff2::subset_non_identity(bytes, kept)?)
    };

    // Rebuild the directly-rewritable tables that ride alongside the CFF.
    let head_bytes = face.table_bytes(tag::HEAD).map_err(SubsetError::from)?;
    let head_out = head_bytes.to_vec();

    let cmap_out = cmap::subset_cmap(face, &gid_map)?;
    let hmtx_out = hmtx::subset_hmtx(face, kept)?;

    let hhea_bytes = face.table_bytes(tag::HHEA).map_err(SubsetError::from)?;
    let mut hhea_out = hhea_bytes.to_vec();
    util::write_hhea_metrics_count(&mut hhea_out, hmtx_out.number_of_h_metrics)?;

    let maxp_bytes = face.table_bytes(tag::MAXP).map_err(SubsetError::from)?;
    let mut maxp_out = maxp_bytes.to_vec();
    util::write_maxp_num_glyphs(&mut maxp_out, new_num_glyphs)?;

    let post_out = util::synthesize_post_format_3(face)?;

    let name_out = face
        .table_bytes(tag::NAME)
        .map(|b| b.to_vec())
        .map_err(SubsetError::from)?;
    let os2_out = face.table_bytes(*b"OS/2").ok().map(<[u8]>::to_vec);

    let mut tables: Vec<([u8; 4], Vec<u8>)> = alloc::vec![
        (tag::HEAD, head_out),
        (tag::HHEA, hhea_out),
        (tag::MAXP, maxp_out),
        (tag::HMTX, hmtx_out.bytes),
        (tag::CMAP, cmap_out),
        (tag::NAME, name_out),
        (tag::POST, post_out),
        (cff_tag, new_cff),
    ];
    if let Some(os2) = os2_out {
        tables.push((*b"OS/2", os2));
    }

    let warnings = Warnings::default();
    push_vertical_tables(face, kept, &gid_map, &warnings, &mut tables);
    push_base_and_stat(face, &gid_map, input, &warnings, &mut tables);
    push_layout_and_variation_tables(face, kept, &gid_map, input, &warnings, &mut tables)?;
    check_unhandled_tables(face, &tables, input)?;

    let bytes = sfnt::build(face.sfnt_version(), &tables);
    Ok(SubsetOutput {
        bytes,
        gid_map: gid_map.into_iter().collect(),
        warnings: warnings.into_sorted(),
    })
}

/// CFF / CFF2 identity-passthrough emitter.
///
/// When the closure walker has retained every glyph in the source font
/// (kept set == `0..num_glyphs`), every byte of the CFF / CFF2 table,
/// including its embedded gid references inside charstrings, charset,
/// Encoding, and Private DICTs, already resolves to the right glyph
/// in the subset (the gid namespace is unchanged). Likewise for cmap,
/// hmtx, hhea, maxp, post, name, OS/2, vhea, vmtx, VORG, COLR, CPAL,
/// and the layout tables. We copy every table the source carries
/// except the small set the rest of the pipeline can't round-trip:
/// legacy `kern` / `morx` / `kerx`. Layout tables (`BASE` included)
/// and variable-font tables (`VVAR` and `MVAR` included) stay only when
/// `input` asks for them, and strict mode (`drop_unhandled` false)
/// rejects the tables that cannot be kept. A `DSIG` is copied with the
/// rest, so its signature goes stale.
///
/// Returns the new SFNT bytes plus an identity `gid_map`.
fn cff_passthrough(
    face: &Face<'_>,
    kept: &[GlyphId],
    input: &SubsetInput,
) -> Result<SubsetOutput, SubsetError> {
    let mut tables: Vec<([u8; 4], Vec<u8>)> = Vec::new();
    for rec in face.records() {
        let dropped_by_flag = (!input.retain_layout && LAYOUT_TABLES.contains(&rec.tag))
            || (!input.retain_variations && VARIATION_TABLES.contains(&rec.tag));
        // Skip the legacy kerning and AAT tables the rest of the
        // pipeline can't round-trip. CFF identity-passthrough is
        // morally a "preserve everything still relevant" emit, so the
        // vertical metrics tables stay: with every glyph kept their
        // glyph ids are already right.
        let unhandled = matches!(&rec.tag, b"kern" | b"morx" | b"kerx");
        if dropped_by_flag || unhandled {
            continue;
        }
        let bytes = face.table_bytes(rec.tag).map_err(SubsetError::from)?;
        tables.push((rec.tag, bytes.to_vec()));
    }
    check_unhandled_tables(face, &tables, input)?;
    let bytes = sfnt::build(face.sfnt_version(), &tables);
    let gid_map: Vec<(GlyphId, GlyphId)> = kept.iter().map(|&g| (g, g)).collect();
    Ok(SubsetOutput {
        bytes,
        gid_map,
        warnings: Vec::new(),
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
        assert!(i.retain_layout);
        assert!(i.retain_variations);
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
        assert!(format!("{e}").contains('9'));
        let e = SubsetError::Unsupported("foo");
        assert!(format!("{e}").contains("foo"));
        let e = SubsetError::MissingTable(*b"glyf");
        assert!(format!("{e}").contains("glyf"));
    }
}
