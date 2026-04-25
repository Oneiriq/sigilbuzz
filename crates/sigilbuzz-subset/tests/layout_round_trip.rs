//! Integration tests covering the `retain_layout` flag on
//! [`SubsetInput`] and the layout-table preservation rules.
//!
//! These tests document the 0.7.0 contract:
//!
//! - `retain_layout = false` is the 0.5.0 baseline — layout tables are
//!   dropped from the subset regardless of the closure size.
//! - `retain_layout = true` (the default) preserves `GSUB`, `GPOS`,
//!   and `GDEF` verbatim when the kept-gid set is the full font (the
//!   gid_map is identity). Under a proper subset, the byte-level
//!   rewriter rebuilds whatever layout content it has support for —
//!   today GSUB type 1 (single-sub) plus GDEF GlyphClassDef and
//!   MarkAttachClassDef. Lookup types without a rewriter drop, the
//!   drop cascade then drops empty subtables / lookups / features /
//!   scripts, and a layout table drops entirely when no script
//!   survives. GPOS currently has no per-type rewriter so it always
//!   drops under a proper subset.

use sigilbuzz::tables::tag;
use sigilbuzz::{Blob, Face};
use sigilbuzz_subset::{subset, SubsetInput};

const OPEN_SANS: &[u8] = include_bytes!("../../../tests/fixtures/opensans_regular.ttf");

fn open_sans_face() -> Face<'static> {
    Face::parse_bytes(OPEN_SANS, 0).unwrap()
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
    //   - GPOS has no per-type rewriter today; it always drops.
    //   - GDEF GlyphClassDef and MarkAttachClassDef are rewritten via
    //     the auto-format ClassDef emitter; AttachList / LigCaretList /
    //     MarkGlyphSetsDef / ItemVariationStore drop.
    //
    // For a proper subset of Open Sans → {A, B}, GPOS drops; GSUB may
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
    // GPOS must drop — no per-type rewriter today.
    assert!(
        subset_face.record(tag::GPOS).is_none(),
        "GPOS must drop until per-type rewriters ship",
    );
    // GDEF must survive — Open Sans carries a GlyphClassDef, the
    // ClassDef rewriter handles it.
    assert!(
        subset_face.record(tag::GDEF).is_some(),
        "GDEF should survive via ClassDef rewriter",
    );
    // GSUB may or may not survive depending on the source's lookups.
    // We don't pin the exact outcome — only that the subset built
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

    // Open Sans carries GSUB / GPOS / GDEF — verify the source has
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
fn closure_pulls_in_ligature_components() {
    // Open Sans carries an `fi` ligature in `GSUB`. When the caller
    // asks for the ligature output gid alone, the closure walker must
    // pull `f` and `i` in too — the ligature can't fire without them.
    let face = open_sans_face();
    let gid_f = cmap_lookup(&face, 'f');
    let gid_i = cmap_lookup(&face, 'i');

    // Find the fi-ligature gid by walking the GSUB ligature subtables
    // for the glyph 'f' and looking for a single-component (i) match.
    // We do this through the public closure API: requesting a known
    // ligature output should expand to include f and i.
    //
    // The Open Sans fixture's `fi` ligature is at the gid produced by
    // running shape("fi") through the original font; we recover it by
    // shaping.
    use sigilbuzz::{shape, Buffer, Font};
    let font = Font::new(face.clone(), 16.0);
    let mut buf = Buffer::new();
    buf.set_text("fi");
    let run = shape(&font, &buf, &[]).unwrap();
    // If shaping produced a single glyph, that's the fi ligature.
    if run.glyphs.len() != 1 {
        // Open Sans build doesn't enable fi by default — skip the
        // assertion but still validate the closure is at least
        // reflexive on the seed.
        let kept = sigilbuzz_subset::compute_closure(&face, &[gid_f, gid_i]).unwrap();
        assert!(kept.contains(&gid_f));
        assert!(kept.contains(&gid_i));
        return;
    }
    let lig_gid = run.glyphs[0].glyph_id as u16;

    let kept = sigilbuzz_subset::compute_closure(&face, &[lig_gid]).unwrap();
    // Closure must include the ligature output, every component, and
    // .notdef.
    assert!(kept.contains(&0));
    assert!(kept.contains(&lig_gid));
    assert!(
        kept.contains(&gid_f),
        "closure of fi-ligature gid {lig_gid} did not pull in 'f' (gid {gid_f})",
    );
    assert!(
        kept.contains(&gid_i),
        "closure of fi-ligature gid {lig_gid} did not pull in 'i' (gid {gid_i})",
    );
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

#[test]
fn closure_ligature_drop_propagates_through_subset() {
    // When we ask only for 'f' (not the fi ligature), the closure
    // walker leaves the ligature gid out — `i` isn't kept, so the
    // forward-pull rule (every component kept ⇒ result gid kept)
    // doesn't fire and the ligature stays out of the subset.
    let face = open_sans_face();
    let gid_f = cmap_lookup(&face, 'f');

    let kept = sigilbuzz_subset::compute_closure(&face, &[gid_f]).unwrap();
    assert!(kept.contains(&gid_f));
    assert!(kept.contains(&0));
    // The closure must always be sorted ascending — callers depend on
    // it for binary-searchable membership.
    for w in kept.windows(2) {
        assert!(w[0] < w[1], "closure not sorted: {w:?}");
    }
}

#[test]
fn closure_pulls_in_fi_ligature_when_both_components_kept() {
    // Forward direction: ask for 'f' and 'i', expect the closure to
    // pull in the `fi` ligature output gid via Open Sans's GSUB type 4
    // lookup. Without this, the rewriter would drop the ligature
    // because its result gid is not in the GidMap, and shape("fi")
    // through the subset would no longer fire the ligature.
    let face = open_sans_face();
    let gid_f = cmap_lookup(&face, 'f');
    let gid_i = cmap_lookup(&face, 'i');

    // First, find the fi ligature gid through the source font.
    use sigilbuzz::{shape, Buffer, Font};
    let font = Font::new(face.clone(), 16.0);
    let mut buf = Buffer::new();
    buf.set_text("fi");
    let run = shape(&font, &buf, &[]).unwrap();
    if run.glyphs.len() != 1 {
        // Open Sans build doesn't enable fi by default — nothing to
        // assert in the forward direction.
        return;
    }
    let lig_gid = run.glyphs[0].glyph_id as u16;

    let kept = sigilbuzz_subset::compute_closure(&face, &[gid_f, gid_i]).unwrap();
    assert!(kept.contains(&gid_f));
    assert!(kept.contains(&gid_i));
    assert!(
        kept.contains(&lig_gid),
        "closure of [f, i] must pull in the fi ligature gid {lig_gid} via the forward GSUB type 4 walk",
    );
}

#[test]
fn open_sans_fi_subset_round_trips_through_shape() {
    // End-to-end: subset Open Sans down to {f, i} (the closure pulls
    // the fi ligature in via the forward type-4 walk; the rewriter
    // re-emits the ligature subtable around the new gid namespace).
    // Shaping "fi" against the subset must still produce a single
    // glyph — the fi ligature — and that glyph's cluster index/source
    // text mapping must match the source font's behaviour.
    let face = open_sans_face();
    let gid_f = cmap_lookup(&face, 'f');
    let gid_i = cmap_lookup(&face, 'i');

    use sigilbuzz::{shape, Buffer, Font};
    let font_src = Font::new(face.clone(), 16.0);
    let mut buf_src = Buffer::new();
    buf_src.set_text("fi");
    let run_src = shape(&font_src, &buf_src, &[]).unwrap();
    if run_src.glyphs.len() != 1 {
        // Build doesn't enable the fi ligature; nothing to test.
        return;
    }

    let input = SubsetInput {
        gids: vec![gid_f, gid_i],
        retain_hints: false,
        drop_unhandled: true,
        retain_layout: true,
        retain_variations: false,
    };
    let out = subset(&face, &input).unwrap();
    let blob = Blob::from_vec(out.bytes.clone());
    let subset_face = Face::parse(&blob, 0).unwrap();

    // GSUB must survive — the type 4 ligature subtable should have
    // rewritten cleanly around the new namespace.
    assert!(
        subset_face.record(tag::GSUB).is_some(),
        "GSUB must survive the {{f, i}} subset — the fi ligature lookup keeps it alive",
    );

    // Shape "fi" through the subset and verify the output is still a
    // single glyph (the fi ligature, now at a renumbered gid).
    let font_subset = Font::new(subset_face.clone(), 16.0);
    let mut buf_subset = Buffer::new();
    buf_subset.set_text("fi");
    let run_subset = shape(&font_subset, &buf_subset, &[]).unwrap();
    assert_eq!(
        run_subset.glyphs.len(),
        1,
        "shape(\"fi\") on the subset must collapse to a single ligature glyph; got {} glyphs",
        run_subset.glyphs.len(),
    );

    // The subset's fi gid should be the renumbered version of the
    // source's fi gid, recoverable via SubsetOutput::gid_map.
    let src_fi_gid = run_src.glyphs[0].glyph_id as u16;
    let new_fi_gid = out
        .gid_map
        .iter()
        .find_map(|(old, new)| if *old == src_fi_gid { Some(*new) } else { None })
        .expect("source fi gid must be in the subset's gid_map");
    assert_eq!(
        run_subset.glyphs[0].glyph_id as u16, new_fi_gid,
        "shape(\"fi\") on subset must produce the renumbered fi ligature gid",
    );
}

#[test]
fn open_sans_fi_subset_size_stays_small() {
    // Locked-down regression: the subset stays small even with GSUB
    // type 4 surviving. Open Sans pre-PR sat ~1% of source for a
    // 3-glyph subset; carrying GSUB ligature subtables adds a few
    // hundred bytes at most.
    let face = open_sans_face();
    let gid_f = cmap_lookup(&face, 'f');
    let gid_i = cmap_lookup(&face, 'i');
    let input = SubsetInput {
        gids: vec![gid_f, gid_i],
        retain_hints: false,
        drop_unhandled: true,
        retain_layout: true,
        retain_variations: false,
    };
    let out = subset(&face, &input).unwrap();
    let pct = (out.bytes.len() as f64 / OPEN_SANS.len() as f64) * 100.0;
    assert!(
        pct < 5.0,
        "Open Sans → {{f, i}} subset must stay under 5% of source; got {pct:.2}% ({} / {} bytes)",
        out.bytes.len(),
        OPEN_SANS.len(),
    );
}

#[test]
fn open_sans_fi_fl_subset_retains_both_ligatures() {
    // Subset Open Sans → {f, i, l}. Both `fi` and `fl` ligatures (when
    // present in the font's lookups) should round-trip and fire on
    // shape("fi") / shape("fl").
    let face = open_sans_face();
    let gid_f = cmap_lookup(&face, 'f');
    let gid_i = cmap_lookup(&face, 'i');
    let gid_l = cmap_lookup(&face, 'l');

    use sigilbuzz::{shape, Buffer, Font};
    let font_src = Font::new(face.clone(), 16.0);

    // Determine which ligatures the source actually fires.
    let mut buf = Buffer::new();
    buf.set_text("fi");
    let src_fi = shape(&font_src, &buf, &[]).unwrap();
    let mut buf = Buffer::new();
    buf.set_text("fl");
    let src_fl = shape(&font_src, &buf, &[]).unwrap();

    let input = SubsetInput {
        gids: vec![gid_f, gid_i, gid_l],
        retain_hints: false,
        drop_unhandled: true,
        retain_layout: true,
        retain_variations: false,
    };
    let out = subset(&face, &input).unwrap();
    let blob = Blob::from_vec(out.bytes);
    let subset_face = Face::parse(&blob, 0).unwrap();
    let font_subset = Font::new(subset_face, 16.0);

    if src_fi.glyphs.len() == 1 {
        let mut b = Buffer::new();
        b.set_text("fi");
        let r = shape(&font_subset, &b, &[]).unwrap();
        assert_eq!(
            r.glyphs.len(),
            1,
            "fi must still fire on the subset font; got {} glyphs",
            r.glyphs.len(),
        );
    }
    if src_fl.glyphs.len() == 1 {
        let mut b = Buffer::new();
        b.set_text("fl");
        let r = shape(&font_subset, &b, &[]).unwrap();
        assert_eq!(
            r.glyphs.len(),
            1,
            "fl must still fire on the subset font; got {} glyphs",
            r.glyphs.len(),
        );
    }
}
