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
//!   today GSUB types 1/2/3/4/7 plus GPOS types 1/2/3/4/5/6/9 plus
//!   GDEF GlyphClassDef and MarkAttachClassDef. Lookup types without
//!   a rewriter drop, the drop cascade then drops empty subtables /
//!   lookups / features / scripts, and a layout table drops entirely
//!   when no script survives.

use sigilbuzz::tables::tag;
use sigilbuzz::{Blob, Face};
use sigilbuzz_subset::{subset, SubsetInput};

const OPEN_SANS: &[u8] = include_bytes!("../../../tests/fixtures/opensans_regular.ttf");
const AMIRI: &[u8] = include_bytes!("../../../tests/fixtures/amiri_regular.ttf");
const RUBIK: &[u8] = include_bytes!("../../../tests/fixtures/rubik_vf.ttf");

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
    //     drop. For Open Sans → {A, B} the few lookups that touch
    //     these glyphs end up empty after pair filtering and the drop
    //     cascade removes GPOS entirely.
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
    // GPOS drops here because Open Sans's pair-pos / mark lookups
    // that cover A/B all empty out after filtering — none of the
    // surviving secondGlyphs / mark partners are in {A, B}. The drop
    // cascade then removes GPOS entirely.
    assert!(
        subset_face.record(tag::GPOS).is_none(),
        "GPOS expected to drop for the {{A, B}} subset of Open Sans",
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

// ===== GSUB type 2 + type 3 round-trip coverage =====
//
// Open Sans only carries type 1 + type 4 lookups, so types 2 and 3 need
// other fixtures:
//
// - Amiri (already vendored for the Arabic shaper) carries type 2
//   `ccmp`-style decomposition lookups. Subsetting an Amiri Arabic
//   character pulls the type-2 sequence outputs through the closure
//   walker, then the rewriter rebuilds the multiple-sub subtable around
//   the renumbered gids.
// - Rubik VF (already vendored for variable-axis tests) carries a
//   type-3 `aalt` lookup that maps base Arabic letters to their
//   isolated/initial/medial/final alternates. Subsetting a base + a
//   selected alternate gid exercises the rewriter's "filter alternates,
//   keep survivors, drop empty sets" path; subsetting the base alone
//   exercises the closure walker's "default alternate (index 0) only"
//   pull.

#[test]
fn amiri_arabic_subset_keeps_gsub_with_type2_lookups() {
    // Pick an Arabic letter Amiri's type-2 lookup decomposes — covered
    // gids are uni08B6..uni08BA which decompose to a base + a small
    // mark. We feed the closure walker the input gid and trust it to
    // pull every sequence output through `pull_multiple`.
    let face = amiri_face();
    // Arabic small letter beh with hamza above (U+08B6) — first input
    // covered by the type-2 lookup we sampled above.
    let ch_input = char::from_u32(0x08B6).unwrap();
    let Some(input_gid) = face.cmap().unwrap().glyph_id(ch_input) else {
        // If the build flavor of Amiri here doesn't carry that codepoint,
        // skip — the assertion below is conditional on the lookup firing.
        return;
    };
    let input = SubsetInput {
        gids: vec![input_gid],
        retain_hints: false,
        drop_unhandled: true,
        retain_layout: true,
        retain_variations: false,
    };
    let out = subset(&face, &input).unwrap();
    let blob = Blob::from_vec(out.bytes);
    let subset_face = Face::parse(&blob, 0).unwrap();

    // GSUB must survive — the type-2 lookup keeps it alive (other types
    // beyond 1/2/3/4/7 drop, but at least one rewriter-handled lookup
    // covers our input gid).
    assert!(
        subset_face.record(tag::GSUB).is_some(),
        "GSUB must survive the Amiri type-2 subset",
    );

    // Walk the rewritten GSUB and verify at least one type-2 subtable
    // exists with a non-empty Coverage. We don't pin the exact byte
    // shape — only that the rewriter produced a parseable subtable.
    let gsub = subset_face.gsub().unwrap().expect("GSUB must parse");
    let lookups = gsub.lookup_list();
    let mut found_type2 = false;
    for li in 0..lookups.len() {
        let Some(lk) = lookups.get(li) else { continue };
        if lk.lookup_type() == sigilbuzz::tables::gsub::lookup_type::MULTIPLE {
            for si in 0..lk.subtable_count() {
                let Some(sub) = lk.subtable_bytes(si) else {
                    continue;
                };
                let parsed = sigilbuzz::tables::gsub::Multiple::parse(sub);
                assert!(
                    parsed.is_ok(),
                    "rewritten type-2 subtable must parse: {:?}",
                    parsed.err(),
                );
                found_type2 = true;
            }
        }
    }
    assert!(
        found_type2,
        "expected at least one surviving type-2 lookup in the rewritten GSUB",
    );
}

#[test]
fn amiri_arabic_subset_is_byte_deterministic() {
    // Determinism guard for the type-2 path: same input → same bytes.
    let face = amiri_face();
    let ch_input = char::from_u32(0x08B6).unwrap();
    let Some(input_gid) = face.cmap().unwrap().glyph_id(ch_input) else {
        return;
    };
    let input = SubsetInput {
        gids: vec![input_gid],
        retain_hints: false,
        drop_unhandled: true,
        retain_layout: true,
        retain_variations: false,
    };
    let a = subset(&face, &input).unwrap();
    let b = subset(&face, &input).unwrap();
    assert_eq!(a.bytes, b.bytes);
}

/// Walks the source font's GSUB and returns the alternate gids that
/// the first type-3 subtable produces for `input_gid` (in the original
/// gid namespace). Returns `None` when there is no type-3 lookup or
/// the input isn't covered.
fn rubik_type3_alternates(face: &Face<'_>, input_gid: u16) -> Option<Vec<u16>> {
    let gsub = face.gsub().ok().flatten()?;
    let lookups = gsub.lookup_list();
    for li in 0..lookups.len() {
        let lk = lookups.get(li)?;
        if lk.lookup_type() != sigilbuzz::tables::gsub::lookup_type::ALTERNATE {
            continue;
        }
        for si in 0..lk.subtable_count() {
            let sub = lk.subtable_bytes(si)?;
            let parsed = sigilbuzz::tables::gsub::Alternate::parse(sub).ok()?;
            // The Alternate parser exposes apply(gid, idx); enumerate
            // alternates until we hit None.
            let mut alts = Vec::new();
            for idx in 0..16u16 {
                match parsed.apply(input_gid, idx) {
                    Some(g) => alts.push(g),
                    None => break,
                }
            }
            if !alts.is_empty() {
                return Some(alts);
            }
        }
    }
    None
}

#[test]
fn rubik_aalt_subset_keeps_alternates_when_explicitly_requested() {
    // Rubik's type-3 lookup maps U+0628 (Arabic letter beh) to a set
    // of presentation-form alternates. Subsetting the base letter alone
    // exercises the closure walker's default-only pull (index 0 of the
    // alternate set). Subsetting the base + an explicitly-named
    // alternate exercises the rewriter's "keep alternates that survive
    // the GidMap, drop the rest" path.
    let face = rubik_face();
    let cmap = face.cmap().unwrap();
    let ch_input = char::from_u32(0x0628).unwrap();
    let Some(gid_input) = cmap.glyph_id(ch_input) else {
        return;
    };

    // Discover the alternate gids in the source font by walking the
    // type-3 lookup directly. This avoids relying on a post-table
    // glyph-name lookup that this build of sigilbuzz does not expose.
    let Some(alts) = rubik_type3_alternates(&face, gid_input) else {
        return;
    };
    if alts.len() < 2 {
        return; // need a non-default alternate to ask for explicitly
    }
    // Pick a *non-default* alternate — index 1 — so we can verify the
    // rewriter keeps the explicitly-requested one in addition to the
    // default that the closure walker pulls automatically.
    let alt_gid = alts[1];

    // Subset → {base, explicit alternate}. The rewriter's type-3 path
    // must keep the AlternateSet entry for the base, with at least one
    // surviving alternate (the one we asked for).
    let input = SubsetInput {
        gids: vec![gid_input, alt_gid],
        retain_hints: false,
        drop_unhandled: true,
        retain_layout: true,
        retain_variations: false,
    };
    let out = subset(&face, &input).unwrap();
    let blob = Blob::from_vec(out.bytes);
    let subset_face = Face::parse(&blob, 0).unwrap();
    assert!(
        subset_face.record(tag::GSUB).is_some(),
        "GSUB must survive the Rubik aalt subset",
    );

    // Find a surviving type-3 subtable and verify the renumbered base
    // gid still has an AlternateSet with at least one entry.
    let gsub = subset_face.gsub().unwrap().expect("GSUB must parse");
    let lookups = gsub.lookup_list();
    let mut found_alt_set = false;
    for li in 0..lookups.len() {
        let Some(lk) = lookups.get(li) else { continue };
        if lk.lookup_type() != sigilbuzz::tables::gsub::lookup_type::ALTERNATE {
            continue;
        }
        for si in 0..lk.subtable_count() {
            let Some(sub) = lk.subtable_bytes(si) else {
                continue;
            };
            let parsed = sigilbuzz::tables::gsub::Alternate::parse(sub).unwrap();
            // Translate the source base gid to its new gid via the
            // subset's gid_map.
            if let Some(new_input) =
                out.gid_map
                    .iter()
                    .find_map(|(o, n)| if *o == gid_input { Some(*n) } else { None })
            {
                if parsed.apply(new_input, 0).is_some() {
                    found_alt_set = true;
                }
            }
        }
    }
    assert!(
        found_alt_set,
        "rewritten type-3 subtable must still cover the base gid with at least one alternate",
    );
}

#[test]
fn rubik_aalt_subset_pulls_default_alternate_via_closure() {
    // Closure-walker rule for type 3: requesting only the base gid
    // pulls the *default* alternate (index 0 of the alternate set) into
    // the closure. We can only assert the default-pulled-in direction
    // here — Rubik has additional type-1 and type-4 lookups whose
    // forward pull rules may also drag the non-default alternates in
    // (a type-1 `init` form mapping, for instance, would pull its
    // output by the SINGLE rule). The "non-default alternates stay out"
    // direction is verified at the unit-test level in `gsub.rs` where
    // we control the lookup graph fully.
    let face = rubik_face();
    let cmap = face.cmap().unwrap();
    let ch_input = char::from_u32(0x0628).unwrap();
    let Some(gid_input) = cmap.glyph_id(ch_input) else {
        return;
    };
    let Some(alts) = rubik_type3_alternates(&face, gid_input) else {
        return;
    };
    if alts.is_empty() {
        return;
    }

    // Subset → {base only}. The closure walker pulls in the default
    // alternate via the type-3 walk.
    let input = SubsetInput {
        gids: vec![gid_input],
        retain_hints: false,
        drop_unhandled: true,
        retain_layout: true,
        retain_variations: false,
    };
    let out = subset(&face, &input).unwrap();

    let default_alt = alts[0];
    let in_subset = |old: u16| -> bool { out.gid_map.iter().any(|(o, _)| *o == old) };
    assert!(
        in_subset(default_alt),
        "closure walker must pull the default (index-0) alternate gid {default_alt} into the subset",
    );
}

#[test]
fn rubik_aalt_subset_is_byte_deterministic() {
    // Determinism guard for the type-3 path.
    let face = rubik_face();
    let cmap = face.cmap().unwrap();
    let ch_input = char::from_u32(0x0628).unwrap();
    let Some(gid_input) = cmap.glyph_id(ch_input) else {
        return;
    };
    let input = SubsetInput {
        gids: vec![gid_input],
        retain_hints: false,
        drop_unhandled: true,
        retain_layout: true,
        retain_variations: false,
    };
    let a = subset(&face, &input).unwrap();
    let b = subset(&face, &input).unwrap();
    assert_eq!(a.bytes, b.bytes);
}

#[test]
fn rubik_latin_subset_retains_gpos() {
    // Rubik VF carries pair-pos (type 2 fmt 1/2), single-adj (type 1
    // wrapped in extension), mark-base / mark-liga / mark-mark, and
    // an extension-wrapped pair-pos. With a broad Latin subset the
    // pair-pos lookups retain at least one (first, second) pair so
    // the rewriter emits a parseable GPOS table.
    let face = rubik_face();
    if face.gpos().ok().flatten().is_none() {
        return;
    }
    let cmap = face.cmap().unwrap();
    let mut gids: Vec<u16> = Vec::new();
    for c in 'A'..='Z' {
        if let Some(g) = cmap.glyph_id(c) {
            gids.push(g);
        }
    }
    for c in 'a'..='z' {
        if let Some(g) = cmap.glyph_id(c) {
            gids.push(g);
        }
    }
    if gids.is_empty() {
        return;
    }
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
    assert!(
        subset_face.record(tag::GPOS).is_some(),
        "GPOS must survive a Latin-letter subset of Rubik — \
         the pair-pos lookup retains at least one (first, second) pair",
    );
    // The rewritten GPOS must parse cleanly.
    let parsed = subset_face.gpos();
    assert!(parsed.is_ok(), "rewritten GPOS must parse: {:?}", parsed.err());
}

#[test]
fn rubik_latin_subset_gpos_is_byte_deterministic() {
    // Determinism guard for GPOS rewrites.
    let face = rubik_face();
    if face.gpos().ok().flatten().is_none() {
        return;
    }
    let cmap = face.cmap().unwrap();
    let mut gids: Vec<u16> = Vec::new();
    for c in 'A'..='Z' {
        if let Some(g) = cmap.glyph_id(c) {
            gids.push(g);
        }
    }
    for c in 'a'..='z' {
        if let Some(g) = cmap.glyph_id(c) {
            gids.push(g);
        }
    }
    if gids.is_empty() {
        return;
    }
    let input = SubsetInput {
        gids,
        retain_hints: false,
        drop_unhandled: true,
        retain_layout: true,
        retain_variations: false,
    };
    let a = subset(&face, &input).unwrap();
    let b = subset(&face, &input).unwrap();
    assert_eq!(a.bytes, b.bytes, "GPOS-bearing subset must be byte-deterministic");
}

#[test]
fn amiri_subset_gpos_round_trips() {
    // Amiri carries mark-to-base / mark-to-mark GPOS lookups. A
    // subset that keeps base glyphs must retain GPOS after the
    // rewriter ships, so the marks the closure pulls in still attach
    // correctly. We only check that GPOS parses post-subset; the
    // attachment math is covered by the unit-test fixtures.
    let face = amiri_face();
    if face.gpos().ok().flatten().is_none() {
        return;
    }
    // Pick a chunk of Arabic letters.
    let chars = [
        '\u{0627}', '\u{0628}', '\u{062A}', '\u{062B}', '\u{062C}', '\u{062D}',
        '\u{062E}', '\u{062F}',
    ];
    let cmap = face.cmap().unwrap();
    let mut gids: Vec<u16> = Vec::new();
    for ch in chars {
        if let Some(gid) = cmap.glyph_id(ch) {
            gids.push(gid);
        }
    }
    if gids.is_empty() {
        return;
    }
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
    if let Some(_) = subset_face.record(tag::GPOS) {
        let parsed = subset_face.gpos();
        assert!(parsed.is_ok(), "rewritten Amiri GPOS must parse: {:?}", parsed.err());
    }
}
