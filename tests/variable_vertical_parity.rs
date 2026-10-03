//! Vertical runs of variable fonts against HarfBuzz 14.5.0: advances and
//! the vertical origins every glyph is moved from, at several weights.
//!
//! - `noto_sans_kr_vf_vertical_subset.otf` (CFF2) has `VORG` and a `VVAR`
//!   whose vertical origin mapping moves the origins of the per mille sign
//!   and U+2170 with the weight.
//!
//! `tests/fixtures/variable_vertical.expected` holds HarfBuzz's output;
//! `tests/tools/variable_vertical_expected.py` regenerates it.

use sigilbuzz::{shape, Blob, Buffer, Direction, Face, Font};

const EXPECTED: &str = include_str!("fixtures/variable_vertical.expected");
const NOTO_KR: &[u8] = include_bytes!("fixtures/noto_sans_kr_vf_vertical_subset.otf");

/// `(glyph_id, x_advance, y_advance, x_offset, y_offset)`.
type Pos = (u32, i32, i32, i32, i32);

/// One record of the expected file.
struct Record<'a> {
    group: &'a str,
    wght: f32,
    text: String,
    glyphs: Vec<Pos>,
}

fn records() -> Vec<Record<'static>> {
    EXPECTED
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
        .map(|line| {
            let mut fields = line.split(' ');
            let group = fields.next().unwrap();
            let wght = fields.next().unwrap().parse().unwrap();
            let text = fields
                .next()
                .unwrap()
                .split(',')
                .map(|h| char::from_u32(u32::from_str_radix(h, 16).unwrap()).unwrap())
                .collect();
            let glyphs = fields
                .map(|g| {
                    let v: Vec<i64> = g.split(',').map(|n| n.parse().unwrap()).collect();
                    (
                        v[0] as u32,
                        v[1] as i32,
                        v[2] as i32,
                        v[3] as i32,
                        v[4] as i32,
                    )
                })
                .collect();
            Record {
                group,
                wght,
                text,
                glyphs,
            }
        })
        .collect()
}

/// sigilbuzz's top-to-bottom run of `text` at user-space `wght`.
fn shape_ttb(face: &Face<'_>, wght: f32, text: &str) -> Vec<Pos> {
    let fvar = face.fvar().unwrap().unwrap();
    let normalized = fvar.normalize_coords(&[wght]);
    let coords = match face.avar().unwrap() {
        Some(avar) => avar.remap_all(&normalized),
        None => normalized,
    };
    let font = Font::new(face.clone(), 1000.0).with_coords(&coords);
    let mut buffer = Buffer::new();
    buffer.push_str(text);
    buffer.set_direction(Direction::Ttb);
    shape(&font, &buffer, &[])
        .unwrap()
        .glyphs
        .iter()
        .map(|g| (g.glyph_id, g.x_advance, g.y_advance, g.x_offset, g.y_offset))
        .collect()
}

/// Checks every record of `group` against `font` and returns how many
/// there were.
fn check_group(group: &str, font: &[u8]) -> usize {
    let blob = Blob::new(font);
    let face = Face::parse(&blob, 0).unwrap();
    let mut checked = 0;
    for r in records().into_iter().filter(|r| r.group == group) {
        assert_eq!(
            shape_ttb(&face, r.wght, &r.text),
            r.glyphs,
            "{group} wght {} {:?}",
            r.wght,
            r.text
        );
        checked += 1;
    }
    checked
}

#[test]
fn vorg_origins_move_by_the_vvar_vertical_origin_deltas() {
    assert_eq!(check_group("vorg", NOTO_KR), 54);
}

#[test]
fn the_vvar_deltas_do_move_the_origins() {
    // The per mille sign sits at its VORG origin, 863, at the default
    // weight and 10 units higher at the heaviest.
    let blob = Blob::new(NOTO_KR);
    let face = Face::parse(&blob, 0).unwrap();
    assert_eq!(shape_ttb(&face, 100.0, "\u{2030}")[0].4, -863);
    assert_eq!(shape_ttb(&face, 900.0, "\u{2030}")[0].4, -873);
}
