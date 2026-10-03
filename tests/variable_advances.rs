//! Advances and extents of variable `glyf` fonts stay robust to tables
//! a run does not need.
//!
//! - A horizontal run never reads `vmtx`, so a malformed one cannot fail
//!   it, at the default instance or away from it.

use sigilbuzz::{shape, Blob, Buffer, Face, Font};

const CJK: &[u8] = include_bytes!("fixtures/noto_sans_cjk_jp_uvs_subset.otf");
const HAHMLET: &[u8] = include_bytes!("fixtures/hahmlet_gvar_subset.ttf");

/// The offset and length of table `tag` in `font`.
fn table(font: &[u8], tag: &[u8; 4]) -> Option<(usize, usize)> {
    let be32 = |at: usize| u32::from_be_bytes([font[at], font[at + 1], font[at + 2], font[at + 3]]);
    let num_tables = usize::from(u16::from_be_bytes([font[4], font[5]]));
    (0..num_tables).find_map(|i| {
        let rec = 12 + 16 * i;
        (&font[rec..rec + 4] == tag).then(|| (be32(rec + 8) as usize, be32(rec + 12) as usize))
    })
}

/// `font` with `extra` tables appended to its table directory.
fn with_tables(font: &[u8], extra: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
    let num_tables = usize::from(u16::from_be_bytes([font[4], font[5]]));
    let mut records: Vec<([u8; 4], Vec<u8>)> = (0..num_tables)
        .map(|i| {
            let rec = 12 + 16 * i;
            let tag = [font[rec], font[rec + 1], font[rec + 2], font[rec + 3]];
            let (offset, len) = table(font, &tag).unwrap();
            (tag, font[offset..offset + len].to_vec())
        })
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
