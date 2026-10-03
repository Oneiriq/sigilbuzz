//! Vertical metrics round-trip tests.
//!
//! A subset must lay vertical text out exactly like its source: the
//! same advance heights (`vhea` / `vmtx`), the same vertical origins
//! (`VORG`), and, for a variable font, the same variations (`VVAR`).
//! Subsets used to drop all four, so vertical text in a subset fell
//! back to an advance and origin made up from the horizontal metrics.
//!
//! The main fixture is a 16-glyph cut of Noto Sans KR Variable, a CFF2
//! font with every one of those tables (see `tests/fixtures/README.md`).
//! It keeps U+2030 PER MILLE SIGN and U+2170 SMALL ROMAN NUMERAL ONE,
//! whose vertical origins vary with the weight, so the instancer's
//! `VORG` bake has something to fold in. A `glyf` font gets the same
//! checks with vertical tables grafted onto Open Sans.
//!
//! The baselines CJK text lines up on live in `BASE`, which subsets
//! used to drop along with `STAT`; both are checked here too.

use sigilbuzz::tables::tag;
use sigilbuzz::tables::variation_store::ItemVariationStore;
use sigilbuzz::{shape, Buffer, Direction, Face, Font};
use sigilbuzz_subset::{instance, subset, InstanceInput, SubsetInput, SubsetOutput};

#[path = "support/sfnt.rs"]
mod support;

const NOTO_KR: &[u8] =
    include_bytes!("../../../tests/fixtures/noto_sans_kr_vf_vertical_subset.otf");
const OPEN_SANS: &[u8] = include_bytes!("../../../tests/fixtures/opensans_regular.ttf");

/// Every character the Noto Sans KR fixture keeps.
const KR_TEXT: &str = " \u{300C}\u{300D}\u{3001}\u{3002}\u{AC00}\u{2030}\u{2170}";

/// Glyph id, cluster, x and y advance, x and y offset.
type Shaped = Vec<(u32, u32, i32, i32, i32, i32)>;

fn shaped(bytes: &[u8], coords: &[f32], text: &str, direction: Direction) -> Shaped {
    let face = Face::parse_bytes(bytes, 0).unwrap();
    let font = Font::new(face, 1.0).with_coords(coords);
    let mut buffer = Buffer::new();
    buffer.set_text(text);
    buffer.set_direction(direction);
    let run = shape(&font, &buffer, &[]).expect("shapes");
    run.glyphs
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

/// Subsets `bytes` to the characters of `text`, after `adjust` edits
/// the input.
fn subset_text(bytes: &[u8], text: &str, adjust: impl FnOnce(&mut SubsetInput)) -> SubsetOutput {
    let face = Face::parse_bytes(bytes, 0).unwrap();
    let cmap = face.cmap().unwrap();
    let mut input = SubsetInput {
        gids: text.chars().map(|c| cmap.glyph_id(c).unwrap()).collect(),
        ..SubsetInput::default()
    };
    adjust(&mut input);
    subset(&face, &input).expect("subset succeeds")
}

fn new_gid(out: &SubsetOutput, old: u32) -> u32 {
    let i = out
        .gid_map
        .binary_search_by_key(&(old as u16), |&(o, _)| o)
        .unwrap_or_else(|_| panic!("glyph {old} was not kept"));
    u32::from(out.gid_map[i].1)
}

/// Asserts that the subset shapes `text` like its source, horizontally
/// and vertically, with every glyph renumbered through the gid map.
fn assert_same_shaping(src: &[u8], out: &SubsetOutput, text: &str, coords: &[f32]) {
    for direction in [Direction::Ltr, Direction::Ttb] {
        let want: Shaped = shaped(src, coords, text, direction)
            .into_iter()
            .map(|g| (new_gid(out, g.0), g.1, g.2, g.3, g.4, g.5))
            .collect();
        let got = shaped(&out.bytes, coords, text, direction);
        assert_eq!(got, want, "{direction:?} shaping of {text:?}");
    }
}

fn has(bytes: &[u8], table: [u8; 4]) -> bool {
    Face::parse_bytes(bytes, 0).unwrap().record(table).is_some()
}

/// The `wght` 700 coordinates of the fixture: normalized, then through
/// `avar` (the shaper's space).
fn wght_700() -> (Vec<f32>, Vec<f32>) {
    let face = Face::parse_bytes(NOTO_KR, 0).unwrap();
    let normalized = face.fvar().unwrap().unwrap().normalize_coords(&[700.0]);
    let shaper = face.avar().unwrap().unwrap().remap_all(&normalized);
    (normalized, shaper)
}

/// The Offset32 at `slot` of `table`.
fn offset_at(table: &[u8], slot: usize) -> usize {
    u32::from_be_bytes(table[slot..slot + 4].try_into().unwrap()) as usize
}

/// The `VVAR` vertical origin delta of `gid` at `coords`. The core
/// parser skips that map, so it is read here.
fn vorg_delta(bytes: &[u8], gid: u16, coords: &[f32]) -> f32 {
    let face = Face::parse_bytes(bytes, 0).unwrap();
    let vvar = face.table_bytes(tag::VVAR).unwrap();
    let store = ItemVariationStore::parse(&vvar[offset_at(vvar, 4)..]).unwrap();
    let map = &vvar[offset_at(vvar, 20)..];
    assert_eq!(map[0], 0, "the fixture's map is format 0");
    let entry_format = map[1];
    let count = usize::from(u16::from_be_bytes([map[2], map[3]]));
    let size = usize::from((entry_format >> 4) & 3) + 1;
    let inner_bits = u32::from(entry_format & 0xF) + 1;
    let at = 4 + usize::from(gid).min(count - 1) * size;
    let raw = map[at..at + size]
        .iter()
        .fold(0u32, |r, &b| (r << 8) | u32::from(b));
    let (outer, inner) = (raw >> inner_bits, raw & ((1 << inner_bits) - 1));
    store.delta(outer as u16, inner as u16, coords)
}

#[test]
fn the_fixture_carries_every_vertical_table() {
    for table in [tag::VHEA, tag::VMTX, tag::VORG, tag::VVAR] {
        assert!(has(NOTO_KR, table), "{:?}", core::str::from_utf8(&table));
    }
}

#[test]
fn a_subset_keeps_vertical_advances_and_origins() {
    let text = "\u{300C}\u{3002}\u{2030}";
    let out = subset_text(NOTO_KR, text, |_| {});
    assert!(out.warnings.is_empty(), "{:?}", out.warnings);
    for table in [tag::VHEA, tag::VMTX, tag::VORG, tag::VVAR] {
        assert!(has(&out.bytes, table));
    }
    assert_same_shaping(NOTO_KR, &out, text, &[]);
    let every = subset_text(NOTO_KR, KR_TEXT, |_| {});
    assert_same_shaping(NOTO_KR, &every, KR_TEXT, &[]);
    // The values themselves: the 1000-unit advance from vmtx, and the
    // origin from VORG (its default for the bracket, an entry of its
    // own for the per mille sign). Before the fix the subset gave
    // -1448 and -1160 from the horizontal metrics.
    let ttb = shaped(&out.bytes, &[], "\u{300C}\u{2030}", Direction::Ttb);
    let metrics: Vec<(i32, i32, i32)> = ttb.iter().map(|g| (g.3, g.4, g.5)).collect();
    assert_eq!(metrics, [(-1000, -500, -880), (-1000, -500, -863)]);
}

#[test]
fn a_subset_keeps_the_vertical_variations() {
    let text = "\u{300C}\u{3002}\u{2030}\u{2170}";
    let out = subset_text(NOTO_KR, text, |_| {});
    let (_, coords) = wght_700();
    assert_same_shaping(NOTO_KR, &out, text, &coords);

    let src = Face::parse_bytes(NOTO_KR, 0).unwrap();
    let sub = Face::parse_bytes(&out.bytes, 0).unwrap();
    let (src_vvar, sub_vvar) = (src.vvar().unwrap().unwrap(), sub.vvar().unwrap().unwrap());
    for &(old, new) in &out.gid_map {
        assert_eq!(
            sub_vvar.advance_height_delta(new, &coords),
            src_vvar.advance_height_delta(old, &coords),
            "advance delta of glyph {old}"
        );
        assert_eq!(
            vorg_delta(&out.bytes, new, &coords),
            vorg_delta(NOTO_KR, old, &coords),
            "vertical origin delta of glyph {old}"
        );
    }
    // The per mille sign's origin does vary: by 7.8 units at wght 700.
    let permille = src.cmap().unwrap().glyph_id('\u{2030}').unwrap();
    assert_eq!(vorg_delta(NOTO_KR, permille, &coords).round(), 8.0);
}

#[test]
fn an_instance_folds_the_vertical_origin_deltas_into_vorg() {
    let (normalized, coords) = wght_700();
    let src = Face::parse_bytes(NOTO_KR, 0).unwrap();
    let input = InstanceInput {
        coords: normalized,
        ..InstanceInput::default()
    };
    let out = instance(&src, &input).expect("instance succeeds");
    assert!(out.warnings.is_empty(), "{:?}", out.warnings);
    let inst = Face::parse_bytes(&out.bytes, 0).unwrap();
    assert!(
        inst.record(tag::VVAR).is_none(),
        "VVAR is baked in and goes"
    );

    let (src_vorg, inst_vorg) = (src.vorg().unwrap().unwrap(), inst.vorg().unwrap().unwrap());
    let (src_vmtx, inst_vmtx) = (src.vmtx().unwrap().unwrap(), inst.vmtx().unwrap().unwrap());
    let vvar = src.vvar().unwrap().unwrap();
    let mut moved = 0;
    for gid in 0..src.maxp().unwrap().num_glyphs {
        let delta = vorg_delta(NOTO_KR, gid, &coords);
        let want = (f32::from(src_vorg.vert_origin_y(gid)) + delta).round() as i16;
        assert_eq!(inst_vorg.vert_origin_y(gid), want, "origin of glyph {gid}");
        moved += usize::from(delta.round() != 0.0);
        let advance =
            f32::from(src_vmtx.advance(gid).unwrap()) + vvar.advance_height_delta(gid, &coords);
        assert_eq!(f32::from(inst_vmtx.advance(gid).unwrap()), advance.round());
    }
    assert_eq!(moved, 2, "the per mille sign and small roman one move");
    // Laid out vertically, the instance puts them at the moved origins:
    // 863 + 7.8 and 867 + 39.8.
    let ttb = shaped(&out.bytes, &[], "\u{2030}\u{2170}", Direction::Ttb);
    let offsets: Vec<i32> = ttb.iter().map(|g| g.5).collect();
    assert_eq!(offsets, [-871, -907]);
}

#[test]
fn strict_mode_keeps_the_vertical_tables() {
    // Every table of the fixture has a subset implementation now, so
    // strict mode, which rejects any it would have to drop, succeeds.
    let out = subset_text(NOTO_KR, "\u{300C}", |input| input.drop_unhandled = false);
    for table in [tag::VHEA, tag::VMTX, tag::VORG, tag::VVAR, tag::BASE, STAT] {
        assert!(has(&out.bytes, table));
    }
}

const STAT: [u8; 4] = *b"STAT";

#[test]
fn base_and_stat_survive_a_subset() {
    let out = subset_text(NOTO_KR, "\u{300C}\u{AC00}", |_| {});
    assert!(out.warnings.is_empty(), "{:?}", out.warnings);
    let src = Face::parse_bytes(NOTO_KR, 0).unwrap();
    let sub = Face::parse_bytes(&out.bytes, 0).unwrap();
    // Noto Sans KR's BASE has no reference glyphs, so it passes
    // through, as STAT always does.
    for table in [tag::BASE, STAT] {
        assert_eq!(
            sub.table_bytes(table).unwrap(),
            src.table_bytes(table).unwrap()
        );
    }
    let base = sub.base().unwrap().expect("BASE parses");
    let ideo = base
        .vertical_axis()
        .and_then(|axis| axis.script(*b"hang"))
        .and_then(|script| script.baseline(*b"ideo"));
    assert!(
        ideo.is_some(),
        "the vertical ideographic baseline of Hangul"
    );
}

/// A `BASE` with a horizontal axis whose one script (`latn`) has two
/// baselines: `ideo`, a format 2 coordinate on `ideo_glyph`, and
/// `romn`, a format 2 coordinate on `romn_glyph`. Returns the table and
/// the offsets of the two coordinates.
fn base_with_reference_glyphs(ideo_glyph: u16, romn_glyph: u16) -> (Vec<u8>, [usize; 2]) {
    let mut b = Vec::new();
    b.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // version 1.0
    b.extend_from_slice(&8u16.to_be_bytes()); // horizAxis
    b.extend_from_slice(&0u16.to_be_bytes()); // vertAxis
    b.extend_from_slice(&4u16.to_be_bytes()); // Axis 8: tag list at 12
    b.extend_from_slice(&14u16.to_be_bytes()); // script list at 22
    b.extend_from_slice(&2u16.to_be_bytes()); // BaseTagList 12
    b.extend_from_slice(b"ideoromn");
    b.extend_from_slice(&1u16.to_be_bytes()); // BaseScriptList 22
    b.extend_from_slice(b"latn");
    b.extend_from_slice(&8u16.to_be_bytes()); // BaseScript at 30
    b.extend_from_slice(&6u16.to_be_bytes()); // BaseValues at 36
    b.extend_from_slice(&0u16.to_be_bytes()); // no default MinMax
    b.extend_from_slice(&0u16.to_be_bytes()); // no language systems
    b.extend_from_slice(&1u16.to_be_bytes()); // BaseValues 36: default romn
    b.extend_from_slice(&2u16.to_be_bytes());
    b.extend_from_slice(&8u16.to_be_bytes()); // coord at 44
    b.extend_from_slice(&16u16.to_be_bytes()); // coord at 52
    for (y, glyph) in [(-240i16, ideo_glyph), (0, romn_glyph)] {
        b.extend_from_slice(&2u16.to_be_bytes());
        b.extend_from_slice(&y.to_be_bytes());
        b.extend_from_slice(&glyph.to_be_bytes());
        b.extend_from_slice(&3u16.to_be_bytes()); // baseCoordPoint
    }
    (b, [44, 52])
}

#[test]
fn base_reference_glyphs_follow_the_gid_map() {
    let src = Face::parse_bytes(OPEN_SANS, 0).unwrap();
    let cmap = src.cmap().unwrap();
    let (h, z) = (cmap.glyph_id('H').unwrap(), cmap.glyph_id('Z').unwrap());
    let (base, [ideo, romn]) = base_with_reference_glyphs(h, z);
    let font = support::edit_tables(OPEN_SANS, &[(tag::BASE, Some(base.clone()))]);
    let out = subset_text(&font, "Hi", |_| {});
    assert!(out.warnings.is_empty(), "{:?}", out.warnings);
    let sub = Face::parse_bytes(&out.bytes, 0).unwrap();
    let new = sub.table_bytes(tag::BASE).unwrap();
    assert_eq!(new.len(), base.len());
    let u16_at = |at: usize| u16::from_be_bytes([new[at], new[at + 1]]);
    // `H` is kept: its coordinate stays format 2, on H's new id.
    assert_eq!(u16_at(ideo), 2);
    assert_eq!(u32::from(u16_at(ideo + 4)), new_gid(&out, u32::from(h)));
    // `Z` is not: its coordinate becomes format 1, same value.
    assert_eq!(u16_at(romn), 1);
    let baselines = sub
        .base()
        .unwrap()
        .and_then(|b| b.horizontal_axis())
        .and_then(|axis| axis.script(*b"latn"))
        .map(|s| (s.baseline(*b"ideo"), s.baseline(*b"romn")));
    assert_eq!(baselines, Some((Some(-240), Some(0))));
}

#[test]
fn a_malformed_base_is_left_out_with_a_warning() {
    let (base, [ideo, _]) = base_with_reference_glyphs(1, 2);
    // Cut inside the first coordinate.
    let font = support::edit_tables(OPEN_SANS, &[(tag::BASE, Some(base[..ideo + 6].to_vec()))]);
    let out = subset_text(&font, "H", |_| {});
    assert!(!has(&out.bytes, tag::BASE));
    let warnings: Vec<([u8; 4], usize)> =
        out.warnings.iter().map(|w| (w.table, w.offset)).collect();
    assert_eq!(warnings, [(tag::BASE, ideo)]);
}

#[test]
fn keeping_every_glyph_passes_the_vertical_tables_through() {
    let src = Face::parse_bytes(NOTO_KR, 0).unwrap();
    let all: Vec<u16> = (0..src.maxp().unwrap().num_glyphs).collect();
    let input = SubsetInput {
        gids: all.clone(),
        ..SubsetInput::default()
    };
    let out = subset(&src, &input).unwrap();
    let sub = Face::parse_bytes(&out.bytes, 0).unwrap();
    for table in [tag::VHEA, tag::VMTX, tag::VORG, tag::VVAR] {
        assert_eq!(
            sub.table_bytes(table).unwrap(),
            src.table_bytes(table).unwrap()
        );
    }
    // Without variations the identity path drops VVAR with the other
    // variation tables, as the rebuilding path does.
    let input = SubsetInput {
        gids: all,
        retain_variations: false,
        ..SubsetInput::default()
    };
    let out = subset(&src, &input).unwrap();
    assert!(!has(&out.bytes, tag::VVAR));
    assert!(has(&out.bytes, tag::VMTX));
}

#[test]
fn dropping_variations_drops_vvar_and_keeps_the_metrics() {
    let text = "\u{300C}\u{2030}";
    let out = subset_text(NOTO_KR, text, |input| input.retain_variations = false);
    assert!(!has(&out.bytes, tag::VVAR));
    for table in [tag::VHEA, tag::VMTX, tag::VORG] {
        assert!(has(&out.bytes, table));
    }
    assert_same_shaping(NOTO_KR, &out, text, &[]);
}

#[test]
fn malformed_vertical_tables_are_left_out_with_warnings() {
    let src = Face::parse_bytes(NOTO_KR, 0).unwrap();
    let cut = |table: [u8; 4], len: usize| {
        let bytes = src.table_bytes(table).unwrap()[..len].to_vec();
        support::edit_tables(NOTO_KR, &[(table, Some(bytes))])
    };
    let warned = |font: &[u8]| {
        let out = subset_text(font, "\u{300C}", |_| {});
        // Vertical text still shapes, on the fallbacks.
        shaped(&out.bytes, &[], "\u{300C}", Direction::Ttb);
        let warnings: Vec<([u8; 4], usize)> =
            out.warnings.iter().map(|w| (w.table, w.offset)).collect();
        (out, warnings)
    };

    // A vmtx shorter than vhea and maxp say: vhea, vmtx and the VVAR
    // that varies them go, each reported; VORG stays.
    let (out, warnings) = warned(&cut(tag::VMTX, 10));
    assert_eq!(warnings, [(tag::VVAR, 0), (tag::VMTX, 10)]);
    for table in [tag::VHEA, tag::VMTX, tag::VVAR] {
        assert!(!has(&out.bytes, table));
    }
    assert!(has(&out.bytes, tag::VORG));

    // A VORG cut inside its entries goes alone.
    let (out, warnings) = warned(&cut(tag::VORG, 10));
    assert_eq!(warnings, [(tag::VORG, 8)]);
    assert!(!has(&out.bytes, tag::VORG));
    assert!(has(&out.bytes, tag::VMTX));

    // So does a truncated VVAR.
    let (out, warnings) = warned(&cut(tag::VVAR, 12));
    assert_eq!(warnings, [(tag::VVAR, 0)]);
    assert!(!has(&out.bytes, tag::VVAR));
    assert!(has(&out.bytes, tag::VMTX));
}

/// Open Sans with hand-built vertical tables: long metrics for the
/// first 40 glyphs and short ones after, and a `VORG` entry
/// for every third glyph.
fn open_sans_with_vertical_tables() -> Vec<u8> {
    let face = Face::parse_bytes(OPEN_SANS, 0).unwrap();
    let n = face.maxp().unwrap().num_glyphs;
    let long = n.min(40);
    let mut vhea = Vec::new();
    vhea.extend_from_slice(&0x0001_1000u32.to_be_bytes()); // version 1.1
    for v in [1100i16, -100, 0] {
        vhea.extend_from_slice(&v.to_be_bytes()); // ascent, descent, gap
    }
    vhea.extend_from_slice(&[0; 22]);
    vhea.extend_from_slice(&0i16.to_be_bytes()); // metricDataFormat
    vhea.extend_from_slice(&long.to_be_bytes());
    let mut vmtx = Vec::new();
    for gid in 0..n {
        if gid < long {
            vmtx.extend_from_slice(&(1000 + gid % 7).to_be_bytes());
        }
        vmtx.extend_from_slice(&((gid % 50) as i16).to_be_bytes());
    }
    let entries: Vec<u16> = (0..n).filter(|g| g % 3 == 0).collect();
    let mut vorg = Vec::new();
    vorg.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    vorg.extend_from_slice(&900i16.to_be_bytes());
    vorg.extend_from_slice(&(entries.len() as u16).to_be_bytes());
    for gid in entries {
        vorg.extend_from_slice(&gid.to_be_bytes());
        vorg.extend_from_slice(&(800 + (gid % 90) as i16).to_be_bytes());
    }
    // Open Sans kerns through a legacy `kern` table, which subsets
    // drop; take it out of the source too, so the two shape alike.
    support::edit_tables(
        OPEN_SANS,
        &[
            (tag::VHEA, Some(vhea)),
            (tag::VMTX, Some(vmtx)),
            (tag::VORG, Some(vorg)),
            (*b"kern", None),
        ],
    )
}

#[test]
fn a_glyf_subset_keeps_vertical_metrics_and_origins() {
    let font = open_sans_with_vertical_tables();
    let text = "Hamburgefonstiv";
    let out = subset_text(&font, text, |_| {});
    assert!(out.warnings.is_empty(), "{:?}", out.warnings);
    assert_same_shaping(&font, &out, text, &[]);

    let src = Face::parse_bytes(&font, 0).unwrap();
    let sub = Face::parse_bytes(&out.bytes, 0).unwrap();
    let (src_vmtx, sub_vmtx) = (src.vmtx().unwrap().unwrap(), sub.vmtx().unwrap().unwrap());
    let (src_vorg, sub_vorg) = (src.vorg().unwrap().unwrap(), sub.vorg().unwrap().unwrap());
    let mut short = 0;
    for &(old, new) in &out.gid_map {
        assert_eq!(sub_vmtx.advance(new), src_vmtx.advance(old), "glyph {old}");
        assert_eq!(sub_vmtx.tsb(new), src_vmtx.tsb(old), "glyph {old}");
        assert_eq!(sub_vorg.vert_origin_y(new), src_vorg.vert_origin_y(old));
        short += usize::from(old >= src.vhea().unwrap().unwrap().number_of_long_ver_metrics);
    }
    assert!(short > 0, "some kept glyphs had short metrics");
    assert!(sub_vorg.len() < src_vorg.len());
}
