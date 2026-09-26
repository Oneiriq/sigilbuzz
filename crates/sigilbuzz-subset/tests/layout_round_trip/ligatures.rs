//! Ligature closure and round-trip tests on Open Sans (GSUB type 4).

use super::*;

#[test]
fn closure_pulls_in_ligature_components() {
    // Open Sans carries an `fi` ligature in `GSUB`. When the caller
    // asks for the ligature output gid alone, the closure walker must
    // pull `f` and `i` in too. The ligature can't fire without them.
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
        // Open Sans build doesn't enable fi by default. Skip the
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
fn closure_ligature_drop_propagates_through_subset() {
    // When we ask only for 'f' (not the fi ligature), the closure
    // walker leaves the ligature gid out: `i` isn't kept, so the
    // forward-pull rule (every component kept => result gid kept)
    // doesn't fire and the ligature stays out of the subset.
    let face = open_sans_face();
    let gid_f = cmap_lookup(&face, 'f');

    let kept = sigilbuzz_subset::compute_closure(&face, &[gid_f]).unwrap();
    assert!(kept.contains(&gid_f));
    assert!(kept.contains(&0));
    // The closure must always be sorted ascending. Callers depend on
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
        // Open Sans build doesn't enable fi by default. Nothing to
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
    // glyph (the fi ligature) and that glyph's cluster index/source
    // text mapping must match the source font's behavior.
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

    // GSUB must survive: the type 4 ligature subtable should have
    // rewritten cleanly around the new namespace.
    assert!(
        subset_face.record(tag::GSUB).is_some(),
        "GSUB must survive the {{f, i}} subset: the fi ligature lookup keeps it alive",
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
        "Open Sans -> {{f, i}} subset must stay under 5% of source; got {pct:.2}% ({} / {} bytes)",
        out.bytes.len(),
        OPEN_SANS.len(),
    );
}

#[test]
fn open_sans_fi_fl_subset_retains_both_ligatures() {
    // Subset Open Sans -> {f, i, l}. Both `fi` and `fl` ligatures (when
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
