//! Robustness regressions for the shaping driver, the buffer, the
//! bidi algorithm, and the face directory.
//!
//! Each test feeds hostile text, a hand-built font, or a mutated copy
//! of a fixture that used to panic, hang, or allocate without bound.
//! The tests only check that the call returns with sane output. The
//! slow cases took seconds to hours before the fixes and now finish in
//! milliseconds.

use sigilbuzz::{
    shape, BidiInfo, BidiParagraph, Blob, Buffer, BufferFlags, ClusterLevel, Face, Feature, Font,
};

const AMIRI: &[u8] = include_bytes!("fixtures/amiri_regular.ttf");
const OPEN_SANS: &[u8] = include_bytes!("fixtures/opensans_regular.ttf");
const AAT_SYNTHETIC: &[u8] = include_bytes!("fixtures/aat_synthetic.ttf");

fn push16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_be_bytes());
}

fn push32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_be_bytes());
}

fn len16(n: usize) -> u16 {
    u16::try_from(n).expect("test table fits an Offset16")
}

/// Packs `(tag, body)` pairs into an SFNT with a TrueType header.
fn assemble_sfnt(tables: &[([u8; 4], Vec<u8>)]) -> Vec<u8> {
    let header_len = 12 + tables.len() * 16;
    let mut out = Vec::new();
    push32(&mut out, 0x0001_0000);
    push16(&mut out, len16(tables.len()));
    out.extend_from_slice(&[0; 6]);
    let mut offset = header_len;
    for (tag, body) in tables {
        out.extend_from_slice(tag);
        push32(&mut out, 0);
        push32(&mut out, u32::try_from(offset).unwrap());
        push32(&mut out, u32::try_from(body.len()).unwrap());
        offset += body.len();
    }
    for (_, body) in tables {
        out.extend_from_slice(body);
    }
    out
}

/// Copies every table out of `font`, replacing or adding the tables
/// in `overrides`.
fn with_tables(font: &[u8], overrides: &[([u8; 4], Vec<u8>)]) -> Vec<u8> {
    let face = Face::parse_bytes(font, 0).expect("fixture parses");
    let mut tables: Vec<([u8; 4], Vec<u8>)> = face
        .records()
        .iter()
        .filter(|r| overrides.iter().all(|(tag, _)| *tag != r.tag))
        .map(|r| (r.tag, face.table_bytes(r.tag).unwrap().to_vec()))
        .collect();
    tables.extend(overrides.iter().cloned());
    tables.sort_by_key(|(tag, _)| *tag);
    assemble_sfnt(&tables)
}

fn shape_glyphs(font_bytes: &[u8], text: &str, features: &[Feature]) -> Vec<sigilbuzz::Glyph> {
    let blob = Blob::new(font_bytes);
    let face = Face::parse(&blob, 0).expect("font parses");
    let font = Font::new(face, 1000.0);
    let mut buffer = Buffer::new();
    buffer.push_str(text);
    shape(&font, &buffer, features)
        .expect("shape succeeds")
        .glyphs
}

/// GDEF 1.0 with no class definitions.
fn empty_gdef() -> Vec<u8> {
    let mut out = Vec::new();
    for v in [1u16, 0, 0, 0, 0, 0] {
        push16(&mut out, v);
    }
    out
}

/// GPOS 1.0 with empty script, feature, and lookup lists.
fn empty_gpos() -> Vec<u8> {
    let mut out = Vec::new();
    for v in [1u16, 0, 10, 12, 14, 0, 0, 0] {
        push16(&mut out, v);
    }
    out
}

#[test]
fn arabic_after_decomposed_thai_vowel_does_not_panic() {
    // U+0E33 decomposes into two codepoints before cmap lookup. The
    // Arabic joining forms used to be computed per source character,
    // so the Arabic segment range ran one past the end of the forms.
    let glyphs = shape_glyphs(AMIRI, "\u{0E33}\u{0628}", &[]);
    assert_eq!(glyphs.len(), 3);
}

#[test]
fn arabic_forms_stay_aligned_after_decomposed_thai_vowel() {
    // Without the fix the two behs took the forms of the characters
    // one position to their left.
    let expected = shape_glyphs(AMIRI, "\u{0628}\u{0628} a", &[]);
    let got = shape_glyphs(AMIRI, "\u{0E33}\u{0628}\u{0628} a", &[]);
    let expected_ids: Vec<u32> = expected.iter().map(|g| g.glyph_id).collect();
    let got_ids: Vec<u32> = got[2..].iter().map(|g| g.glyph_id).collect();
    assert_eq!(got_ids, expected_ids);
}

#[test]
fn morx_ligature_with_gdef_does_not_panic() {
    // The morx `fi` ligature shrinks the run after the segment ranges
    // were recorded. The mark zeroing pass then sliced past the end.
    let font = with_tables(AAT_SYNTHETIC, &[(*b"GDEF", empty_gdef())]);
    let glyphs = shape_glyphs(&font, "fi", &[]);
    assert_eq!(glyphs.len(), 1);
    assert_eq!(glyphs[0].glyph_id, 3);
}

#[test]
fn morx_ligature_with_gpos_does_not_panic() {
    // Same shrink, caught by the GPOS pass slicing each segment.
    let font = with_tables(AAT_SYNTHETIC, &[(*b"GPOS", empty_gpos())]);
    let expected = shape_glyphs(AAT_SYNTHETIC, "fifi", &[]);
    let glyphs = shape_glyphs(&font, "fifi", &[]);
    assert!(glyphs.len() < 4);
    let ids: Vec<u32> = glyphs.iter().map(|g| g.glyph_id).collect();
    let expected_ids: Vec<u32> = expected.iter().map(|g| g.glyph_id).collect();
    assert_eq!(ids, expected_ids);
}

#[test]
fn long_digit_run_resolves_bidi_in_linear_time() {
    // W2 and W7 used to walk back to the start of the sequence for
    // every European number, which is quadratic on a long digit run.
    let text = "1".repeat(200_000);
    let info = BidiInfo::new(&text, None);
    assert_eq!(info.char_count(), 200_000);
    assert!(info.levels().iter().all(|&l| l == 0));

    let rtl = format!("\u{0627}{text}");
    let info = BidiInfo::new(&rtl, None);
    assert_eq!(info.levels()[0], 1);
    assert!(info.levels()[1..].iter().all(|&l| l == 2));
}

#[test]
fn long_fsi_run_resolves_bidi_in_linear_time() {
    // Every FSI used to scan to the end of the text for its first
    // strong character.
    let mut text = "\u{2068}".repeat(200_000);
    text.push('\u{05D0}');
    let info = BidiInfo::new(&text, None);
    assert_eq!(info.char_count(), 200_001);

    let paragraph = BidiParagraph::new(&text, None);
    let covered: usize = paragraph.runs().iter().map(|run| run.range.len()).sum();
    assert_eq!(covered, text.len());
    let visual: usize = paragraph.visual_runs().iter().map(|r| r.range.len()).sum();
    assert_eq!(visual, text.len());
}

#[test]
fn long_mark_run_shapes_in_linear_time() {
    // Mark-to-base used to walk back to the base for every mark and
    // then sum every advance in between.
    let mut text = String::from("\u{0628}");
    text.push_str(&"\u{064E}".repeat(20_000));
    let glyphs = shape_glyphs(AMIRI, &text, &[]);
    assert_eq!(glyphs.len(), 20_001);
}

#[test]
fn long_joiner_run_shapes_in_linear_time() {
    // The default-ignorable check used a linear scan per glyph.
    let text = "\u{200D}".repeat(200_000);
    let glyphs = shape_glyphs(OPEN_SANS, &text, &[]);
    assert_eq!(glyphs.len(), 200_000);
    assert!(glyphs.iter().all(|g| g.x_advance == 0));
}

/// GSUB whose DFLT LangSys repeats feature 0 `feature_repeats` times.
/// Feature 0 is `liga` and lists `lookup_count` distinct lookup
/// indices. The LookupList is empty, so none of them resolve.
fn gsub_with_repeated_feature(feature_repeats: usize, lookup_count: usize) -> Vec<u8> {
    let lookup_list = vec![0u8, 0];

    let mut feature_list = Vec::new();
    push16(&mut feature_list, 1);
    feature_list.extend_from_slice(b"liga");
    push16(&mut feature_list, 8);
    push16(&mut feature_list, 0);
    push16(&mut feature_list, len16(lookup_count));
    for i in 0..lookup_count {
        push16(&mut feature_list, len16(i));
    }

    let mut script_list = Vec::new();
    push16(&mut script_list, 1);
    script_list.extend_from_slice(b"DFLT");
    push16(&mut script_list, 8);
    // Script table: default LangSys right after its 4-byte header.
    push16(&mut script_list, 4);
    push16(&mut script_list, 0);
    // LangSys.
    push16(&mut script_list, 0);
    push16(&mut script_list, 0xFFFF);
    push16(&mut script_list, len16(feature_repeats));
    for _ in 0..feature_repeats {
        push16(&mut script_list, 0);
    }

    let lookup_off = 10;
    let feature_off = lookup_off + lookup_list.len();
    let script_off = feature_off + feature_list.len();
    let mut out = Vec::new();
    for v in [
        1,
        0,
        len16(script_off),
        len16(feature_off),
        len16(lookup_off),
    ] {
        push16(&mut out, v);
    }
    out.extend_from_slice(&lookup_list);
    out.extend_from_slice(&feature_list);
    out.extend_from_slice(&script_list);
    out
}

#[test]
fn repeated_feature_indices_collect_lookups_quickly() {
    // Collecting lookup indices used `Vec::contains` for every entry
    // of every repeated feature record: 30 000 x 1 000 x 1 000 steps.
    let gsub = gsub_with_repeated_feature(30_000, 1_000);
    let font = with_tables(OPEN_SANS, &[(*b"GSUB", gsub)]);
    let glyphs = shape_glyphs(&font, "fi", &[]);
    assert_eq!(glyphs.len(), 2);
}

/// GPOS whose DFLT LangSys enables eight features. Every feature
/// lists the same `lookup_count` lookups, and every lookup is one
/// SinglePos that adds 32767 to the advance of every glyph.
fn gpos_with_huge_single_adjustments(lookup_count: usize) -> Vec<u8> {
    let tags: [&[u8; 4]; 8] = [
        b"kern", b"dist", b"mark", b"mkmk", b"tst1", b"tst2", b"tst3", b"tst4",
    ];

    let mut script_list = Vec::new();
    push16(&mut script_list, 1);
    script_list.extend_from_slice(b"DFLT");
    push16(&mut script_list, 8);
    push16(&mut script_list, 4);
    push16(&mut script_list, 0);
    push16(&mut script_list, 0);
    push16(&mut script_list, 0xFFFF);
    push16(&mut script_list, len16(tags.len()));
    for i in 0..tags.len() {
        push16(&mut script_list, len16(i));
    }

    let mut feature_list = Vec::new();
    push16(&mut feature_list, len16(tags.len()));
    let feature_table_off = 2 + tags.len() * 6;
    for tag in tags {
        feature_list.extend_from_slice(tag);
        push16(&mut feature_list, len16(feature_table_off));
    }
    push16(&mut feature_list, 0);
    push16(&mut feature_list, len16(lookup_count));
    for i in 0..lookup_count {
        push16(&mut feature_list, len16(i));
    }

    let mut lookup_list = Vec::new();
    push16(&mut lookup_list, len16(lookup_count));
    let lookup_table_off = 2 + 2 * lookup_count;
    for _ in 0..lookup_count {
        push16(&mut lookup_list, len16(lookup_table_off));
    }
    // Lookup: type 1, flag 0, one subtable right after the header.
    for v in [1u16, 0, 1, 8] {
        push16(&mut lookup_list, v);
    }
    // SinglePos format 1: coverage at +10, XPlacement | XAdvance.
    for v in [1u16, 10, 0x0005] {
        push16(&mut lookup_list, v);
    }
    push16(&mut lookup_list, 0x7FFF);
    push16(&mut lookup_list, 0x7FFF);
    // Coverage format 2: every glyph id.
    for v in [2u16, 1, 0, 0xFFFF, 0] {
        push16(&mut lookup_list, v);
    }

    let script_off = 10;
    let feature_off = script_off + script_list.len();
    let lookup_off = feature_off + feature_list.len();
    let mut out = Vec::new();
    for v in [
        1,
        0,
        len16(script_off),
        len16(feature_off),
        len16(lookup_off),
    ] {
        push16(&mut out, v);
    }
    out.extend_from_slice(&script_list);
    out.extend_from_slice(&feature_list);
    out.extend_from_slice(&lookup_list);
    out
}

#[test]
fn repeated_single_adjustments_apply_each_shared_lookup_once() {
    // Eight features share one list of 16 000 lookups that each add
    // 32 767 units. Applying every feature's lookups separately gave
    // 8 x 16 000 x 32 767, which overflows i32, and debug builds
    // panicked on the addition. HarfBuzz runs each GPOS lookup of the
    // stage once however many features list it, so the adjustment
    // lands once. The saturating sums are covered by the unit test
    // `value_records_saturate_at_the_i32_bounds`.
    let gpos = gpos_with_huge_single_adjustments(16_000);
    let font = with_tables(OPEN_SANS, &[(*b"GPOS", gpos)]);
    let plain = shape_glyphs(OPEN_SANS, "A", &[]);
    let features: Vec<Feature> = [b"tst1", b"tst2", b"tst3", b"tst4"]
        .iter()
        .map(|tag| Feature {
            tag: **tag,
            value: 1,
        })
        .collect();
    let glyphs = shape_glyphs(&font, "A", &features);
    assert_eq!(glyphs.len(), 1);
    let once = 16_000 * 32_767;
    assert_eq!(glyphs[0].x_advance, plain[0].x_advance + once);
    assert_eq!(glyphs[0].x_offset, once);
}

/// GPOS with one DFLT feature `kern` that runs lookup 0, a single
/// lookup of `lookup_type` holding `subtable`.
fn gpos_with_one_lookup(lookup_type: u16, subtable: &[u8]) -> Vec<u8> {
    let mut script_list = Vec::new();
    push16(&mut script_list, 1);
    script_list.extend_from_slice(b"DFLT");
    for v in [8u16, 4, 0, 0, 0xFFFF, 1, 0] {
        push16(&mut script_list, v);
    }

    let mut feature_list = Vec::new();
    push16(&mut feature_list, 1);
    feature_list.extend_from_slice(b"kern");
    for v in [8u16, 0, 1, 0] {
        push16(&mut feature_list, v);
    }

    let mut lookup_list = Vec::new();
    for v in [1u16, 4, lookup_type, 0, 1, 8] {
        push16(&mut lookup_list, v);
    }
    lookup_list.extend_from_slice(subtable);

    let script_off = 10;
    let feature_off = script_off + script_list.len();
    let lookup_off = feature_off + feature_list.len();
    let mut out = Vec::new();
    for v in [
        1,
        0,
        len16(script_off),
        len16(feature_off),
        len16(lookup_off),
    ] {
        push16(&mut out, v);
    }
    out.extend_from_slice(&script_list);
    out.extend_from_slice(&feature_list);
    out.extend_from_slice(&lookup_list);
    out
}

#[test]
fn gpos_nested_fan_out_is_bounded() {
    // ChainContextPos format 3 whose one rule runs lookup 0 (itself)
    // eight times at the matched glyph: 8^16 nested calls before the
    // depth guard stops a branch.
    let mut chain = Vec::new();
    for v in [3u16, 0, 1, 44, 0, 8] {
        push16(&mut chain, v);
    }
    for _ in 0..8 {
        push16(&mut chain, 0);
        push16(&mut chain, 0);
    }
    for v in [2u16, 1, 0, 0xFFFF, 0] {
        push16(&mut chain, v);
    }
    let gpos = gpos_with_one_lookup(8, &chain);
    let font = with_tables(OPEN_SANS, &[(*b"GPOS", gpos)]);
    let expected = shape_glyphs(
        OPEN_SANS,
        "AV",
        &[Feature {
            tag: *b"kern",
            value: 0,
        }],
    );
    let glyphs = shape_glyphs(&font, "AV", &[]);
    let ids: Vec<u32> = glyphs.iter().map(|g| g.glyph_id).collect();
    let expected_ids: Vec<u32> = expected.iter().map(|g| g.glyph_id).collect();
    assert_eq!(ids, expected_ids);
}

/// A 100x100 square: one contour, four on-curve points.
fn square_glyph() -> Vec<u8> {
    let mut g = Vec::new();
    for v in [1i16, 0, 0, 100, 100] {
        g.extend_from_slice(&v.to_be_bytes());
    }
    push16(&mut g, 3);
    push16(&mut g, 0);
    g.extend_from_slice(&[0x01; 4]);
    for d in [0i16, 100, 0, -100, 0, 0, 100, 0] {
        g.extend_from_slice(&d.to_be_bytes());
    }
    g
}

/// A font whose VARC glyph `g` (1..=`depth`) has `fan_out` components
/// that all reference glyph `g + 1`. Glyph `depth + 1` is a square.
/// Resolving glyph 1 visits `fan_out ^ depth` leaves.
fn varc_fan_out_font(depth: u16, fan_out: usize) -> Vec<u8> {
    let num_glyphs = depth + 2;

    let mut head = Vec::new();
    push32(&mut head, 0x0001_0000);
    push32(&mut head, 0);
    push32(&mut head, 0);
    push32(&mut head, 0x5F0F_3CF5);
    push16(&mut head, 0);
    push16(&mut head, 1000);
    head.extend_from_slice(&[0; 16]);
    for v in [0u16, 0, 100, 100, 0, 8, 2, 0, 0] {
        push16(&mut head, v);
    }

    let mut maxp = Vec::new();
    push32(&mut maxp, 0x0000_5000);
    push16(&mut maxp, num_glyphs);

    let mut hhea = Vec::new();
    push32(&mut hhea, 0x0001_0000);
    for v in [800u16, 0xFF38, 0, 500, 0, 0, 500, 1, 0, 0, 0, 0, 0, 0, 0] {
        push16(&mut hhea, v);
    }
    push16(&mut hhea, num_glyphs);

    let mut hmtx = Vec::new();
    for _ in 0..num_glyphs {
        push16(&mut hmtx, 500);
        push16(&mut hmtx, 0);
    }

    // Every glyph but the last is empty in glyf.
    let square = square_glyph();
    let glyf = square.clone();
    let mut loca = Vec::new();
    for _ in 0..=depth {
        push16(&mut loca, 0);
    }
    push16(&mut loca, 0);
    push16(&mut loca, len16(square.len() / 2));

    // VARC: coverage 1..=depth, one record per covered glyph.
    let mut records: Vec<Vec<u8>> = Vec::new();
    for g in 1..=depth {
        let mut rec = Vec::new();
        for _ in 0..fan_out {
            rec.push(0x00);
            push16(&mut rec, g + 1);
        }
        records.push(rec);
    }
    let mut varc = Vec::new();
    push16(&mut varc, 1);
    push16(&mut varc, 0);
    let cov_slot = varc.len();
    push32(&mut varc, 0);
    push32(&mut varc, 0);
    push32(&mut varc, 0);
    push32(&mut varc, 0);
    let records_slot = varc.len();
    push32(&mut varc, 0);

    let cov_off = u32::try_from(varc.len()).unwrap();
    varc[cov_slot..cov_slot + 4].copy_from_slice(&cov_off.to_be_bytes());
    push16(&mut varc, 1);
    push16(&mut varc, depth);
    for g in 1..=depth {
        push16(&mut varc, g);
    }

    // glyphRecords: a CFF2-style INDEX with 4-byte offsets.
    let records_off = u32::try_from(varc.len()).unwrap();
    varc[records_slot..records_slot + 4].copy_from_slice(&records_off.to_be_bytes());
    push32(&mut varc, u32::from(depth));
    varc.push(4);
    let mut offset = 1u32;
    push32(&mut varc, offset);
    for rec in &records {
        offset += u32::try_from(rec.len()).unwrap();
        push32(&mut varc, offset);
    }
    for rec in &records {
        varc.extend_from_slice(rec);
    }

    assemble_sfnt(&[
        (*b"VARC", varc),
        (*b"glyf", glyf),
        (*b"head", head),
        (*b"hhea", hhea),
        (*b"hmtx", hmtx),
        (*b"loca", loca),
        (*b"maxp", maxp),
    ])
}

#[test]
fn varc_small_fan_out_still_resolves() {
    // 2 ^ 3 = 8 squares, well inside the component budget.
    let bytes = varc_fan_out_font(3, 2);
    let face = Face::parse_bytes(&bytes, 0).unwrap();
    let outline = face.glyph_outline(1).unwrap().expect("outline");
    let square = face.glyph_outline(4).unwrap().expect("square");
    assert_eq!(outline.ops().len(), 8 * square.ops().len());
}

#[test]
fn varc_exponential_fan_out_is_bounded() {
    // 4 ^ 40 leaves used to be walked one by one.
    let bytes = varc_fan_out_font(40, 4);
    let face = Face::parse_bytes(&bytes, 0).unwrap();
    assert!(face.glyph_outline(1).is_err());
}

#[test]
fn face_directory_claiming_many_tables_is_rejected() {
    // numTables = 65535 with no records behind it.
    let mut bytes = Vec::new();
    push32(&mut bytes, 0x0001_0000);
    push16(&mut bytes, 0xFFFF);
    bytes.extend_from_slice(&[0; 6]);
    assert!(Face::parse_bytes(&bytes, 0).is_err());
}

#[test]
fn collection_with_huge_member_index_is_rejected() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"ttcf");
    push32(&mut bytes, 0x0001_0000);
    push32(&mut bytes, u32::MAX);
    push32(&mut bytes, 16);
    assert!(Face::parse_bytes(&bytes, u32::MAX - 1).is_err());
    assert!(Face::parse_bytes(&bytes, u32::MAX).is_err());
}

/// Hostile strings for the buffer, bidi, script-run, NFC, and shape
/// paths.
fn hostile_strings() -> Vec<String> {
    let mut out: Vec<String> = [
        "",
        "\u{0301}",
        "\u{0301}\u{0301}\u{0301}",
        "\u{064E}\u{0628}",
        "\u{200D}\u{200C}\u{200E}\u{200F}\u{061C}",
        "((((((]]]]]]",
        "a(b[c{d)e]f}g",
        "\u{05D0}(\u{05D1}[\u{05D2})\u{05D3}]",
        "\u{2066}\u{2067}\u{2068}\u{2069}\u{2069}\u{2069}\u{2069}",
        "\u{202A}\u{202B}\u{202C}\u{202D}\u{202E}\u{202C}\u{202C}",
        "\u{0E33}\u{0EB3}\u{17C4}\u{17C5}\u{0BCA}\u{0DDA}\u{0DDD}",
        "\u{1100}\u{1161}\u{11A8}\u{1100}\u{1161}\u{D7CB}",
        "\u{0915}\u{094D}\u{0937}\u{093F}\u{0930}\u{094D}",
        "\u{1820}\u{180B}\u{1821}\u{180E}",
        "\u{0F40}\u{0F90}\u{0F71}",
        "a\u{0628}1\u{05D0}\u{0915}\u{0E01}\u{1780}\u{AC00}\u{3042}",
        "\u{FFFF}\u{10FFFF}\u{E000}\u{FEFF}",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect();
    out.push("\u{202B}".repeat(300) + "\u{05D0}a1");
    out.push("\u{2067}".repeat(300) + "a" + &"\u{2069}".repeat(300));
    out.push("(".repeat(500) + &")".repeat(500));
    out.push("\u{05D0}1".repeat(1_000));
    out.push("\u{0628}\u{0300}".repeat(1_000));
    out
}

#[test]
fn hostile_strings_never_panic() {
    let fonts: [&[u8]; 3] = [AMIRI, OPEN_SANS, AAT_SYNTHETIC];
    for text in hostile_strings() {
        let info = BidiInfo::new(&text, None);
        let order = info.reorder();
        assert_eq!(order.len(), text.chars().count());
        let paragraph = BidiParagraph::new(&text, None);
        for byte in 0..=text.len() + 1 {
            let _ = paragraph.level_at(byte);
            let _ = paragraph.run_at(byte);
        }
        let visual: usize = paragraph.visual_runs().iter().map(|r| r.range.len()).sum();
        assert_eq!(visual, text.len());

        let mut buffer = Buffer::new();
        buffer.set_text(&text);
        let _ = buffer.script_runs();
        for font_bytes in fonts {
            let blob = Blob::new(font_bytes);
            let face = Face::parse(&blob, 0).unwrap();
            let font = Font::new(face, 16.0);
            // Normalization always runs now. Vary the cluster level and
            // the buffer flags instead, which drive the other merges.
            for (level, flags) in [
                (ClusterLevel::MonotoneGraphemes, BufferFlags::DEFAULT),
                (
                    ClusterLevel::Characters,
                    BufferFlags::BOT | BufferFlags::EOT | BufferFlags::REMOVE_DEFAULT_IGNORABLES,
                ),
            ] {
                buffer.set_cluster_level(level);
                buffer.set_flags(flags);
                let run = shape(&font, &buffer, &[]).unwrap();
                assert!(run.len() <= 4 * text.chars().count() + 4);
                let run = paragraph.shape(&font, &buffer, &[]).unwrap();
                assert!(run.len() <= 4 * text.chars().count() + 4);
            }
        }
    }
}

#[test]
fn reorder_visual_of_any_levels_is_a_permutation() {
    // `BidiParagraph::reorder_visual` takes levels from the caller,
    // who may pass levels no paragraph produces: past the UAX #9
    // maximum depth, all odd, or none at all.
    let cases: [&[u8]; 6] = [
        &[],
        &[255],
        &[255, 0, 255, 0],
        &[0, 254, 1, 255, 126, 125],
        &[u8::MAX; 64],
        &[1, 3, 5, 7, 9, 11, 13, 15],
    ];
    for levels in cases {
        let mut order = BidiParagraph::reorder_visual(levels);
        order.sort_unstable();
        assert!(order.iter().copied().eq(0..levels.len()), "{levels:?}");
    }
}

#[test]
fn bidi_paragraph_lookups_past_the_end_are_none() {
    // Offsets at or past the end of the text, and offsets inside a
    // character, never index out of bounds.
    for text in ["", "abc", "\u{05D0}\u{05D1}", "a\u{0628}1"] {
        let paragraph = BidiParagraph::new(text, None);
        for byte in text.len()..text.len() + 4 {
            assert_eq!(paragraph.level_at(byte), None);
            assert!(paragraph.run_at(byte).is_none());
        }
        for byte in 0..text.len() {
            assert!(paragraph.level_at(byte).is_some());
            assert!(paragraph.run_at(byte).is_some());
        }
    }
}

#[test]
fn many_bidi_runs_shape_in_reasonable_time() {
    // Hebrew letters and digits alternate levels, so every character
    // is a run of its own and the paragraph shapes 20000 runs.
    let text = "\u{05D0}1".repeat(10_000);
    let paragraph = BidiParagraph::new(&text, None);
    assert_eq!(paragraph.runs().len(), 20_000);
    let blob = Blob::new(OPEN_SANS);
    let face = Face::parse(&blob, 0).unwrap();
    let font = Font::new(face, 16.0);
    let run = paragraph.shape(&font, &Buffer::new(), &[]).unwrap();
    assert_eq!(run.len(), 20_000);
}
