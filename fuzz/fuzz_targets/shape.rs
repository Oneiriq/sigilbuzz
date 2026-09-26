//! Shapes text in every supported script against arbitrary font bytes, so the
//! GSUB, GPOS, GDEF, kern, morx, and kerx interpreters see hostile tables.
#![no_main]

use libfuzzer_sys::fuzz_target;
use sigilbuzz::Face;
use sigilbuzz_fuzz::{shape_samples, split_control, Knobs};

fuzz_target!(|data: &[u8]| {
    let (control, font) = split_control(data, 16);
    let mut knobs = Knobs::new(control);
    if let Ok(face) = Face::parse_bytes(font, 0) {
        shape_samples(face, &mut knobs, None);
    }
});
