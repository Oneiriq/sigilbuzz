//! Subsets and instances arbitrary font bytes, then parses the output.
#![no_main]

use libfuzzer_sys::fuzz_target;
use sigilbuzz::Face;
use sigilbuzz_fuzz::{axis_coords, glyph_ids, num_glyphs, split_control, Knobs};
use sigilbuzz_subset::{instance, subset, AxisPin, InstanceInput, SubsetInput};

fuzz_target!(|data: &[u8]| {
    let (control, font) = split_control(data, 24);
    let mut knobs = Knobs::new(control);
    let Ok(face) = Face::parse_bytes(font, 0) else { return };

    let mut gids = glyph_ids(&face, &mut knobs);
    if knobs.byte() % 4 == 0 {
        // Keep every glyph, which takes the identity passthrough paths.
        gids = (0..num_glyphs(&face)).collect();
    }
    let flags = knobs.byte();
    let input = SubsetInput {
        gids,
        retain_hints: flags & 1 != 0,
        drop_unhandled: flags & 2 != 0,
        retain_layout: flags & 4 != 0,
        retain_variations: flags & 8 != 0,
    };
    if let Ok(out) = subset(&face, &input) {
        if let Ok(reparsed) = Face::parse_bytes(&out.bytes, 0) {
            let _ = reparsed.glyph_outline(0);
        }
    }

    let coords = axis_coords(&face, &mut knobs);
    let pins_mode = knobs.byte();
    let axis_pins = match pins_mode % 3 {
        0 => Vec::new(),
        1 => coords.iter().map(|_| AxisPin::Pin).collect(),
        _ => coords
            .iter()
            .enumerate()
            .map(|(i, _)| if (usize::from(pins_mode) >> (i % 8)) & 1 == 0 { AxisPin::Keep } else { AxisPin::Pin })
            .collect(),
    };
    let input = InstanceInput { coords, drop_var_tables: flags & 16 != 0, axis_pins };
    if let Ok(out) = instance(&face, &input) {
        let _ = Face::parse_bytes(&out.bytes, 0);
    }
});
