//! Advances and extents of variable `glyf` fonts stay robust to tables
//! a run does not need.
//!
//! - A horizontal run never reads `vmtx`, so a malformed one cannot fail
//!   it, at the default instance or away from it.
//! - A direction reads `gvar` only when its own variations table (`HVAR`
//!   or `VVAR`) is missing, so the usual horizontal variable font, with
//!   `HVAR` and no `VVAR`, shapes horizontal runs without `gvar`.
//! - When a glyph's varied phantom points cannot be computed, the glyph
//!   keeps its `hmtx` advance, the font's own default, and the rest of
//!   the run is unaffected. Drawing the glyph still fails.
//! - A vertical run without `VVAR` takes its advances from the varied
//!   top and bottom phantom points, which `vmtx` places.

use sigilbuzz::{shape, Blob, Buffer, Face, Font};

const CJK: &[u8] = include_bytes!("fixtures/noto_sans_cjk_jp_uvs_subset.otf");
const HAHMLET: &[u8] = include_bytes!("fixtures/hahmlet_gvar_subset.ttf");
const RUBIK: &[u8] = include_bytes!("fixtures/rubik_vf.ttf");

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
    let num_tables = usize::from(u16::from_be_bytes([font[4], font[5]]));
    let mut records: Vec<([u8; 4], Vec<u8>)> = (0..num_tables)
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

/// A `vhea` whose `numOfLongVerMetrics` is zero, which no `vmtx`
/// parses against.
fn broken_vhea() -> Vec<u8> {
    let mut vhea = vec![0u8; 36];
    vhea[..4].copy_from_slice(&0x0001_1000u32.to_be_bytes());
    vhea
}

fn advances(face: &Face<'_>, coords: &[f32], text: &str) -> Result<Vec<i32>, sigilbuzz::Error> {
    let font = Font::new(face.clone(), 1000.0).with_coords(coords);
    let mut buffer = Buffer::new();
    buffer.push_str(text);
    let run = shape(&font, &buffer, &[])?;
    Ok(run.glyphs.iter().map(|g| g.x_advance).collect())
}

/// Hahmlet's normalized coords at `wght`, through `fvar` and `avar`.
fn hahmlet_coords(face: &Face<'_>, wght: f32) -> Vec<f32> {
    let fvar = face.fvar().unwrap().unwrap();
    face.avar()
        .unwrap()
        .unwrap()
        .remap_all(&fvar.normalize_coords(&[wght]))
}

#[test]
fn a_horizontal_run_ignores_a_broken_vmtx() {
    let mut bytes = CJK.to_vec();
    let (vhea, _) = table(&bytes, b"vhea").unwrap();
    bytes[vhea + 34..vhea + 36].copy_from_slice(&0u16.to_be_bytes());
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    assert!(face.vmtx().is_err());
    assert_eq!(advances(&face, &[], "A").unwrap(), [1000]);
}

#[test]
fn varied_extents_ignore_a_broken_vmtx() {
    let intact_blob = Blob::new(HAHMLET);
    let intact = Face::parse(&intact_blob, 0).unwrap();
    let bytes = with_tables(HAHMLET, &[(b"vhea", broken_vhea()), (b"vmtx", vec![0; 48])]);
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    assert!(face.vmtx().is_err());
    let coords = hahmlet_coords(&face, 900.0);
    for gid in 0..12 {
        assert_eq!(
            face.glyph_bounds_at_coords(gid, &coords).unwrap(),
            intact.glyph_bounds_at_coords(gid, &coords).unwrap(),
            "glyph {gid}"
        );
    }
    let text = "AO \u{C1}\u{C5}\u{BE60}";
    assert_eq!(
        advances(&face, &coords, text).unwrap(),
        advances(&intact, &coords, text).unwrap()
    );
}

/// `font` with its `HVAR` record renamed, so varied advances come from
/// the phantom points.
fn without_hvar(font: &[u8]) -> Vec<u8> {
    let mut bytes = font.to_vec();
    for i in 0..be16(&bytes, 4) {
        let rec = 12 + 16 * i;
        if &bytes[rec..rec + 4] == b"HVAR" {
            bytes[rec..rec + 4].copy_from_slice(b"HVAX");
        }
    }
    bytes
}

/// `font` with its `gvar` major version set to 2, which no parser
/// accepts.
fn with_gvar_v2(font: &[u8]) -> Vec<u8> {
    let mut bytes = font.to_vec();
    let (gvar, _) = table(&bytes, b"gvar").unwrap();
    bytes[gvar..gvar + 2].copy_from_slice(&2u16.to_be_bytes());
    bytes
}

/// The offset in `font` of glyph `gid`'s `GlyphVariationData`.
fn glyph_variation_data(font: &[u8], gid: usize) -> usize {
    let (gvar, _) = table(font, b"gvar").unwrap();
    let data_array = be32(font, gvar + 16);
    let entry = if be16(font, gvar + 14) & 1 != 0 {
        be32(font, gvar + 20 + 4 * gid)
    } else {
        2 * be16(font, gvar + 20 + 2 * gid)
    };
    gvar + data_array + entry
}

#[test]
fn a_horizontal_run_with_hvar_never_reads_gvar() {
    let intact_blob = Blob::new(RUBIK);
    let intact = Face::parse(&intact_blob, 0).unwrap();
    let bytes = with_gvar_v2(RUBIK);
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    assert!(face.gvar().is_err());
    let heaviest = [1.0];
    let got = advances(&face, &heaviest, "Hello").unwrap();
    assert_eq!(got, advances(&intact, &heaviest, "Hello").unwrap());
    // HVAR did move them.
    assert_ne!(got, advances(&intact, &[], "Hello").unwrap());
}

#[test]
fn a_gvar_that_does_not_parse_leaves_the_static_advances() {
    let bytes = with_gvar_v2(&without_hvar(HAHMLET));
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    assert!(face.hvar().unwrap().is_none());
    let coords = hahmlet_coords(&face, 900.0);
    // hmtx: A 834, O 891, space 248.
    assert_eq!(advances(&face, &coords, "AO ").unwrap(), [834, 891, 248]);
}

#[test]
fn a_malformed_glyph_keeps_its_static_advance() {
    let intact_bytes = without_hvar(HAHMLET);
    let intact_blob = Blob::new(&intact_bytes);
    let intact = Face::parse(&intact_blob, 0).unwrap();
    // Point glyph 4's (`O`) serialized data past its end.
    let mut bytes = intact_bytes.clone();
    let o = glyph_variation_data(&bytes, 4);
    bytes[o + 2..o + 4].copy_from_slice(&0xFFFFu16.to_be_bytes());
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    let coords = hahmlet_coords(&face, 900.0);
    assert!(face.glyph_outline_at_coords(4, &coords).is_err());

    let want = advances(&intact, &coords, "AO ").unwrap();
    // The phantom points widen A and the space at this weight.
    assert_ne!((want[0], want[2]), (834, 248));
    assert_eq!(
        advances(&face, &coords, "AO ").unwrap(),
        [want[0], 891, want[2]]
    );
}

#[test]
fn a_vertical_run_without_vvar_takes_advances_from_phantom_points() {
    // Hahmlet with vhea and vmtx (advance height 1000, top side bearing
    // 50 for every glyph) and a gvar of its own, which at peak wght 1.0
    // moves the top and bottom phantom points of `O` (glyph 4) by 40 and
    // -25. Without VVAR the vertical advance comes from those points.
    let blob = Blob::new(HAHMLET);
    let face = Face::parse(&blob, 0).unwrap();
    let o_points = face
        .glyf()
        .unwrap()
        .point_count(&face.loca().unwrap(), 4)
        .unwrap()
        .unwrap();
    let top = o_points - 2;
    let mut vhea = broken_vhea();
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
    let bytes = with_tables(
        HAHMLET,
        &[(b"vhea", vhea), (b"vmtx", vmtx), (b"gvar", gvar)],
    );
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    assert!(face.vvar().unwrap().is_none());

    let y_advance = |wght: f32| {
        let coords = hahmlet_coords(&face, wght);
        let font = Font::new(face.clone(), 1000.0).with_coords(&coords);
        let mut buffer = Buffer::new();
        buffer.push_str("O");
        buffer.set_direction(sigilbuzz::Direction::Ttb);
        let run = shape(&font, &buffer, &[]).unwrap();
        assert_eq!(run.glyphs[0].glyph_id, 4);
        run.glyphs[0].y_advance
    };
    assert_eq!(y_advance(400.0), -1000);
    assert_eq!(y_advance(900.0), -1065);
    // The horizontal advance still comes from HVAR.
    let coords = hahmlet_coords(&face, 900.0);
    let intact_blob = Blob::new(HAHMLET);
    let intact = Face::parse(&intact_blob, 0).unwrap();
    assert_eq!(
        advances(&face, &coords, "O").unwrap(),
        advances(&intact, &coords, "O").unwrap()
    );
}

/// An `ItemVariationStore` with one region, peaking at the top of the
/// first axis, and one item per glyph, each moving by `delta` there.
fn ivs_every_glyph(glyphs: u16, delta: i16) -> Vec<u8> {
    let mut ivs = Vec::new();
    // Format 1, the region list at 12, one data subtable at 22.
    ivs.extend_from_slice(&1u16.to_be_bytes());
    ivs.extend_from_slice(&12u32.to_be_bytes());
    ivs.extend_from_slice(&1u16.to_be_bytes());
    ivs.extend_from_slice(&22u32.to_be_bytes());
    // One axis, one region: start 0, peak 1, end 1.
    for v in [1u16, 1, 0, 0x4000, 0x4000] {
        ivs.extend_from_slice(&v.to_be_bytes());
    }
    // `glyphs` items of one word delta each, for region 0.
    for v in [glyphs, 1, 1, 0] {
        ivs.extend_from_slice(&v.to_be_bytes());
    }
    for _ in 0..glyphs {
        ivs.extend_from_slice(&delta.to_be_bytes());
    }
    ivs
}

#[test]
fn varied_advances_stop_at_zero() {
    // HarfBuzz adds the rounded HVAR (or VVAR) delta to the advance
    // and stops at zero: `hb_max (0, advance + roundf (delta))`.
    // Hahmlet with an HVAR and a VVAR that take 5000 units off every
    // advance at wght 900, and a vmtx of 1000-unit advances.
    let ivs = ivs_every_glyph(12, -5000);
    let mut hvar = vec![0, 1, 0, 0];
    hvar.extend_from_slice(&20u32.to_be_bytes());
    hvar.extend_from_slice(&[0; 12]);
    hvar.extend_from_slice(&ivs);
    let mut vvar = vec![0, 1, 0, 0];
    vvar.extend_from_slice(&24u32.to_be_bytes());
    vvar.extend_from_slice(&[0; 16]);
    vvar.extend_from_slice(&ivs);
    let mut vhea = broken_vhea();
    vhea[34..36].copy_from_slice(&12u16.to_be_bytes());
    let vmtx: Vec<u8> = (0..12).flat_map(|_| [0x03, 0xE8, 0, 50]).collect();
    let bytes = with_tables(
        HAHMLET,
        &[
            (b"HVAR", hvar),
            (b"VVAR", vvar),
            (b"vhea", vhea),
            (b"vmtx", vmtx),
        ],
    );
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    let y_advances = |coords: &[f32]| {
        let font = Font::new(face.clone(), 1000.0).with_coords(coords);
        let mut buffer = Buffer::new();
        buffer.push_str("AO ");
        buffer.set_direction(sigilbuzz::Direction::Ttb);
        let run = shape(&font, &buffer, &[]).unwrap();
        run.glyphs.iter().map(|g| g.y_advance).collect::<Vec<_>>()
    };
    // HarfBuzz 14.5.0 gives the same advances.
    assert_eq!(advances(&face, &[], "AO ").unwrap(), [834, 891, 248]);
    assert_eq!(y_advances(&[]), [-1000, -1000, -1000]);
    for wght in [650.0, 900.0] {
        let coords = hahmlet_coords(&face, wght);
        assert_eq!(advances(&face, &coords, "AO ").unwrap(), [0, 0, 0]);
        assert_eq!(y_advances(&coords), [0, 0, 0]);
    }
}
