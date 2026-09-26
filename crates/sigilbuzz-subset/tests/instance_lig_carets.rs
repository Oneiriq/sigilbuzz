//! Instancing folds ligature caret variations.
//!
//! Rubik VF's LigCaretList holds format 3 carets whose VariationIndex
//! tables move the caret with weight. Instancing drops the GDEF
//! ItemVariationStore, so the instancer has to resolve each caret's
//! delta at the instance first; otherwise every caret falls back to its
//! default-instance position.

use sigilbuzz::tables::tag;
use sigilbuzz::Face;
use sigilbuzz_subset::{instance, InstanceInput};

const RUBIK: &[u8] = include_bytes!("../../../tests/fixtures/rubik_vf.ttf");

fn u16_at(buf: &[u8], pos: usize) -> u16 {
    u16::from_be_bytes([buf[pos], buf[pos + 1]])
}

/// `(format, coordinate, device (outer, inner) if any)`.
type Caret = (u16, i16, Option<(u16, u16)>);

/// Every caret of every ligature in the GDEF LigCaretList, in list
/// order. Format 3 devices resolve against their CaretValue.
fn carets(gdef: &[u8]) -> Vec<Vec<Caret>> {
    let list = usize::from(u16_at(gdef, 8));
    (0..usize::from(u16_at(gdef, list + 2)))
        .map(|i| {
            let lig = list + usize::from(u16_at(gdef, list + 4 + i * 2));
            (0..usize::from(u16_at(gdef, lig)))
                .map(|k| {
                    let caret = lig + usize::from(u16_at(gdef, lig + 2 + k * 2));
                    let format = u16_at(gdef, caret);
                    let dev = (format == 3).then(|| usize::from(u16_at(gdef, caret + 4)));
                    let device = dev.filter(|&d| d != 0).map(|d| {
                        let table = caret + d;
                        assert_eq!(u16_at(gdef, table + 4), 0x8000, "VariationIndex");
                        (u16_at(gdef, table), u16_at(gdef, table + 2))
                    });
                    (format, u16_at(gdef, caret + 2) as i16, device)
                })
                .collect()
        })
        .collect()
}

#[test]
fn instanced_carets_absorb_their_variation_deltas() {
    let face = Face::parse_bytes(RUBIK, 0).unwrap();
    let source = carets(face.table_bytes(tag::GDEF).unwrap());
    let gdef = face.gdef().unwrap().unwrap();
    let store = gdef.item_variation_store().expect("Rubik has a store");
    let mut moved = false;
    for wght in [650.0f32, 900.0] {
        let coords = face.fvar().unwrap().unwrap().normalize_coords(&[wght]);
        let input = InstanceInput {
            coords: coords.clone(),
            drop_var_tables: true,
            axis_pins: Vec::new(),
        };
        let out = instance(&face, &input).expect("instance succeeds");
        let baked_face = Face::parse_bytes(&out.bytes, 0).unwrap();
        let baked = carets(baked_face.table_bytes(tag::GDEF).unwrap());
        // The store is evaluated in the post-avar space.
        let coords = match face.avar().unwrap() {
            Some(avar) => avar.remap_all(&coords),
            None => coords,
        };
        let expected: Vec<Vec<Caret>> = source
            .iter()
            .map(|lig| {
                lig.iter()
                    .map(|&(format, coord, device)| {
                        let delta =
                            device.map(|(outer, inner)| store.delta(outer, inner, &coords).round());
                        let shift = delta.unwrap_or(0.0) as i16;
                        moved |= shift != 0;
                        (format, coord + shift, None)
                    })
                    .collect()
            })
            .collect();
        assert_eq!(baked, expected, "wght={wght}");
    }
    assert!(moved, "expected some caret to move with weight");
}
