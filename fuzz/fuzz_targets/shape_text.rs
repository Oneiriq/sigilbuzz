//! Shapes arbitrary text with real fonts. This reaches the buffer, bidi,
//! normalization, script-run, and per-script shaper code with valid tables
//! and hostile strings.
#![no_main]

use libfuzzer_sys::fuzz_target;
use sigilbuzz::{BidiInfo, Buffer, Face};
use sigilbuzz_fuzz::{shape_samples, split_control, Knobs};

const FONTS: &[&[u8]] = &[
    include_bytes!("../../tests/fixtures/opensans_regular.ttf"),
    include_bytes!("../../tests/fixtures/amiri_regular.ttf"),
    include_bytes!("../../tests/fixtures/rubik_vf.ttf"),
    include_bytes!("../../tests/fonts/NotoSansDevanagari-Regular.ttf"),
    include_bytes!("../../tests/fonts/NotoSansHebrew-Regular.ttf"),
    include_bytes!("../../tests/fonts/NotoSansKhmer-Regular.ttf"),
    include_bytes!("../../tests/fonts/NotoSansMyanmar-Regular.ttf"),
    include_bytes!("../../tests/fonts/NotoSansThai-Regular.ttf"),
    include_bytes!("../../tests/fonts/NotoSansMongolian-Regular.ttf"),
    include_bytes!("../../tests/fonts/NotoSerifTibetan-Regular.ttf"),
    include_bytes!("../../tests/fonts/NotoSansOldHangul-Subset.ttf"),
    include_bytes!("../../tests/fonts/NotoSansTamil-Regular.ttf"),
    include_bytes!("../../tests/fonts/NotoSansSharada-Regular.ttf"),
    include_bytes!("../../tests/fonts/SourceSans3VF-Latin-Subset.otf"),
];

fuzz_target!(|data: &[u8]| {
    let (control, text_bytes) = split_control(data, 8);
    let mut knobs = Knobs::new(control);
    let text = String::from_utf8_lossy(text_bytes);

    let info = BidiInfo::new(&text, None);
    let _ = info.levels();
    let _ = info.reorder();

    let mut buffer = Buffer::new();
    buffer.set_text_bidi(&text);
    if let Some(map) = buffer.bidi_map() {
        for i in [0, 1, text.len() / 2, text.len(), text.len() + 1, usize::MAX] {
            let _ = map.visual_to_logical(i);
            let _ = map.logical_to_visual(i);
        }
    }

    let font = FONTS[usize::from(knobs.byte()) % FONTS.len()];
    if let Ok(face) = Face::parse_bytes(font, 0) {
        shape_samples(face, &mut knobs, Some(&text));
    }
});
