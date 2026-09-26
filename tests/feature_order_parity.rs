//! Which features the shapers run, in which order, and how often,
//! against rustybuzz 0.20.
//!
//! The vendored fonts' own lookups rarely show a feature running twice
//! or out of order, so each test swaps a real font's GSUB for a small
//! synthetic one whose lookups do: a single substitution that turns
//! glyph `g` into `g + 1` and `g + 1` into `g + 2` gives a different
//! glyph when it runs twice, and a ligature only forms when its
//! components sit next to each other in that order.

use sigilbuzz::{shape, Blob, Buffer, Direction, Face, Font};

const NOTO_THAI: &[u8] = include_bytes!("fonts/NotoSansThai-Regular.ttf");
const NOTO_LAO: &[u8] = include_bytes!("fonts/NotoSansLao-Regular.ttf");
const OLD_HANGUL: &[u8] = include_bytes!("fonts/NotoSansOldHangul-Subset.ttf");

// --- Synthetic GSUB ------------------------------------------------------------

/// One lookup of a synthetic GSUB.
enum Lookup {
    /// Single substitution (format 2), `(from, to)` sorted by `from`.
    Single(Vec<(u16, u16)>),
    /// Chained context (format 3): `input` followed by `lookahead` runs
    /// lookup `nested` on `input`.
    Chain {
        input: u16,
        lookahead: u16,
        nested: u16,
    },
}

fn push16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_be_bytes());
}

fn offset16(v: usize) -> u16 {
    u16::try_from(v).expect("offset fits in 16 bits")
}

fn coverage(glyphs: &[u16]) -> Vec<u8> {
    let mut out = Vec::new();
    push16(&mut out, 1);
    push16(&mut out, glyphs.len() as u16);
    for &g in glyphs {
        push16(&mut out, g);
    }
    out
}

fn subtable(lookup: &Lookup) -> (u16, Vec<u8>) {
    let mut out = Vec::new();
    match lookup {
        Lookup::Single(pairs) => {
            push16(&mut out, 2);
            push16(&mut out, offset16(6 + 2 * pairs.len()));
            push16(&mut out, pairs.len() as u16);
            for &(_, to) in pairs {
                push16(&mut out, to);
            }
            let from: Vec<u16> = pairs.iter().map(|p| p.0).collect();
            out.extend(coverage(&from));
            (1, out)
        }
        Lookup::Chain {
            input,
            lookahead,
            nested,
        } => {
            // Format, no backtrack, one input and one lookahead
            // coverage, one lookup record, then the two coverages.
            let input_at = 18;
            push16(&mut out, 3);
            push16(&mut out, 0);
            push16(&mut out, 1);
            push16(&mut out, offset16(input_at));
            push16(&mut out, 1);
            push16(&mut out, offset16(input_at + 6));
            push16(&mut out, 1);
            push16(&mut out, 0);
            push16(&mut out, *nested);
            out.extend(coverage(&[*input]));
            out.extend(coverage(&[*lookahead]));
            (6, out)
        }
    }
}

/// A GSUB whose `scripts` (sorted) all have a default language system
/// enabling every one of `features` (sorted by tag), each feature
/// selecting its lookup indices.
fn gsub(scripts: &[[u8; 4]], features: &[([u8; 4], Vec<u16>)], lookups: &[Lookup]) -> Vec<u8> {
    // ScriptList: records, one shared Script, one LangSys.
    let mut script_list = Vec::new();
    push16(&mut script_list, scripts.len() as u16);
    let script_at = 2 + 6 * scripts.len();
    for tag in scripts {
        script_list.extend_from_slice(tag);
        push16(&mut script_list, offset16(script_at));
    }
    push16(&mut script_list, 4); // default LangSys right after
    push16(&mut script_list, 0);
    push16(&mut script_list, 0); // lookupOrder
    push16(&mut script_list, 0xFFFF); // no required feature
    push16(&mut script_list, features.len() as u16);
    for i in 0..features.len() {
        push16(&mut script_list, i as u16);
    }

    let mut feature_list = Vec::new();
    push16(&mut feature_list, features.len() as u16);
    let mut body = Vec::new();
    let body_at = 2 + 6 * features.len();
    for (tag, indices) in features {
        feature_list.extend_from_slice(tag);
        push16(&mut feature_list, offset16(body_at + body.len()));
        push16(&mut body, 0);
        push16(&mut body, indices.len() as u16);
        for &i in indices {
            push16(&mut body, i);
        }
    }
    feature_list.extend(body);

    let mut lookup_list = Vec::new();
    push16(&mut lookup_list, lookups.len() as u16);
    let mut body = Vec::new();
    let body_at = 2 + 2 * lookups.len();
    for lookup in lookups {
        push16(&mut lookup_list, offset16(body_at + body.len()));
        let (kind, sub) = subtable(lookup);
        push16(&mut body, kind);
        push16(&mut body, 0); // lookupFlag
        push16(&mut body, 1);
        push16(&mut body, 8);
        body.extend(sub);
    }
    lookup_list.extend(body);

    let mut out = Vec::new();
    push16(&mut out, 1);
    push16(&mut out, 0);
    push16(&mut out, 10);
    push16(&mut out, offset16(10 + script_list.len()));
    push16(
        &mut out,
        offset16(10 + script_list.len() + feature_list.len()),
    );
    out.extend(script_list);
    out.extend(feature_list);
    out.extend(lookup_list);
    out
}

/// `font` with its `tag` table replaced by `table`.
fn with_table(font: &[u8], tag: [u8; 4], table: &[u8]) -> Vec<u8> {
    let be16 = |at: usize| usize::from(u16::from_be_bytes([font[at], font[at + 1]]));
    let be32 = |at: usize| {
        u32::from_be_bytes([font[at], font[at + 1], font[at + 2], font[at + 3]]) as usize
    };
    let mut tables: Vec<([u8; 4], &[u8])> = (0..be16(4))
        .map(|i| 12 + 16 * i)
        .map(|at| {
            let t = [font[at], font[at + 1], font[at + 2], font[at + 3]];
            (t, &font[be32(at + 8)..be32(at + 8) + be32(at + 12)])
        })
        .filter(|(t, _)| *t != tag)
        .collect();
    tables.push((tag, table));
    tables.sort_by_key(|(t, _)| *t);
    let count = tables.len() as u16;
    let selector = 15 - count.leading_zeros() as u16;
    let range = (1u16 << selector) * 16;
    let mut out = font[..4].to_vec();
    for v in [count, range, selector, count * 16 - range] {
        push16(&mut out, v);
    }
    let mut data = Vec::new();
    let data_at = 12 + 16 * tables.len();
    for (t, bytes) in &tables {
        out.extend_from_slice(t);
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&((data_at + data.len()) as u32).to_be_bytes());
        out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
        data.extend_from_slice(bytes);
        while data.len() % 4 != 0 {
            data.push(0);
        }
    }
    out.extend(data);
    out
}

fn glyph(font: &[u8], ch: char) -> u16 {
    let blob = Blob::new(font);
    let face = Face::parse(&blob, 0).expect("parse face");
    face.cmap().expect("cmap").glyph_id(ch).expect("mapped")
}

/// A lookup that moves `g` to `g + 1` and `g + 1` to `g + 2`: running
/// it twice gives `g + 2`.
fn bump(g: u16) -> Lookup {
    Lookup::Single(vec![(g, g + 1), (g + 1, g + 2)])
}

// --- Shaping ---------------------------------------------------------------------

/// `(glyph id, x advance, x offset, y offset)` per glyph.
type Row = (u32, i32, i32, i32);

fn sigilbuzz_rows(font: &[u8], text: &str, direction: Direction) -> Vec<Row> {
    let blob = Blob::new(font);
    let face = Face::parse(&blob, 0).expect("parse face");
    let font = Font::new(face, 1000.0);
    let mut buffer = Buffer::new();
    buffer.push_str(text);
    buffer.set_direction(direction);
    shape(&font, &buffer, &[])
        .expect("shape")
        .glyphs
        .iter()
        .map(|g| (g.glyph_id, g.x_advance, g.x_offset, g.y_offset))
        .collect()
}

fn rustybuzz_rows(font: &[u8], text: &str, direction: Direction) -> Vec<Row> {
    let face = rustybuzz::Face::from_slice(font, 0).expect("parse face");
    let mut buffer = rustybuzz::UnicodeBuffer::new();
    buffer.push_str(text);
    buffer.guess_segment_properties();
    buffer.set_direction(match direction {
        Direction::Rtl => rustybuzz::Direction::RightToLeft,
        _ => rustybuzz::Direction::LeftToRight,
    });
    let out = rustybuzz::shape(&face, &[], buffer);
    out.glyph_infos()
        .iter()
        .zip(out.glyph_positions())
        .map(|(i, p)| (i.glyph_id, p.x_advance, p.x_offset, p.y_offset))
        .collect()
}

fn assert_parity(font: &[u8], text: &str, direction: Direction) -> Vec<Row> {
    let sig = sigilbuzz_rows(font, text, direction);
    assert_eq!(sig, rustybuzz_rows(font, text, direction), "{text:?}");
    sig
}

// --- Tests -------------------------------------------------------------------------

#[test]
fn thai_lao_and_hangul_run_each_default_feature_once() {
    // HarfBuzz's Thai and Hangul shapers add nothing but the Hangul
    // jamo features to the default ones, so each runs once; the Hangul
    // shaper turns `calt` off entirely.
    let cases: [(&[u8], [u8; 4], char, &str); 3] = [
        (NOTO_THAI, *b"thai", '\u{0E01}', "\u{0E01}"),
        (NOTO_LAO, *b"lao ", '\u{0E81}', "\u{0E81}"),
        // An Extended-A leading jamo, which never composes, and a vowel.
        (OLD_HANGUL, *b"hang", '\u{A960}', "\u{A960}\u{1161}"),
    ];
    for (font, script, ch, text) in cases {
        let g = glyph(font, ch);
        for feature in [*b"ccmp", *b"liga", *b"calt"] {
            let table = gsub(&[*b"DFLT", script], &[(feature, vec![0])], &[bump(g)]);
            let patched = with_table(font, *b"GSUB", &table);
            let rows = assert_parity(&patched, text, Direction::Ltr);
            let runs = u32::from(!(script == *b"hang" && feature == *b"calt"));
            assert_eq!(rows[0].0, u32::from(g) + runs, "{feature:?} on {text:?}");
        }
    }
}

#[test]
fn direction_features_follow_the_requested_direction() {
    // HarfBuzz enables ltra and ltrm for a left-to-right plan and rtla
    // for a right-to-left one, whatever direction the text then shapes
    // in: LTR Hebrew is read as visual order and shaped right to left,
    // but still gets ltra.
    const OPEN_SANS: &[u8] = include_bytes!("fixtures/opensans_regular.ttf");
    const NOTO_HEBREW: &[u8] = include_bytes!("fonts/NotoSansHebrew-Regular.ttf");
    let cases: [(&[u8], [u8; 4], char, Direction); 3] = [
        (OPEN_SANS, *b"latn", 'a', Direction::Ltr),
        (NOTO_HEBREW, *b"hebr", '\u{05D0}', Direction::Rtl),
        (NOTO_HEBREW, *b"hebr", '\u{05D0}', Direction::Ltr),
    ];
    for (font, script, ch, direction) in cases {
        let g = glyph(font, ch);
        for feature in [*b"ltra", *b"ltrm", *b"rtla"] {
            let table = gsub(&[*b"DFLT", script], &[(feature, vec![0])], &[bump(g)]);
            let patched = with_table(font, *b"GSUB", &table);
            let rows = assert_parity(&patched, &ch.to_string(), direction);
            let enabled = match direction {
                Direction::Ltr => feature != *b"rtla",
                _ => feature == *b"rtla",
            };
            assert_eq!(rows[0].0, u32::from(g) + u32::from(enabled), "{feature:?}");
        }
    }
}

#[test]
fn non_joining_characters_take_no_isol() {
    // HarfBuzz's joining state machine gives a non-joining character
    // (hamza, a space) no action, so `isol` only reaches the letters
    // that join.
    const AMIRI: &[u8] = include_bytes!("fixtures/amiri_regular.ttf");
    let hamza = glyph(AMIRI, '\u{0621}');
    let beh = glyph(AMIRI, '\u{0628}');
    let mut pairs = vec![(hamza, hamza + 1), (beh, beh + 1)];
    pairs.sort_unstable();
    let table = gsub(
        &[*b"DFLT", *b"arab"],
        &[(*b"isol", vec![0])],
        &[Lookup::Single(pairs)],
    );
    let patched = with_table(AMIRI, *b"GSUB", &table);
    let rows = assert_parity(&patched, "\u{0621} \u{0628}", Direction::Rtl);
    // Visual order: beh, space, hamza.
    assert_eq!(rows[0].0, u32::from(beh) + 1);
    assert_eq!(rows[2].0, u32::from(hamza));
}

#[test]
fn myanmar_runs_locl_and_ccmp_before_reordering() {
    // HarfBuzz's Myanmar shaper applies locl and ccmp to the logical
    // order, then moves the medial ra in front of its base. A rule for
    // "ka followed by medial ra" only matches before that.
    const NOTO_MYANMAR: &[u8] = include_bytes!("fonts/NotoSansMyanmar-Regular.ttf");
    let ka = glyph(NOTO_MYANMAR, '\u{1000}');
    let medial_ra = glyph(NOTO_MYANMAR, '\u{103C}');
    for feature in [*b"ccmp", *b"locl"] {
        let lookups = [
            Lookup::Chain {
                input: ka,
                lookahead: medial_ra,
                nested: 1,
            },
            Lookup::Single(vec![(ka, ka + 1)]),
        ];
        let table = gsub(&[*b"DFLT", *b"mym2"], &[(feature, vec![0])], &lookups);
        let patched = with_table(NOTO_MYANMAR, *b"GSUB", &table);
        let rows = assert_parity(&patched, "\u{1000}\u{103C}", Direction::Ltr);
        assert!(rows.iter().any(|r| r.0 == u32::from(ka) + 1), "{rows:?}");
    }
}
