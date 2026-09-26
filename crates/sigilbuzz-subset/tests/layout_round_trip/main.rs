//! Integration tests covering the `retain_layout` flag on
//! [`SubsetInput`] and the layout-table preservation rules.
//!
//! These tests document the 0.7.0 contract:
//!
//! - `retain_layout = false` is the 0.5.0 baseline: layout tables are
//!   dropped from the subset regardless of the closure size.
//! - `retain_layout = true` (the default) preserves `GSUB`, `GPOS`,
//!   and `GDEF` verbatim when the kept-gid set is the full font (the
//!   gid_map is identity). Under a proper subset, the byte-level
//!   rewriter rebuilds whatever layout content it has support for:
//!   today GSUB types 1/2/3/4/7 plus GPOS types 1/2/3/4/5/6/9 plus
//!   GDEF GlyphClassDef and MarkAttachClassDef. Lookup types without
//!   a rewriter drop, the drop cascade then drops empty subtables /
//!   lookups / features / scripts, and a layout table drops entirely
//!   when no script survives.

use sigilbuzz::tables::tag;
use sigilbuzz::{Blob, Face};
use sigilbuzz_subset::{subset, SubsetInput};

const OPEN_SANS: &[u8] = include_bytes!("../../../../tests/fixtures/opensans_regular.ttf");
const AMIRI: &[u8] = include_bytes!("../../../../tests/fixtures/amiri_regular.ttf");
const RUBIK: &[u8] = include_bytes!("../../../../tests/fixtures/rubik_vf.ttf");

mod context;
mod ligatures;
mod positioning;
mod substitution;

fn open_sans_face() -> Face<'static> {
    Face::parse_bytes(OPEN_SANS, 0).unwrap()
}

fn amiri_face() -> Face<'static> {
    Face::parse_bytes(AMIRI, 0).unwrap()
}

fn rubik_face() -> Face<'static> {
    Face::parse_bytes(RUBIK, 0).unwrap()
}

fn cmap_lookup(face: &Face<'_>, ch: char) -> u16 {
    face.cmap().unwrap().glyph_id(ch).unwrap()
}

#[test]
fn retain_layout_false_drops_gsub_gpos_gdef() {
    // Strict 0.5.0-compatible mode: layout tables are gone from the
    // output even when the source carries them.
    let face = open_sans_face();
    let gid_a = cmap_lookup(&face, 'A');
    let input = SubsetInput {
        gids: vec![gid_a],
        retain_hints: false,
        drop_unhandled: true,
        retain_layout: false,
        retain_variations: false,
    };
    let out = subset(&face, &input).unwrap();
    let blob = Blob::from_vec(out.bytes);
    let subset_face = Face::parse(&blob, 0).unwrap();
    assert!(subset_face.record(tag::GSUB).is_none());
    assert!(subset_face.record(tag::GPOS).is_none());
    assert!(subset_face.record(tag::GDEF).is_none());
}

#[test]
fn retain_layout_true_with_proper_subset_routes_through_rewriter() {
    // The 0.7.0 byte-level rewriter rebuilds whatever it has support
    // for. As of this commit:
    //   - GSUB type 1 (single-sub) survives when its rewritten lookup
    //     keeps at least one (input, output) pair. Other GSUB types
    //     drop. The drop cascade then drops empty lookups / features /
    //     scripts, and GSUB itself drops when no script survives.
    //   - GPOS types 1/2/3/4/5/6/9 have rewriters; types 7/8 (context)
    //     drop. For Open Sans -> {A, B} the few lookups that touch
    //     these glyphs end up empty after pair filtering and the drop
    //     cascade removes GPOS entirely.
    //   - GDEF GlyphClassDef and MarkAttachClassDef are rewritten via
    //     the auto-format ClassDef emitter; AttachList / LigCaretList /
    //     MarkGlyphSetsDef / ItemVariationStore drop.
    //
    // For a proper subset of Open Sans -> {A, B}, GPOS drops; GSUB may
    // or may not survive depending on whether any single-sub lookup
    // covers A or B; GDEF survives because Open Sans carries
    // GlyphClassDef.
    let face = open_sans_face();
    let gid_a = cmap_lookup(&face, 'A');
    let gid_b = cmap_lookup(&face, 'B');
    let input = SubsetInput {
        gids: vec![gid_a, gid_b],
        retain_hints: false,
        drop_unhandled: true,
        retain_layout: true,
        retain_variations: false,
    };
    let out = subset(&face, &input).unwrap();
    let blob = Blob::from_vec(out.bytes);
    let subset_face = Face::parse(&blob, 0).unwrap();
    // GPOS drops here because Open Sans's pair-pos / mark lookups
    // that cover A/B all empty out after filtering. None of the
    // surviving secondGlyphs / mark partners are in {A, B}. The drop
    // cascade then removes GPOS entirely.
    assert!(
        subset_face.record(tag::GPOS).is_none(),
        "GPOS expected to drop for the {{A, B}} subset of Open Sans",
    );
    // GDEF must survive: Open Sans carries a GlyphClassDef, the
    // ClassDef rewriter handles it.
    assert!(
        subset_face.record(tag::GDEF).is_some(),
        "GDEF should survive via ClassDef rewriter",
    );
    // GSUB may or may not survive depending on the source's lookups.
    // We don't pin the exact outcome, only that the subset built
    // cleanly and parses. (Tightening this assertion lands once the
    // remaining GSUB lookup types ship their rewriters.)
}

#[test]
fn retain_layout_true_with_identity_kept_set_preserves_layout_tables() {
    // The closure-equal-to-source case: every gid is kept, the gid_map
    // is identity, and the layout tables are pass-through.
    let face = open_sans_face();
    let num_glyphs = face.maxp().unwrap().num_glyphs;
    let gids: Vec<u16> = (1..num_glyphs).collect(); // .notdef is implicit
    let input = SubsetInput {
        gids,
        retain_hints: false,
        drop_unhandled: true,
        retain_layout: true,
        retain_variations: false,
    };
    let out = subset(&face, &input).unwrap();
    let blob = Blob::from_vec(out.bytes);
    let subset_face = Face::parse(&blob, 0).unwrap();

    // Open Sans carries GSUB / GPOS / GDEF. Verify the source has
    // them and the subset preserved them all.
    assert!(face.record(tag::GSUB).is_some());
    assert!(face.record(tag::GPOS).is_some());
    assert!(face.record(tag::GDEF).is_some());
    assert!(subset_face.record(tag::GSUB).is_some());
    assert!(subset_face.record(tag::GPOS).is_some());
    assert!(subset_face.record(tag::GDEF).is_some());

    // The preserved tables must be byte-identical to the source.
    let src_gsub = face.table_bytes(tag::GSUB).unwrap();
    let dst_gsub = subset_face.table_bytes(tag::GSUB).unwrap();
    assert_eq!(src_gsub, dst_gsub);
    let src_gpos = face.table_bytes(tag::GPOS).unwrap();
    let dst_gpos = subset_face.table_bytes(tag::GPOS).unwrap();
    assert_eq!(src_gpos, dst_gpos);
    let src_gdef = face.table_bytes(tag::GDEF).unwrap();
    let dst_gdef = subset_face.table_bytes(tag::GDEF).unwrap();
    assert_eq!(src_gdef, dst_gdef);
}

#[test]
fn retain_layout_default_is_true() {
    // Default `SubsetInput` should set retain_layout to true, matching
    // the documented 0.6.0 contract.
    let i = SubsetInput::default();
    assert!(i.retain_layout);
}

#[test]
fn proper_subset_yields_parseable_gdef() {
    // The GDEF rewriter rebuilds GlyphClassDef around the new gid
    // namespace. Given a small subset of Open Sans, the output GDEF
    // must round-trip through the parser.
    let face = open_sans_face();
    let gid_a = cmap_lookup(&face, 'A');
    let gid_b = cmap_lookup(&face, 'B');
    let input = SubsetInput {
        gids: vec![gid_a, gid_b],
        retain_hints: false,
        drop_unhandled: true,
        retain_layout: true,
        retain_variations: false,
    };
    let out = subset(&face, &input).unwrap();
    let blob = Blob::from_vec(out.bytes);
    let subset_face = Face::parse(&blob, 0).unwrap();
    if let Some(_rec) = subset_face.record(tag::GDEF) {
        // Source carries GDEF and the rewriter built a v1.0 around the
        // remapped ClassDef. Just assert it parses.
        let parsed = subset_face.gdef();
        assert!(
            parsed.is_ok(),
            "rewritten GDEF must parse: {:?}",
            parsed.err()
        );
    }
}

#[test]
fn proper_subset_is_byte_deterministic() {
    // Determinism guard: subsetting the same face with the same
    // input twice must produce byte-identical output.
    let face = open_sans_face();
    let gid_a = cmap_lookup(&face, 'A');
    let gid_b = cmap_lookup(&face, 'B');
    let gid_c = cmap_lookup(&face, 'C');
    let input = SubsetInput {
        gids: vec![gid_a, gid_b, gid_c],
        retain_hints: false,
        drop_unhandled: true,
        retain_layout: true,
        retain_variations: false,
    };
    let out1 = subset(&face, &input).unwrap();
    let out2 = subset(&face, &input).unwrap();
    assert_eq!(
        out1.bytes, out2.bytes,
        "subset output must be deterministic"
    );
}

#[test]
fn proper_subset_is_smaller_than_source() {
    // Regression for the 0.5.0 baseline: the subset must remain a
    // small fraction of the source even with the rewriter wired.
    let face = open_sans_face();
    let gid_a = cmap_lookup(&face, 'A');
    let gid_b = cmap_lookup(&face, 'B');
    let gid_c = cmap_lookup(&face, 'C');
    let input = SubsetInput {
        gids: vec![gid_a, gid_b, gid_c],
        retain_hints: false,
        drop_unhandled: true,
        retain_layout: true,
        retain_variations: false,
    };
    let out = subset(&face, &input).unwrap();
    let source_size = OPEN_SANS.len();
    let pct = (out.bytes.len() as f64 / source_size as f64) * 100.0;
    assert!(
        pct < 5.0,
        "subset should be < 5% of source, got {pct:.1}% ({} / {} bytes)",
        out.bytes.len(),
        source_size,
    );
}
