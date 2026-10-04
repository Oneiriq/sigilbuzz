//! AAT `morx` contextual and ligature subtables against HarfBuzz.
//!
//! `tests/fixtures/aat_morx_contextual.ttf` carries a contextual subtable
//! whose substitution table is the spec's unsized offset array, with
//! mark, current, end-of-text and unset-mark entries.
//! `tests/fixtures/aat_morx_ligature.ttf` carries a ligature subtable with
//! cascading ligatures (the ligature stays on the stack), a component set
//! twice through DontAdvance, Store without Last, an action at the end of
//! text, and a stack underflow. Neither font has GSUB, so both HarfBuzz
//! and sigilbuzz shape them through `morx`.
//!
//! `tests/fixtures/aat_morx_parity.expected` holds HarfBuzz 14.5.0's glyphs
//! and clusters for each string. `tests/tools/build_varc_morx_parity_fixtures.py`
//! builds the fonts and `tests/tools/varc_morx_parity_expected.py` writes
//! the expected file.

use sigilbuzz::{shape, Blob, Buffer, Face, Font};

const CONTEXTUAL: &[u8] = include_bytes!("fixtures/aat_morx_contextual.ttf");
const LIGATURE: &[u8] = include_bytes!("fixtures/aat_morx_ligature.ttf");
const EXPECTED: &str = include_str!("fixtures/aat_morx_parity.expected");

#[test]
fn morx_shaping_matches_harfbuzz() {
    let mut checked = 0;
    for line in EXPECTED.lines().filter(|l| l.starts_with("shape ")) {
        let mut fields = line.split(' ').skip(1);
        let font_name = fields.next().unwrap();
        let text = fields.next().unwrap();
        let want: Vec<(u32, u32)> = fields
            .map(|f| {
                let (g, c) = f.split_once(':').unwrap();
                (g.parse().unwrap(), c.parse().unwrap())
            })
            .collect();
        let data = match font_name {
            "aat_morx_contextual.ttf" => CONTEXTUAL,
            "aat_morx_ligature.ttf" => LIGATURE,
            other => panic!("unknown fixture {other}"),
        };
        let blob = Blob::new(data);
        let face = Face::parse(&blob, 0).unwrap();
        let font = Font::new(face, 1000.0);
        let mut buf = Buffer::new();
        buf.push_str(text);
        let run = shape(&font, &buf, &[]).unwrap();
        let got: Vec<(u32, u32)> = run.glyphs.iter().map(|g| (g.glyph_id, g.cluster)).collect();
        assert_eq!(got, want, "{font_name}: {text:?}");
        checked += 1;
    }
    assert_eq!(checked, 25);
}
