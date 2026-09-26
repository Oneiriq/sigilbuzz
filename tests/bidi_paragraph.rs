//! End-to-end contract of [`sigilbuzz::BidiParagraph`]: HarfBuzz's
//! run-level bidi model.
//!
//! Every level run is shaped in logical order with its own direction and
//! the paragraph text around it as context, then the runs are put in
//! visual order. The tests pin the three promises that model makes:
//!
//! - every glyph's cluster is a byte offset into the text the caller
//!   wrote, at the character that made the glyph;
//! - each run shapes exactly like rustybuzz shaping that logical run with
//!   the same direction and context, so Arabic joining and mark
//!   attachment see logical order and letters across run edges;
//! - runs are ordered per line, with the line's trailing whitespace at
//!   the paragraph level.

use rustybuzz::Direction as RbDirection;
use sigilbuzz::{
    shape, BidiParagraph, BidiRun, Buffer, Direction, Face, Font, Glyph, ShapedBidiRun,
};

const AMIRI: &[u8] = include_bytes!("fixtures/amiri_regular.ttf");
const NOTO_HEBREW: &[u8] = include_bytes!("fonts/NotoSansHebrew-Regular.ttf");

/// `(glyph_id, cluster, x_advance, y_advance, x_offset, y_offset)`.
type Pinned = (u32, u32, i32, i32, i32, i32);

/// Mixed-direction paragraphs, each with the font that covers most of
/// it. Glyphs a font lacks still carry their clusters.
const CORPUS: &[(&[u8], &str)] = &[
    // LTR paragraph: Latin, Hebrew, digits after Hebrew (level 2),
    // Arabic, final punctuation.
    (
        AMIRI,
        "Hello \u{05E2}\u{05D1}\u{05E8}\u{05D9}\u{05EA} 123 \u{0645}\u{0631}\u{062D}\u{0628}\u{0627}!",
    ),
    // RTL paragraph: Arabic, Latin, digits, Arabic.
    (
        AMIRI,
        "\u{0645}\u{0631}\u{062D}\u{0628}\u{0627} abc 42 \u{0628}\u{0633}\u{0645}",
    ),
    // Latin directly between two Arabic letters.
    (AMIRI, "\u{0628}abc\u{0628}"),
    // Vocalized Arabic inside an LTR paragraph.
    (
        AMIRI,
        "abc \u{0628}\u{0650}\u{0633}\u{0652}\u{0645}\u{0650} def",
    ),
    // Arabic-Indic digits (AN) in brackets inside Arabic.
    (
        AMIRI,
        "\u{0627}\u{0644}\u{0639}\u{0631}\u{0628}\u{064A}\u{0629} (\u{0661}\u{0662}\u{0663}) abc.",
    ),
    // Persian with a ZWNJ inside the word, in an LTR paragraph.
    (
        AMIRI,
        "abc \u{0645}\u{06CC}\u{200C}\u{062E}\u{0648}\u{0627}\u{0647}\u{0645}",
    ),
    // Hebrew with digits and Latin.
    (NOTO_HEBREW, "abc \u{05E9}\u{05DC}\u{05D5}\u{05DD} 123"),
    // Hebrew with points and a Latin tail.
    (
        NOTO_HEBREW,
        "\u{05E9}\u{05C1}\u{05B8}\u{05DC}\u{05D5}\u{05B9}\u{05DD} abc",
    ),
    // Pointed Hebrew alone.
    (
        NOTO_HEBREW,
        "\u{05D1}\u{05BC}\u{05B0}\u{05E8}\u{05B5}\u{05D0}\u{05E9}\u{05C1}\u{05B4}\u{05D9}\u{05EA}",
    ),
];

fn font(data: &'static [u8]) -> Font<'static> {
    Font::new(Face::parse_bytes(data, 0).expect("parse face"), 1000.0)
}

fn pin(glyphs: &[Glyph]) -> Vec<Pinned> {
    glyphs
        .iter()
        .map(|g| {
            (
                g.glyph_id,
                g.cluster,
                g.x_advance,
                g.y_advance,
                g.x_offset,
                g.y_offset,
            )
        })
        .collect()
}

/// rustybuzz shaping of `run` alone: its text, its direction, and the
/// paragraph text around it as context. Clusters are moved to
/// paragraph offsets.
fn rustybuzz_run(data: &[u8], text: &str, run: &BidiRun) -> Vec<Pinned> {
    let face = rustybuzz::Face::from_slice(data, 0).expect("parse rustybuzz face");
    let mut buffer = rustybuzz::UnicodeBuffer::new();
    buffer.set_pre_context(&text[..run.range.start]);
    buffer.push_str(&text[run.range.clone()]);
    // `push_str` clears the post-context, so set it afterwards.
    buffer.set_post_context(&text[run.range.end..]);
    buffer.set_direction(if run.is_rtl() {
        RbDirection::RightToLeft
    } else {
        RbDirection::LeftToRight
    });
    buffer.guess_segment_properties();
    let out = rustybuzz::shape(&face, &[], buffer);
    let offset = run.range.start as u32;
    out.glyph_infos()
        .iter()
        .zip(out.glyph_positions())
        .map(|(info, pos)| {
            (
                info.glyph_id,
                info.cluster + offset,
                pos.x_advance,
                pos.y_advance,
                pos.x_offset,
                pos.y_offset,
            )
        })
        .collect()
}

/// The Hebrew and Arabic combining marks the corpus uses.
fn is_mark(ch: char) -> bool {
    matches!(
        ch as u32,
        0x0591..=0x05BD
            | 0x05BF
            | 0x05C1..=0x05C2
            | 0x05C4..=0x05C5
            | 0x05C7
            | 0x0610..=0x061A
            | 0x064B..=0x065F
            | 0x0670
    )
}

/// Moves every cluster that points at a combining mark back to the
/// mark's base. rustybuzz, at HarfBuzz's default cluster level, gives a
/// mark glyph its base's cluster; sigilbuzz gives it the mark's own
/// offset. Both point into the same grapheme, which is what these tests
/// compare.
fn to_bases(text: &str, pinned: Vec<Pinned>) -> Vec<Pinned> {
    pinned
        .into_iter()
        .map(|(gid, cluster, xa, ya, xo, yo)| {
            let mut at = cluster as usize;
            while text[at..].chars().next().is_some_and(is_mark) {
                at = text[..at].char_indices().next_back().map_or(0, |(i, _)| i);
            }
            (gid, at as u32, xa, ya, xo, yo)
        })
        .collect()
}

fn clusters(glyphs: &[Glyph]) -> Vec<u32> {
    glyphs.iter().map(|g| g.cluster).collect()
}

/// Byte offset of the `n`th character of `text`.
fn byte_of(text: &str, n: usize) -> u32 {
    text.char_indices()
        .nth(n)
        .map(|(i, _)| i as u32)
        .expect("char index in range")
}

#[test]
fn every_run_matches_rustybuzz_with_the_same_direction_and_context() {
    for &(data, text) in CORPUS {
        let font = font(data);
        let paragraph = BidiParagraph::new(text, None);
        for run in paragraph.runs() {
            let ours = paragraph
                .shape_run(&font, &Buffer::new(), &[], run)
                .expect("shape run");
            assert_eq!(
                to_bases(text, pin(&ours.glyphs)),
                to_bases(text, rustybuzz_run(data, text, run)),
                "{text:?} run {run:?}"
            );
        }
    }
}

#[test]
fn paragraph_output_is_its_runs_in_visual_order() {
    for &(data, text) in CORPUS {
        let font = font(data);
        let paragraph = BidiParagraph::new(text, None);
        let whole = paragraph.shape(&font, &Buffer::new(), &[]).expect("shape");
        let mut expected = Vec::new();
        for run in paragraph.visual_runs() {
            expected.extend(rustybuzz_run(data, text, &run));
        }
        assert_eq!(
            to_bases(text, pin(&whole.glyphs)),
            to_bases(text, expected),
            "{text:?}"
        );
    }
}

#[test]
fn every_cluster_is_a_character_of_the_original_text() {
    for &(data, text) in CORPUS {
        let font = font(data);
        let paragraph = BidiParagraph::new(text, None);
        let run = paragraph.shape(&font, &Buffer::new(), &[]).expect("shape");
        assert!(!run.is_empty());
        for glyph in &run.glyphs {
            let cluster = glyph.cluster as usize;
            assert!(
                cluster < text.len() && text.is_char_boundary(cluster),
                "{text:?}: cluster {cluster} is not a character start"
            );
        }
    }
}

/// With fonts that map these characters one to one (no ligatures, no
/// contextual forms), each glyph must be the cmap glyph of the character
/// its cluster points at, mirrored inside right-to-left runs, and every
/// character must produce exactly one glyph. Noto Sans Hebrew covers the
/// Hebrew letters and Open Sans the Latin letters, digits and brackets;
/// the rest are `.notdef` in each.
#[test]
fn clusters_point_at_the_characters_that_made_the_glyphs() {
    const OPEN_SANS: &[u8] = include_bytes!("fixtures/opensans_regular.ttf");
    let texts = [
        "Hello (\u{05E9}\u{05DC}\u{05D5}\u{05DD}) 123 world [\u{05D0}\u{05D1}\u{05D2}].",
        "\u{05E9}\u{05DC}\u{05D5}\u{05DD} (abc) 12 [\u{05D3}\u{05D4}]!",
        "\u{05D0} 1-2 \u{05D1}, a.b \u{05D2}",
    ];
    for (data, text) in [NOTO_HEBREW, OPEN_SANS]
        .into_iter()
        .flat_map(|data| texts.map(|text| (data, text)))
    {
        let font = font(data);
        let cmap = font.face().cmap().expect("cmap");
        let paragraph = BidiParagraph::new(text, None);
        let run = paragraph.shape(&font, &Buffer::new(), &[]).expect("shape");
        let mut seen: Vec<u32> = clusters(&run.glyphs);
        seen.sort_unstable();
        let starts: Vec<u32> = text.char_indices().map(|(i, _)| i as u32).collect();
        assert_eq!(seen, starts, "{text:?}: one glyph per character");
        for glyph in &run.glyphs {
            let cluster = glyph.cluster as usize;
            let ch = text[cluster..].chars().next().expect("char start");
            let rtl = paragraph.level_at(cluster).is_some_and(|l| l % 2 == 1);
            let drawn = match (rtl, ch) {
                (true, '(') => ')',
                (true, ')') => '(',
                (true, '[') => ']',
                (true, ']') => '[',
                _ => ch,
            };
            let expected = u32::from(cmap.glyph_id(drawn).unwrap_or(0));
            assert_eq!(
                glyph.glyph_id, expected,
                "{text:?}: glyph for {ch:?} at {cluster}"
            );
        }
    }
}

#[test]
fn pure_paragraphs_shape_like_one_buffer() {
    // A one-direction paragraph is a single run: the same as shaping one
    // buffer in that direction.
    let cases = [
        (
            AMIRI,
            "\u{0645}\u{0631}\u{062D}\u{0628}\u{0627} \u{0628}\u{0633}\u{0645}",
            Direction::Rtl,
        ),
        (AMIRI, "Hello world", Direction::Ltr),
        (
            NOTO_HEBREW,
            "\u{05E9}\u{05C1}\u{05B8}\u{05DC}\u{05D5}\u{05B9}\u{05DD}",
            Direction::Rtl,
        ),
    ];
    for (data, text, direction) in cases {
        let font = font(data);
        let paragraph = BidiParagraph::new(text, None);
        assert_eq!(paragraph.direction(), direction);
        assert_eq!(paragraph.runs().len(), 1);
        let mut buffer = Buffer::new();
        buffer.set_text(text);
        buffer.set_direction(direction);
        let plain = shape(&font, &buffer, &[]).expect("shape buffer");
        let ours = paragraph
            .shape(&font, &Buffer::new(), &[])
            .expect("shape paragraph");
        assert_eq!(ours.glyphs, plain.glyphs, "{text:?}");
    }
}

/// Glyph ids of `text` shaped alone, right to left, with no context.
fn isolated_rtl(font: &Font<'_>, text: &str) -> Vec<u32> {
    let mut buffer = Buffer::new();
    buffer.set_text(text);
    buffer.set_direction(Direction::Rtl);
    let run = shape(font, &buffer, &[]).expect("shape");
    run.glyphs.iter().map(|g| g.glyph_id).collect()
}

#[test]
fn arabic_joins_across_a_run_edge_through_transparent_controls() {
    // beh, LRM, beh in an LTR paragraph: the LRM is strong left to
    // right, so it is a run of its own between two level-1 runs. It is
    // transparent to Arabic joining, so each beh still joins the other
    // through the run's context: the glyphs rustybuzz gives each run
    // with that context, not the isolated beh of a run shaped alone.
    let text = "\u{0628}\u{200E}\u{0628}";
    let font = font(AMIRI);
    let paragraph = BidiParagraph::new(text, Some(Direction::Ltr));
    let levels: Vec<u8> = paragraph.runs().iter().map(|r| r.level).collect();
    assert_eq!(levels, [1, 0, 1]);

    let mut behs = Vec::new();
    for run in paragraph.runs() {
        let ours = paragraph
            .shape_run(&font, &Buffer::new(), &[], run)
            .expect("shape");
        assert_eq!(
            pin(&ours.glyphs),
            rustybuzz_run(AMIRI, text, run),
            "run {run:?}"
        );
        behs.push(ours.glyphs[0].glyph_id);
    }
    let isolated = isolated_rtl(&font, "\u{0628}")[0];
    assert_ne!(behs[0], isolated, "the first beh joins forward");
    assert_ne!(behs[2], isolated, "the second beh joins backward");
    assert_ne!(behs[0], behs[2], "initial and final forms differ");
}

#[test]
fn arabic_keeps_its_joins_when_a_line_breaks_inside_a_word() {
    // Cut "marhaba" at every character boundary and shape the two
    // pieces as a line breaker would. Each piece matches rustybuzz
    // shaping it with the rest of the word as context. Where the
    // letters on both sides of the cut join (meem-reh, hah-beh,
    // beh-alef), the context changes the pieces: shaped alone, the edge
    // letters would lose the join. Reh does not join the letter after
    // it, so a cut between reh and hah changes nothing. (GSUB lookups
    // never see context, in HarfBuzz either, so a contextual alternate
    // that spans the cut is not expected to survive.)
    let text = "\u{0645}\u{0631}\u{062D}\u{0628}\u{0627}";
    let font = font(AMIRI);
    let paragraph = BidiParagraph::new(text, None);
    let joining_cuts = [2, 6, 8];
    for (cut, _) in text.char_indices().skip(1) {
        let mut with_context = Vec::new();
        let mut alone = Vec::new();
        for range in [0..cut, cut..text.len()] {
            let run = BidiRun {
                range: range.clone(),
                level: 1,
            };
            let shaped = paragraph
                .shape_run(&font, &Buffer::new(), &[], &run)
                .expect("shape");
            assert_eq!(
                pin(&shaped.glyphs),
                rustybuzz_run(AMIRI, text, &run),
                "cut at {cut}"
            );
            with_context.extend(shaped.glyphs.iter().map(|g| g.glyph_id));
            alone.extend(isolated_rtl(&font, &text[range]));
        }
        assert_eq!(
            with_context != alone,
            joining_cuts.contains(&cut),
            "cut at byte {cut}"
        );
    }
}

#[test]
fn latin_between_arabic_letters_breaks_the_join() {
    // "b" "abc" "b" in an RTL paragraph: three runs, and both behs are
    // isolated because the Latin letters beside them do not join.
    let text = "\u{0628}abc\u{0628}";
    let font = font(AMIRI);
    let paragraph = BidiParagraph::new(text, None);
    assert_eq!(
        paragraph.runs(),
        [
            BidiRun {
                range: 0..2,
                level: 1
            },
            BidiRun {
                range: 2..5,
                level: 2
            },
            BidiRun {
                range: 5..7,
                level: 1
            },
        ]
    );
    let isolated = rustybuzz_run(
        AMIRI,
        "\u{0628}",
        &BidiRun {
            range: 0..2,
            level: 1,
        },
    )[0]
    .0;
    let run = paragraph.shape(&font, &Buffer::new(), &[]).expect("shape");
    // Visual order: the second beh at the left, then "abc", then the
    // first beh at the right.
    assert_eq!(clusters(&run.glyphs), [5, 2, 3, 4, 0]);
    assert_eq!(run.glyphs[0].glyph_id, isolated);
    assert_eq!(run.glyphs[4].glyph_id, isolated);
}

#[test]
fn isolates_and_embeddings_shape_at_their_levels() {
    // LTR paragraph: "abc ", an RLI isolate holding Arabic and a
    // number, PDI, " def ", an RLE embedding holding Latin, PDF, and an
    // LRO override holding Hebrew, PDF.
    let text = "abc \u{2067}\u{0628}\u{0633}\u{0645} 12\u{2069} def \u{202B}xy\u{202C} \u{202D}\u{05D0}\u{05D1}\u{202C}";
    let paragraph = BidiParagraph::new(text, None);
    let level = |s: &str| {
        let at = text.find(s).expect("substring");
        paragraph.level_at(at).expect("level")
    };
    assert_eq!(level("abc"), 0);
    assert_eq!(level("\u{2067}"), 0);
    assert_eq!(level("\u{0628}"), 1);
    assert_eq!(level("12"), 2);
    assert_eq!(level("\u{2069}"), 0);
    assert_eq!(level("xy"), 2);
    // LRO forces the Hebrew letters to strong L: level 2 in the LRO.
    assert_eq!(level("\u{05D0}"), 2);

    // Every run still matches rustybuzz shaping that run in its
    // direction: the isolate's Arabic right to left, the digits and the
    // overridden Hebrew left to right.
    let font = font(AMIRI);
    for run in paragraph.runs() {
        let ours = paragraph
            .shape_run(&font, &Buffer::new(), &[], run)
            .expect("shape");
        assert_eq!(
            pin(&ours.glyphs),
            rustybuzz_run(AMIRI, text, run),
            "run {run:?}"
        );
    }
    // Visual order inside the isolate: the number, then the Arabic word
    // reversed.
    let shaped = paragraph.shape(&font, &Buffer::new(), &[]).expect("shape");
    let order = clusters(&shaped.glyphs);
    let pos = |byte: usize| {
        order
            .iter()
            .position(|&c| c as usize == byte)
            .expect("glyph")
    };
    let beh = text.find('\u{0628}').expect("beh");
    let meem = text.find('\u{0645}').expect("meem");
    let one = text.find('1').expect("digit");
    assert!(pos(one) < pos(meem) && pos(meem) < pos(beh));
    // The overridden Hebrew keeps logical order left to right.
    let alef = text.find('\u{05D0}').expect("alef");
    let bet = text.find('\u{05D1}').expect("bet");
    assert!(pos(alef) < pos(bet));
}

#[test]
fn fsi_takes_the_direction_of_its_first_strong_character() {
    let text = "abc \u{2068}\u{05D0}\u{05D1} x\u{2069} def";
    let paragraph = BidiParagraph::new(text, None);
    let alef = text.find('\u{05D0}').expect("alef");
    let x = text.find('x').expect("x");
    // FSI resolves to RLI: the Hebrew sits at level 1, the Latin inside
    // the isolate at level 2.
    assert_eq!(paragraph.level_at(alef), Some(1));
    assert_eq!(paragraph.level_at(x), Some(2));
}

#[test]
fn brackets_mirror_inside_rtl_runs() {
    // RTL paragraph with a bracketed Arabic word: N0 gives the brackets
    // the paragraph direction, they shape right to left and are drawn
    // mirrored, so the opening bracket at the right shows ')'.
    let text = "\u{0628} (\u{0633}\u{0645}) \u{062F}";
    let font = font(AMIRI);
    let cmap = font.face().cmap().expect("cmap");
    let paragraph = BidiParagraph::new(text, None);
    assert_eq!(
        paragraph.runs(),
        [BidiRun {
            range: 0..text.len(),
            level: 1
        }]
    );
    let run = paragraph.shape(&font, &Buffer::new(), &[]).expect("shape");
    let open = text.find('(').expect("open");
    let close = text.find(')').expect("close");
    let order = clusters(&run.glyphs);
    let at = |byte: usize| {
        order
            .iter()
            .position(|&c| c as usize == byte)
            .expect("glyph")
    };
    assert!(
        at(close) < at(open),
        "right to left: ')' is drawn left of '('"
    );
    let glyph = |byte: usize| run.glyphs[at(byte)].glyph_id;
    assert_eq!(glyph(open), u32::from(cmap.glyph_id(')').expect("paren")));
    assert_eq!(glyph(close), u32::from(cmap.glyph_id('(').expect("paren")));

    // In an LTR paragraph, brackets around Hebrew take the paragraph
    // direction and are not mirrored: the '(' is the glyph a plain
    // left-to-right "abc (" gives it. (Amiri draws Latin-context
    // brackets with their own glyphs, so this is not the cmap glyph.)
    let text = "abc (\u{05D1}\u{05D2}) def";
    let paragraph = BidiParagraph::new(text, None);
    let open = text.find('(').expect("open");
    assert_eq!(paragraph.level_at(open), Some(0));
    let run = paragraph.shape(&font, &Buffer::new(), &[]).expect("shape");
    let first = run
        .glyphs
        .iter()
        .find(|g| g.cluster as usize == open)
        .expect("glyph");
    let last_ltr = |s: &str| {
        let mut buffer = Buffer::new();
        buffer.set_text(s);
        buffer.set_direction(Direction::Ltr);
        shape(&font, &buffer, &[]).expect("shape").glyphs[4].glyph_id
    };
    assert_eq!(first.glyph_id, last_ltr("abc ("));
    assert_ne!(first.glyph_id, last_ltr("abc )"));
    for run in paragraph.runs() {
        let ours = paragraph
            .shape_run(&font, &Buffer::new(), &[], run)
            .expect("shape");
        assert_eq!(
            pin(&ours.glyphs),
            rustybuzz_run(AMIRI, text, run),
            "run {run:?}"
        );
    }
}

fn line_clusters(line: &[ShapedBidiRun]) -> Vec<u32> {
    line.iter()
        .flat_map(|piece| clusters(&piece.glyphs))
        .collect()
}

#[test]
fn lines_reorder_on_their_own() {
    // LTR paragraph "abc ALEF BET GIMEL DALET HE xyz" wrapped after
    // "GIMEL ": on one line the Hebrew words reverse together; on two
    // lines each line reverses its own part, and the space that ends
    // the first line moves to its right end.
    let text = "abc \u{05D0}\u{05D1}\u{05D2} \u{05D3}\u{05D4} xyz";
    let font = font(NOTO_HEBREW);
    let paragraph = BidiParagraph::new(text, None);
    let b = |n| byte_of(text, n);

    let one = paragraph
        .shape_line(&font, &Buffer::new(), &[], 0..text.len())
        .expect("shape line");
    assert_eq!(
        line_clusters(&one),
        [
            0,
            1,
            2,
            3,
            b(9),
            b(8),
            b(7),
            b(6),
            b(5),
            b(4),
            b(10),
            b(11),
            b(12),
            b(13)
        ]
    );

    let cut = b(8) as usize;
    let first = paragraph
        .shape_line(&font, &Buffer::new(), &[], 0..cut)
        .expect("shape line");
    assert_eq!(line_clusters(&first), [0, 1, 2, 3, b(6), b(5), b(4), b(7)]);
    let levels: Vec<u8> = first.iter().map(|piece| piece.run.level).collect();
    assert_eq!(levels, [0, 1, 0]);
    let second = paragraph
        .shape_line(&font, &Buffer::new(), &[], cut..text.len())
        .expect("shape line");
    assert_eq!(
        line_clusters(&second),
        [b(9), b(8), b(10), b(11), b(12), b(13)]
    );
}

#[test]
fn rtl_lines_put_trailing_spaces_at_the_left() {
    // RTL paragraph "ALEF BET abc def GIMEL" wrapped inside the Latin
    // run after "abc ": the first line's trailing space takes level 1
    // and is drawn at the line's left end, left of "abc".
    let text = "\u{05D0}\u{05D1} abc def \u{05D2}";
    let font = font(NOTO_HEBREW);
    let paragraph = BidiParagraph::new(text, None);
    let b = |n| byte_of(text, n);
    let cut = b(7) as usize; // after "abc "
    let first = paragraph
        .shape_line(&font, &Buffer::new(), &[], 0..cut)
        .expect("shape line");
    assert_eq!(
        line_clusters(&first),
        [b(6), b(3), b(4), b(5), b(2), b(1), b(0)]
    );
    let second = paragraph
        .shape_line(&font, &Buffer::new(), &[], cut..text.len())
        .expect("shape line");
    // "def" reads first, so it is drawn at the right; the space and
    // gimel follow to its left.
    assert_eq!(line_clusters(&second), [b(11), b(10), b(7), b(8), b(9)]);
}

#[test]
fn line_pieces_match_rustybuzz_with_paragraph_context() {
    // A line that starts inside an Arabic word: the piece before the
    // break is context, so the first letter on the line keeps its medial
    // or final form, as rustybuzz gives it with the same context.
    let text = "abc \u{0645}\u{0631}\u{062D}\u{0628}\u{0627} xyz";
    let font = font(AMIRI);
    let paragraph = BidiParagraph::new(text, None);
    let cut = text.find('\u{062D}').expect("hah");
    for line in [0..cut, cut..text.len()] {
        let pieces = paragraph
            .shape_line(&font, &Buffer::new(), &[], line.clone())
            .expect("shape line");
        for piece in &pieces {
            assert_eq!(
                pin(&piece.glyphs),
                rustybuzz_run(AMIRI, text, &piece.run),
                "line {line:?} piece {:?}",
                piece.run
            );
        }
    }
}

#[test]
fn buffer_settings_reach_every_run() {
    // The template buffer's language and script reach each run; its
    // text, direction and context do not.
    let text = "abc \u{0628}\u{0633}\u{0645}";
    let font = font(AMIRI);
    let paragraph = BidiParagraph::new(text, None);
    let mut template = Buffer::new();
    template.set_text("ignored");
    template.set_direction(Direction::Ttb);
    template.set_pre_context("\u{0628}");
    let with_template = paragraph.shape(&font, &template, &[]).expect("shape");
    let plain = paragraph.shape(&font, &Buffer::new(), &[]).expect("shape");
    assert_eq!(with_template.glyphs, plain.glyphs);
}
