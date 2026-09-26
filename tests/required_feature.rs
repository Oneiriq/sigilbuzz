//! LangSys required features: HarfBuzz applies them even when their
//! tag is not one the shaper enables, or when the caller turned it off
//! (`hb_ot_map_builder_t::compile` then runs them in GSUB stage 0).
//!
//! None of the vendored fonts has a required feature, so each test
//! patches one: a GSUB (Open Sans) or GPOS (Source Sans) `latn`
//! default language system gets one of its own features as the
//! required feature. HarfBuzz adds a GPOS required feature to the one
//! GPOS stage, again whatever its tag. Output is compared with
//! rustybuzz 0.20 on the same patched bytes.

use rustybuzz::ttf_parser::Tag;
use sigilbuzz::{shape, Blob, Buffer, Direction, Face, Feature, Font};

const OPEN_SANS: &[u8] = include_bytes!("fixtures/opensans_regular.ttf");

fn u16_at(data: &[u8], at: usize) -> usize {
    usize::from(u16::from_be_bytes([data[at], data[at + 1]]))
}

/// Open Sans with the feature tagged `tag` in the GSUB `latn` default
/// language system made that language system's required feature.
fn with_required(tag: &[u8; 4]) -> Vec<u8> {
    with_required_in(OPEN_SANS, b"GSUB", b"latn", tag)
}

/// `font` with the feature tagged `tag` in the default language system
/// of `script` in `table` (GSUB or GPOS) made that language system's
/// required feature.
fn with_required_in(font: &[u8], table: &[u8; 4], script: &[u8; 4], tag: &[u8; 4]) -> Vec<u8> {
    let mut data = font.to_vec();
    let tables = u16_at(&data, 4);
    let layout = (0..tables)
        .map(|i| 12 + 16 * i)
        .find(|&rec| &data[rec..rec + 4] == table)
        .map(|rec| u32::from_be_bytes(data[rec + 8..rec + 12].try_into().unwrap()) as usize)
        .expect("layout table");
    let script_list = layout + u16_at(&data, layout + 4);
    let feature_list = layout + u16_at(&data, layout + 6);
    let script_table = (0..u16_at(&data, script_list))
        .map(|i| script_list + 2 + 6 * i)
        .find(|&rec| &data[rec..rec + 4] == script)
        .map(|rec| script_list + u16_at(&data, rec + 4))
        .expect("script");
    let lang_sys = script_table + u16_at(&data, script_table);
    let index = (0..u16_at(&data, lang_sys + 4))
        .map(|i| u16_at(&data, lang_sys + 6 + 2 * i))
        .find(|&f| &data[feature_list + 2 + 6 * f..feature_list + 6 + 6 * f] == tag)
        .expect("feature in the default language system");
    data[lang_sys + 2..lang_sys + 4].copy_from_slice(&(index as u16).to_be_bytes());
    data
}

fn sigilbuzz_ids(data: &[u8], text: &str, features: &[Feature]) -> Vec<(u32, i32)> {
    let blob = Blob::new(data);
    let face = Face::parse(&blob, 0).expect("parse sigilbuzz face");
    let font = Font::new(face, 1000.0);
    let mut buffer = Buffer::new();
    buffer.push_str(text);
    shape(&font, &buffer, features)
        .expect("sigilbuzz shape")
        .glyphs
        .iter()
        .map(|g| (g.glyph_id, g.x_advance))
        .collect()
}

/// Glyph ids and all four position fields, from sigilbuzz shaping
/// `text` top to bottom when `vertical` is set.
fn sigilbuzz_positions(data: &[u8], text: &str, features: &[Feature], vertical: bool) -> Vec<Pos> {
    let blob = Blob::new(data);
    let face = Face::parse(&blob, 0).expect("parse sigilbuzz face");
    let font = Font::new(face, 1000.0);
    let mut buffer = Buffer::new();
    buffer.push_str(text);
    buffer.set_direction(if vertical {
        Direction::Ttb
    } else {
        Direction::Ltr
    });
    shape(&font, &buffer, features)
        .expect("sigilbuzz shape")
        .glyphs
        .iter()
        .map(|g| (g.glyph_id, g.x_advance, g.y_advance, g.x_offset, g.y_offset))
        .collect()
}

/// [`sigilbuzz_positions`] for rustybuzz.
fn rustybuzz_positions(data: &[u8], text: &str, features: &[Feature], vertical: bool) -> Vec<Pos> {
    let face = rustybuzz::Face::from_slice(data, 0).expect("parse rustybuzz face");
    let features: Vec<rustybuzz::Feature> = features
        .iter()
        .map(|f| rustybuzz::Feature::new(Tag::from_bytes(&f.tag), f.value, ..))
        .collect();
    let mut buffer = rustybuzz::UnicodeBuffer::new();
    buffer.push_str(text);
    buffer.guess_segment_properties();
    buffer.set_direction(if vertical {
        rustybuzz::Direction::TopToBottom
    } else {
        rustybuzz::Direction::LeftToRight
    });
    let out = rustybuzz::shape(&face, &features, buffer);
    out.glyph_infos()
        .iter()
        .zip(out.glyph_positions())
        .map(|(i, p)| (i.glyph_id, p.x_advance, p.y_advance, p.x_offset, p.y_offset))
        .collect()
}

/// Glyph id, x/y advance, x/y offset.
type Pos = (u32, i32, i32, i32, i32);

fn rustybuzz_ids(data: &[u8], text: &str, features: &[Feature]) -> Vec<(u32, i32)> {
    let face = rustybuzz::Face::from_slice(data, 0).expect("parse rustybuzz face");
    let features: Vec<rustybuzz::Feature> = features
        .iter()
        .map(|f| rustybuzz::Feature::new(Tag::from_bytes(&f.tag), f.value, ..))
        .collect();
    let mut buffer = rustybuzz::UnicodeBuffer::new();
    buffer.push_str(text);
    let out = rustybuzz::shape(&face, &features, buffer);
    out.glyph_infos()
        .iter()
        .zip(out.glyph_positions())
        .map(|(i, p)| (i.glyph_id, p.x_advance))
        .collect()
}

#[test]
fn required_feature_with_an_unrequested_tag_still_applies() {
    // `onum` (old-style figures) is not a feature the pipeline enables.
    let patched = with_required(b"onum");
    let text = "Year 2024";
    let ours = sigilbuzz_ids(&patched, text, &[]);
    assert_eq!(ours, rustybuzz_ids(&patched, text, &[]));
    assert_ne!(
        ours,
        sigilbuzz_ids(OPEN_SANS, text, &[]),
        "onum must change the digits"
    );
}

#[test]
fn required_feature_applies_when_the_caller_disables_its_tag() {
    let patched = with_required(b"liga");
    let text = "office flight";
    let off = [Feature {
        tag: *b"liga",
        value: 0,
    }];
    let ours = sigilbuzz_ids(&patched, text, &off);
    assert_eq!(ours, rustybuzz_ids(&patched, text, &off));
    // The ligatures form despite liga=0, unlike in the unpatched font.
    assert!(ours.len() < sigilbuzz_ids(OPEN_SANS, text, &off).len());
    // And they form exactly once when liga stays on.
    assert_eq!(
        sigilbuzz_ids(&patched, text, &[]),
        rustybuzz_ids(&patched, text, &[])
    );
}

#[test]
fn required_feature_with_a_default_tag_joins_that_tag() {
    // `salt` is not requested; `liga` as required and requested is the
    // merged case. Both must match rustybuzz with extra user features.
    let patched = with_required(b"salt");
    let text = "Big salt fish 42";
    let on = [Feature {
        tag: *b"salt",
        value: 1,
    }];
    assert_eq!(
        sigilbuzz_ids(&patched, text, &on),
        rustybuzz_ids(&patched, text, &on)
    );
    assert_eq!(
        sigilbuzz_ids(&patched, text, &[]),
        rustybuzz_ids(&patched, text, &[])
    );
}

const SOURCE_SANS: &[u8] = include_bytes!("fonts/SourceSans3VF-Latin-Subset.otf");

#[test]
fn gpos_required_feature_applies_when_the_caller_disables_its_tag() {
    // HarfBuzz adds a required feature's lookups to the GPOS stage
    // whatever the caller says about its tag.
    let patched = with_required_in(SOURCE_SANS, b"GPOS", b"latn", b"kern");
    let text = "AVATAR Type";
    let off = [Feature {
        tag: *b"kern",
        value: 0,
    }];
    let ours = sigilbuzz_positions(&patched, text, &off, false);
    assert_eq!(ours, rustybuzz_positions(&patched, text, &off, false));
    assert_ne!(
        ours,
        sigilbuzz_positions(SOURCE_SANS, text, &off, false),
        "the required kern feature must still kern"
    );
}

#[test]
fn gpos_required_feature_with_an_unrequested_tag_still_applies() {
    // Source Sans's kerning feature retagged `zkrn`, a tag nothing
    // requests, then made the `latn` required feature: it still kerns.
    let renamed = rename_feature(SOURCE_SANS, b"GPOS", b"kern", b"zkrn");
    let patched = with_required_in(&renamed, b"GPOS", b"latn", b"zkrn");
    let text = "AVATAR Type";
    let ours = sigilbuzz_positions(&patched, text, &[], false);
    assert_eq!(ours, rustybuzz_positions(&patched, text, &[], false));
    assert_ne!(
        ours,
        sigilbuzz_positions(&renamed, text, &[], false),
        "the required feature must kern"
    );
}

/// `font` with the FeatureList records of `table` tagged `from`
/// retagged `to`.
fn rename_feature(font: &[u8], table: &[u8; 4], from: &[u8; 4], to: &[u8; 4]) -> Vec<u8> {
    let mut data = font.to_vec();
    let layout = (0..u16_at(&data, 4))
        .map(|i| 12 + 16 * i)
        .find(|&rec| &data[rec..rec + 4] == table)
        .map(|rec| u32::from_be_bytes(data[rec + 8..rec + 12].try_into().unwrap()) as usize)
        .expect("layout table");
    let feature_list = layout + u16_at(&data, layout + 6);
    for i in 0..u16_at(&data, feature_list) {
        let rec = feature_list + 2 + 6 * i;
        if &data[rec..rec + 4] == from {
            data[rec..rec + 4].copy_from_slice(to);
        }
    }
    data
}
