//! Vertical runs of variable fonts against HarfBuzz 14.5.0: advances and
//! the vertical origins every glyph is moved from, at several weights.
//!
//! - `noto_sans_kr_vf_vertical_subset.otf` (CFF2) has `VORG` and a `VVAR`
//!   whose vertical origin mapping moves the origins of the per mille sign
//!   and U+2170 with the weight.
//! - `hahmlet_gvar_subset.ttf` with a `vhea`, a `vmtx` and a `gvar` of its
//!   own (see [`phantom_font`]) takes its origins from the top phantom
//!   points, which `gvar` moves for `O` and which the composites `Á` and
//!   `Å` take from their `USE_MY_METRICS` component.
//! - `hahmlet_gvar_subset.ttf` and `rubik_vf.ttf` as they ship, without
//!   `vmtx`, center each glyph's varied box in the ascender-to-descender
//!   span.
//!
//! `tests/fixtures/vertical_shaping.expected` holds HarfBuzz's output;
//! `tests/tools/vertical_shaping_expected.py` regenerates it, building the
//! phantom font the same way.

use sigilbuzz::{shape, Blob, Buffer, Direction, Face, Font};

const EXPECTED: &str = include_str!("fixtures/vertical_shaping.expected");
const NOTO_KR: &[u8] = include_bytes!("fixtures/noto_sans_kr_vf_vertical_subset.otf");
const HAHMLET: &[u8] = include_bytes!("fixtures/hahmlet_gvar_subset.ttf");
const RUBIK: &[u8] = include_bytes!("fixtures/rubik_vf.ttf");

/// `(glyph_id, x_advance, y_advance, x_offset, y_offset)`.
type Pos = (u32, i32, i32, i32, i32);

/// One record of the expected file.
struct Record<'a> {
    group: &'a str,
    font: &'a str,
    wght: f32,
    text: String,
    glyphs: Vec<Pos>,
}

fn records() -> Vec<Record<'static>> {
    EXPECTED
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
        .filter_map(|line| {
            let mut fields = line.split(' ');
            let group = fields.next().unwrap();
            let font = fields.next().unwrap();
            // The static fonts are `tests/vertical_shaping.rs`'s.
            let wght = fields.next().unwrap().parse().ok()?;
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
            Some(Record {
                group,
                font,
                wght,
                text,
                glyphs,
            })
        })
        .collect()
}

fn be16(font: &[u8], at: usize) -> usize {
    usize::from(u16::from_be_bytes([font[at], font[at + 1]]))
}

fn be32(font: &[u8], at: usize) -> usize {
    u32::from_be_bytes([font[at], font[at + 1], font[at + 2], font[at + 3]]) as usize
}

/// The offset and length of table `tag` in `font`.
fn table(font: &[u8], tag: &[u8; 4]) -> Option<(usize, usize)> {
    (0..be16(font, 4)).find_map(|i| {
        let rec = 12 + 16 * i;
        (&font[rec..rec + 4] == tag).then(|| (be32(font, rec + 8), be32(font, rec + 12)))
    })
}

/// `font` with `extra` tables added to its table directory, each
/// replacing a table of the same tag.
fn with_tables(font: &[u8], extra: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
    let mut records: Vec<([u8; 4], Vec<u8>)> = (0..be16(font, 4))
        .map(|i| {
            let rec = 12 + 16 * i;
            let tag = [font[rec], font[rec + 1], font[rec + 2], font[rec + 3]];
            let (offset, len) = table(font, &tag).unwrap();
            (tag, font[offset..offset + len].to_vec())
        })
        .filter(|(tag, _)| extra.iter().all(|(t, _)| *t != tag))
        .collect();
    records.extend(extra.iter().map(|(tag, body)| (**tag, body.clone())));
    records.sort_by_key(|r| r.0);
    let mut out = font[..4].to_vec();
    out.extend_from_slice(&(records.len() as u16).to_be_bytes());
    out.extend_from_slice(&[0; 6]);
    let mut offset = 12 + 16 * records.len();
    for (tag, body) in &records {
        out.extend_from_slice(tag);
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&(offset as u32).to_be_bytes());
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        offset += (body.len() + 3) & !3;
    }
    for (_, body) in &records {
        out.extend_from_slice(body);
        out.resize((out.len() + 3) & !3, 0);
    }
    out
}

/// Hahmlet's subset with a `vhea` and `vmtx` (advance 1000, top side
/// bearing 50 for all 12 glyphs) and a `gvar` of its own, which at the
/// top of the weight axis moves the top and bottom phantom points of `O`
/// (glyph 4) by 40 and -25 units.
fn phantom_font() -> Vec<u8> {
    let blob = Blob::new(HAHMLET);
    let face = Face::parse(&blob, 0).unwrap();
    let o_points = face
        .glyf()
        .unwrap()
        .point_count(&face.loca().unwrap(), 4)
        .unwrap()
        .unwrap();
    let top = o_points - 2;
    let mut vhea = vec![0u8; 36];
    vhea[..4].copy_from_slice(&0x0001_1000u32.to_be_bytes());
    vhea[34..36].copy_from_slice(&12u16.to_be_bytes());
    let vmtx: Vec<u8> = (0..12).flat_map(|_| [0x03, 0xE8, 0, 50]).collect();
    // One axis, no shared tuples, 12 glyphs with long offsets; only
    // glyph 4 has data: one tuple with an embedded peak and private
    // point numbers.
    let mut gvar = Vec::new();
    for v in [1u16, 0, 1, 0] {
        gvar.extend_from_slice(&v.to_be_bytes());
    }
    let data_array = 20 + 4 * 13;
    gvar.extend_from_slice(&(data_array as u32).to_be_bytes());
    gvar.extend_from_slice(&12u16.to_be_bytes());
    gvar.extend_from_slice(&1u16.to_be_bytes());
    gvar.extend_from_slice(&(data_array as u32).to_be_bytes());
    // One tuple, its data at 10: a header of data size (patched below),
    // flags, and the peak.
    let mut data = Vec::new();
    for v in [1u16, 10, 0, 0xA000, 0x4000] {
        data.extend_from_slice(&v.to_be_bytes());
    }
    let serialized = data.len();
    data.extend_from_slice(&[2, 0x81]); // two points, as words
    data.extend_from_slice(&top.to_be_bytes());
    data.extend_from_slice(&1u16.to_be_bytes());
    data.push(0x81); // two zero x deltas
    data.push(0x41); // two y deltas, as words
    data.extend_from_slice(&40i16.to_be_bytes());
    data.extend_from_slice(&(-25i16).to_be_bytes());
    let size = (data.len() - serialized) as u16;
    data[4..6].copy_from_slice(&size.to_be_bytes());
    for gid in 0..=12u32 {
        let offset = if gid > 4 { data.len() as u32 } else { 0 };
        gvar.extend_from_slice(&offset.to_be_bytes());
    }
    gvar.extend(data);
    with_tables(
        HAHMLET,
        &[(b"vhea", vhea), (b"vmtx", vmtx), (b"gvar", gvar)],
    )
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

/// Checks every record of `group` for the font file `name` against
/// `font` and returns how many there were.
fn check_group(group: &str, name: &str, font: &[u8]) -> usize {
    let blob = Blob::new(font);
    let face = Face::parse(&blob, 0).unwrap();
    let mut checked = 0;
    for r in records()
        .into_iter()
        .filter(|r| r.group == group && r.font == name)
    {
        assert_eq!(
            shape_ttb(&face, r.wght, &r.text),
            r.glyphs,
            "{group} {name} wght {} {:?}",
            r.wght,
            r.text
        );
        checked += 1;
    }
    checked
}

#[test]
fn vorg_origins_move_by_the_vvar_vertical_origin_deltas() {
    let name = "noto_sans_kr_vf_vertical_subset.otf";
    assert_eq!(check_group("vorg", name, NOTO_KR), 54);
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

#[test]
fn glyf_origins_follow_the_varied_top_phantom_point() {
    let font = phantom_font();
    assert_eq!(check_group("phantom", "hahmlet_gvar_subset.ttf", &font), 50);
    let blob = Blob::new(&font);
    let face = Face::parse(&blob, 0).unwrap();
    // O's box tops out at 750: the origin is 750 + 50 at the default
    // weight, and the gvar delta lifts it 40 more at 900.
    assert_eq!(shape_ttb(&face, 400.0, "O")[0].4, -800);
    assert_eq!(shape_ttb(&face, 900.0, "O")[0].4, -840);
    // The composite Á takes its top phantom point from A, its
    // USE_MY_METRICS component, at the default weight too.
    assert_eq!(shape_ttb(&face, 400.0, "\u{C1}")[0].4, -802);
}

#[test]
fn origins_without_vmtx_center_the_varied_box() {
    assert_eq!(
        check_group("extents", "hahmlet_gvar_subset.ttf", HAHMLET),
        50
    );
    assert_eq!(check_group("extents", "rubik_vf.ttf", RUBIK), 16);
}
