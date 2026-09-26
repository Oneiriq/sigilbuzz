//! LangSys required features: HarfBuzz applies them even when their
//! tag is not one the shaper enables, or when the caller turned it off
//! (`hb_ot_map_builder_t::compile` then runs them in GSUB stage 0).
//!
//! None of the vendored fonts has a required feature, so each test
//! patches Open Sans: its `latn` default language system gets one of
//! its own features as the required feature. Output is compared with
//! rustybuzz 0.20 on the same patched bytes.

use rustybuzz::ttf_parser::Tag;
use sigilbuzz::{shape, Blob, Buffer, Face, Feature, Font};

const OPEN_SANS: &[u8] = include_bytes!("fixtures/opensans_regular.ttf");

fn u16_at(data: &[u8], at: usize) -> usize {
    usize::from(u16::from_be_bytes([data[at], data[at + 1]]))
}

/// Open Sans with the feature tagged `tag` in the GSUB `latn` default
/// language system made that language system's required feature.
fn with_required(tag: &[u8; 4]) -> Vec<u8> {
    let mut data = OPEN_SANS.to_vec();
    let tables = u16_at(&data, 4);
    let gsub = (0..tables)
        .map(|i| 12 + 16 * i)
        .find(|&rec| &data[rec..rec + 4] == b"GSUB")
        .map(|rec| u32::from_be_bytes(data[rec + 8..rec + 12].try_into().unwrap()) as usize)
        .expect("GSUB table");
    let script_list = gsub + u16_at(&data, gsub + 4);
    let feature_list = gsub + u16_at(&data, gsub + 6);
    let latn = (0..u16_at(&data, script_list))
        .map(|i| script_list + 2 + 6 * i)
        .find(|&rec| &data[rec..rec + 4] == b"latn")
        .map(|rec| script_list + u16_at(&data, rec + 4))
        .expect("latn script");
    let lang_sys = latn + u16_at(&data, latn);
    let index = (0..u16_at(&data, lang_sys + 4))
        .map(|i| u16_at(&data, lang_sys + 6 + 2 * i))
        .find(|&f| &data[feature_list + 2 + 6 * f..feature_list + 6 + 6 * f] == tag)
        .expect("feature in the latn default language system");
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
