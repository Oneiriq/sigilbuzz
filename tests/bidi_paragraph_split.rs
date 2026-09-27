//! UAX #9 rule P1 in [`sigilbuzz::BidiParagraph`]: text splits into
//! paragraphs after each paragraph separator, and each paragraph
//! resolves, orders, and shapes on its own.
//!
//! The Unicode conformance files test one paragraph at a time
//! (BidiCharacterTest.txt has no paragraph separators, and
//! BidiTest.txt only has them last), so these tests build paragraphs
//! from them. Each case below is a line of
//! BidiCharacterTest-17.0.0.txt. Rule P1 says a text made of two cases
//! joined by a separator resolves to the two cases' levels side by
//! side, with the separator at the first paragraph's level, and orders
//! each paragraph as its own line.
//!
//! Shaping is checked against HarfBuzz 14.5.0 (uharfbuzz) shaping each
//! paragraph as a buffer of its own, which is how HarfBuzz callers
//! shape paragraphs.

use sigilbuzz::{
    bidi_class, BidiClass, BidiParagraph, BidiParagraphSpan, Buffer, BufferFlags, Direction, Face,
    Font, Glyph,
};

/// Lines of BidiCharacterTest-17.0.0.txt: code points, direction
/// (0 LTR, 1 RTL, 2 auto), paragraph level, levels (x for characters
/// rule X9 removes), and visual order.
const CASES: &[&str] = &[
    "202A 05D0 0028 05D1 202C 202D 0029;2;1;x 3 3 3 x x 2;3 2 1 6",
    "202A 05D0 0028 05D1 202C 202D 0029 202C;2;1;x 3 3 3 x x 2 x;3 2 1 6",
    "202B 0061 0028 0062 202C 202E 0029;2;0;x 2 2 2 x x 1;6 1 2 3",
    "202B 0061 0028 0062 202C 202E 0029 202C;2;0;x 2 2 2 x x 1 x;6 1 2 3",
    "202A 202E 0061 202C 0028 05D0 202C 202D 0029 202C;2;0;x x 3 x 3 3 x x 2 x;5 4 2 8",
    "202B 202D 05D0 202C 0028 0061 202C 202E 0029 202C;2;1;x x 4 x 4 4 x x 3 x;8 2 4 5",
    "202D 0028 202C 202A 05D0 0029 05D1;2;1;x 2 x x 3 3 3;1 6 5 4",
    "202D 0028 202C 202A 05D0 0029 05D1 202C;2;1;x 2 x x 3 3 3 x;1 6 5 4",
    "0028 05D0 0029 0028 0029 0061;0;0;0 1 0 0 0 0;0 1 2 3 4 5",
    "0061 0028 2680 0062 0029 0028 0063 0029;0;0;0 0 0 0 0 0 0 0;0 1 2 3 4 5 6 7",
    "2680 0028 0028 2681 0029 0029;0;0;0 0 0 0 0 0;0 1 2 3 4 5",
    "0028 0061 0028 2680 0029 0029 2681 0062;0;0;0 0 0 0 0 0 0 0;0 1 2 3 4 5 6 7",
    "05D0 0028 0028 05D1 0029 0061 0029 2680;0;0;1 0 0 1 0 0 0 0;0 1 2 3 4 5 6 7",
    "0028 2680 0029 05D0 005B 0061 05D1 005D;0;0;0 0 0 1 0 0 1 0;0 1 2 3 4 5 6 7",
    "0028 05D0 0029 0061 05D1 005B 005D 05D2;0;0;0 1 0 0 1 1 1 1;0 1 2 3 7 6 5 4",
    "0028 005B 2680 005D 05D0 2681 0029 05D1;0;0;0 0 0 0 1 0 0 1;0 1 2 3 4 5 6 7",
    "2680 0028 2681 05D0 0029 0028 0061 0029;1;1;1 1 1 1 1 1 2 1;7 6 5 4 3 2 1 0",
    "0028 0029 05D0 2680 0028 2681 0029 05D1;1;1;1 1 1 1 1 1 1 1;7 6 5 4 3 2 1 0",
    "0028 2680 0028 2681 0029 2682 05D0 0029;1;1;1 1 1 1 1 1 1 1;7 6 5 4 3 2 1 0",
    "0028 0061 05D0 0028 05D1 0029 0062 0029;1;1;1 2 1 1 1 1 2 1;7 6 5 4 3 2 1 0",
    "05D0 0028 0061 0029 05D1 005B 005D;1;1;1 1 2 1 1 1 1;6 5 4 3 2 1 0",
    "0028 0029 0061 05D0 0062 005B 2680 005D;1;1;1 1 2 1 2 1 1 1;7 6 5 4 3 2 1 0",
    "05D0 0028 005B 2680 005D 0029 0061;1;1;1 1 1 1 1 1 2;6 5 4 3 2 1 0",
    "0028 0061 005B 05D0 005D 2680 0029 0062;1;1;1 2 1 1 1 1 1 2;7 6 5 4 3 2 1 0",
];

/// The separators the cases are joined with: PARAGRAPH SEPARATOR, LF,
/// CR LF (one separator), and NEL.
const SEPARATORS: &[&str] = &["\u{2029}", "\n", "\r\n", "\u{85}"];

struct Case {
    chars: Vec<char>,
    /// The direction field: `None` for auto.
    direction: Option<Direction>,
    paragraph_level: u8,
    /// `None` where rule X9 removes the character.
    levels: Vec<Option<u8>>,
    /// Visual order of the characters rule X9 keeps.
    order: Vec<usize>,
}

fn parse(line: &str) -> Case {
    let fields: Vec<&str> = line.split(';').collect();
    let chars = fields[0]
        .split_whitespace()
        .map(|t| char::from_u32(u32::from_str_radix(t, 16).expect("hex")).expect("char"))
        .collect();
    let direction = match fields[1] {
        "0" => Some(Direction::Ltr),
        "1" => Some(Direction::Rtl),
        _ => None,
    };
    let levels = fields[3]
        .split_whitespace()
        .map(|t| t.parse().ok())
        .collect();
    let order = fields[4]
        .split_whitespace()
        .map(|t| t.parse().expect("index"))
        .collect();
    Case {
        chars,
        direction,
        paragraph_level: fields[2].parse().expect("level"),
        levels,
        order,
    }
}

/// True for the classes rule X9 removes.
fn removed(ch: char) -> bool {
    matches!(
        bidi_class(ch),
        BidiClass::Rle
            | BidiClass::Lre
            | BidiClass::Rlo
            | BidiClass::Lro
            | BidiClass::Pdf
            | BidiClass::Bn
    )
}

/// The levels and the visual order of the characters `paragraph`
/// resolves, the removed ones left out of the order.
fn resolve(paragraph: &BidiParagraph) -> (Vec<u8>, Vec<usize>) {
    let text = paragraph.text();
    let starts: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
    let levels = starts
        .iter()
        .map(|&i| paragraph.level_at(i).expect("level"))
        .collect();
    let mut order = Vec::new();
    for run in paragraph.visual_runs() {
        let mut indices: Vec<usize> = (0..starts.len())
            .filter(|&n| run.range.contains(&starts[n]))
            .collect();
        if run.is_rtl() {
            indices.reverse();
        }
        order.extend(indices);
    }
    let chars: Vec<char> = text.chars().collect();
    order.retain(|&n| !removed(chars[n]));
    (levels, order)
}

/// Checks `first` and `second` joined by `separator` under `direction`.
fn check_pair(first: &Case, second: &Case, separator: &str, direction: Option<Direction>) {
    let text: String = first
        .chars
        .iter()
        .copied()
        .chain(separator.chars())
        .chain(second.chars.iter().copied())
        .collect();
    let paragraph = BidiParagraph::new(&text, direction);
    let split = first.chars.iter().map(|c| c.len_utf8()).sum::<usize>() + separator.len();
    assert_eq!(
        paragraph.paragraphs(),
        [
            BidiParagraphSpan {
                range: 0..split,
                level: first.paragraph_level
            },
            BidiParagraphSpan {
                range: split..text.len(),
                level: second.paragraph_level
            },
        ],
        "{text:?}"
    );

    let sep_len = separator.chars().count();
    let expected_levels: Vec<Option<u8>> = first
        .levels
        .iter()
        .copied()
        .chain(core::iter::repeat(Some(first.paragraph_level)).take(sep_len))
        .chain(second.levels.iter().copied())
        .collect();
    // The separator is last in its paragraph at the paragraph level, so
    // it stays at the right end of a left-to-right line and goes to the
    // left end of a right-to-left one.
    let n = first.chars.len();
    let separator_order: Vec<usize> = if first.paragraph_level == 0 {
        (n..n + sep_len).collect()
    } else {
        (n..n + sep_len).rev().collect()
    };
    let mut expected_order: Vec<usize> = Vec::new();
    if first.paragraph_level == 0 {
        expected_order.extend(&first.order);
        expected_order.extend(&separator_order);
    } else {
        expected_order.extend(&separator_order);
        expected_order.extend(&first.order);
    }
    expected_order.extend(second.order.iter().map(|&i| i + n + sep_len));

    let (levels, order) = resolve(&paragraph);
    for (i, (&got, want)) in levels.iter().zip(&expected_levels).enumerate() {
        if let Some(want) = want {
            assert_eq!(got, *want, "{text:?} character {i}");
        }
    }
    assert_eq!(levels.len(), expected_levels.len(), "{text:?}");
    assert_eq!(order, expected_order, "{text:?}");
}

#[test]
fn joined_conformance_cases_resolve_as_separate_paragraphs() {
    let cases: Vec<Case> = CASES.iter().map(|line| parse(line)).collect();
    let mut pairs = 0;
    for first in &cases {
        for second in &cases {
            // Auto cases pair with each other. A forced direction
            // applies to both paragraphs, so forced cases pair only
            // with cases forced the same way.
            if first.direction != second.direction {
                continue;
            }
            for separator in SEPARATORS {
                check_pair(first, second, separator, first.direction);
                pairs += 1;
            }
        }
    }
    assert_eq!(pairs, 3 * 8 * 8 * SEPARATORS.len());
}

#[test]
fn each_conformance_case_alone_is_one_paragraph() {
    for line in CASES {
        let case = parse(line);
        let text: String = case.chars.iter().collect();
        let paragraph = BidiParagraph::new(&text, case.direction);
        assert_eq!(paragraph.paragraphs().len(), 1);
        assert_eq!(paragraph.base_level(), case.paragraph_level);
        let (levels, order) = resolve(&paragraph);
        for (got, want) in levels.iter().zip(&case.levels) {
            if let Some(want) = want {
                assert_eq!(got, want, "{line}");
            }
        }
        assert_eq!(order, case.order, "{line}");
    }
}

// ---------------------------------------------------------------------
// Shaping.
// ---------------------------------------------------------------------

const AMIRI: &[u8] = include_bytes!("fixtures/amiri_regular.ttf");

/// `(glyph_id, cluster, x_advance, y_advance, x_offset, y_offset)`.
type Pinned = (u32, u32, i32, i32, i32, i32);

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

fn amiri() -> Font<'static> {
    Font::new(Face::parse_bytes(AMIRI, 0).expect("parse Amiri"), 1000.0)
}

fn buffer_with(flags: BufferFlags) -> Buffer {
    let mut buffer = Buffer::new();
    buffer.set_flags(flags);
    buffer
}

#[test]
fn a_paragraph_starts_its_own_text_when_shaped() {
    // "ab", PARAGRAPH SEPARATOR, then a shadda and a beh: the second
    // paragraph opens with a mark. HarfBuzz shaping that paragraph as
    // its own buffer with BOT gives the mark a dotted circle (glyph 385)
    // to sit on (hb_insert_dotted_circle in hb-ot-shape.cc).
    let text = "ab\u{2029}\u{0651}\u{0628}";
    let paragraph = BidiParagraph::new(text, None);
    let font = amiri();
    let first: [Pinned; 3] = [
        (6256, 0, 420, 0, 0, 0),
        (6257, 1, 486, 0, 0, 0),
        (376, 2, 0, 0, 0, 0),
    ];

    let bot = buffer_with(BufferFlags::BOT | BufferFlags::EOT);
    let shaped = paragraph.shape(&font, &bot, &[]).expect("shape");
    let second: [Pinned; 3] = [
        (56, 7, 926, 0, 0, 0),
        (97, 5, 0, 0, 132, -210),
        (385, 5, 679, 0, 0, 0),
    ];
    let want: Vec<Pinned> = first.iter().chain(&second).copied().collect();
    assert_eq!(pin(&shaped.glyphs), want);

    // Without BOT, HarfBuzz inserts nothing.
    let plain = buffer_with(BufferFlags::empty());
    let shaped = paragraph.shape(&font, &plain, &[]).expect("shape");
    let second: [Pinned; 2] = [(56, 7, 926, 0, 0, 0), (97, 5, 0, 0, 0, 0)];
    let want: Vec<Pinned> = first.iter().chain(&second).copied().collect();
    assert_eq!(pin(&shaped.glyphs), want);
}

#[test]
fn paragraphs_shape_as_they_do_alone() {
    // Mixed paragraphs, each shaped in a BidiParagraph of its own for
    // comparison, clusters moved to their place in the whole text.
    let paragraphs = [
        "abc \u{0628}\u{0627}\u{2029}",
        "\u{0645}\u{0631}\u{062D}\u{0628}\u{0627} 12 abc\u{2029}",
        "\u{0651}\u{0628}\u{0650} def\u{2029}",
        "(\u{0661}\u{0662}) \u{0628}",
    ];
    let text: String = paragraphs.concat();
    let font = amiri();
    for flags in [
        BufferFlags::BOT | BufferFlags::EOT,
        BufferFlags::empty(),
        BufferFlags::BOT | BufferFlags::DO_NOT_INSERT_DOTTED_CIRCLE,
    ] {
        let buffer = buffer_with(flags);
        let whole = BidiParagraph::new(&text, None);
        assert_eq!(whole.paragraphs().len(), paragraphs.len());
        let got = pin(&whole.shape(&font, &buffer, &[]).expect("shape").glyphs);

        let mut want: Vec<Pinned> = Vec::new();
        let mut offset = 0u32;
        for part in paragraphs {
            let alone = BidiParagraph::new(part, None);
            let glyphs = alone.shape(&font, &buffer, &[]).expect("shape").glyphs;
            want.extend(pin(&glyphs).into_iter().map(|mut g| {
                g.1 += offset;
                g
            }));
            offset += part.len() as u32;
        }
        assert_eq!(got, want, "{flags:?}");
    }
}

#[test]
fn a_line_that_spans_paragraphs_shapes_each_part() {
    let text = "\u{0628}\u{0627} ab\u{2029}cd \u{0628}";
    let paragraph = BidiParagraph::new(text, None);
    let font = amiri();
    let line = paragraph
        .shape_line(&font, &Buffer::new(), &[], 0..text.len())
        .expect("shape");
    let ranges: Vec<(core::ops::Range<usize>, u8)> = line
        .iter()
        .map(|piece| (piece.run.range.clone(), piece.run.level))
        .collect();
    // The Arabic paragraph right to left, with its separator at its
    // left end, then the Latin paragraph left to right.
    assert_eq!(
        ranges,
        [(7..10, 1), (5..7, 2), (0..5, 1), (10..13, 0), (13..15, 1)]
    );
}
