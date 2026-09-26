//! GSUB type 2 (multiple) and type 3 (alternate) round trips on Amiri and Rubik.

use super::*;

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
    // Pick an Arabic letter Amiri's type-2 lookup decomposes. Covered
    // gids are uni08B6..uni08BA which decompose to a base + a small
    // mark. We feed the closure walker the input gid and trust it to
    // pull every sequence output through `pull_multiple`.
    let face = amiri_face();
    // Arabic small letter beh with hamza above (U+08B6), first input
    // covered by the type-2 lookup we sampled above.
    let ch_input = char::from_u32(0x08B6).unwrap();
    let Some(input_gid) = face.cmap().unwrap().glyph_id(ch_input) else {
        // If the build flavor of Amiri here doesn't carry that codepoint,
        // skip. The assertion below is conditional on the lookup firing.
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

    // GSUB must survive: the type-2 lookup keeps it alive (other types
    // beyond 1/2/3/4/7 drop, but at least one rewriter-handled lookup
    // covers our input gid).
    assert!(
        subset_face.record(tag::GSUB).is_some(),
        "GSUB must survive the Amiri type-2 subset",
    );

    // Walk the rewritten GSUB and verify at least one type-2 subtable
    // exists with a non-empty Coverage. We don't pin the exact byte
    // shape, only that the rewriter produced a parseable subtable.
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
    // Determinism guard for the type-2 path: same input -> same bytes.
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
    // Pick a *non-default* alternate (index 1) so we can verify the
    // rewriter keeps the explicitly-requested one in addition to the
    // default that the closure walker pulls automatically.
    let alt_gid = alts[1];

    // Subset -> {base, explicit alternate}. The rewriter's type-3 path
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
    // here. Rubik has additional type-1 and type-4 lookups whose
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

    // Subset -> {base only}. The closure walker pulls in the default
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
