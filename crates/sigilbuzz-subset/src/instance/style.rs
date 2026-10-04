//! The `OS/2` and `post` fields an instance sets from where it puts its
//! axes and from its advances, as HarfBuzz's instancer sets them:
//!
//! - `usWeightClass` from the `wght` location, rounded and clamped to
//!   1 to 1000;
//! - `usWidthClass` from the `wdth` location, through the spec's
//!   percentage table;
//! - `post.italicAngle` from the `slnt` location, clamped to -90 to 90;
//! - `xAvgCharWidth`, the mean of the instance's advances that are not
//!   zero.
//!
//! An axis' location is what HarfBuzz records for it: the value a
//! pinned axis takes, in the axis' own units. An axis kept variable has
//! none, and its fields stay.

use super::glyf::clamp_i16;

/// Each axis' tag, and its location in user units when the instance
/// pins it.
pub(super) type AxisLocations = [([u8; 4], Option<f32>)];

/// The location of the first axis tagged `tag` that has one.
fn location(axes: &AxisLocations, tag: [u8; 4]) -> Option<f32> {
    axes.iter()
        .filter(|(t, _)| *t == tag)
        .find_map(|(_, v)| *v)
        .filter(|v| v.is_finite())
}

/// HarfBuzz's `map_wdth_to_widthclass`: the `usWidthClass`, before
/// rounding, of a `wdth` location, interpolating between the spec's
/// classes (50%, 62.5%, 75%, 87.5%, 100%, 112.5%, 125%, 150% and 200%).
fn width_class(width: f32) -> f32 {
    if width < 50.0 {
        return 1.0;
    }
    if width > 200.0 {
        return 9.0;
    }
    let ratio = (width - 50.0) / 12.5;
    let mut a = ratio.floor() as i32;
    let mut b = ratio.ceil() as i32;
    if b <= 6 {
        if a == b {
            return a as f32 + 1.0;
        }
    } else if b == 7 {
        // No class for 137.5%.
        a = 6;
        b = 8;
    } else if b == 8 {
        if a == b {
            return 8.0;
        }
        a = 6;
    } else {
        if a == b && a == 12 {
            return 9.0;
        }
        b = 12;
        a = 8;
    }
    let va = 50.0 + a as f32 * 12.5;
    let vb = 50.0 + b as f32 * 12.5;
    let mut class = a as f32 + (width - va) / (vb - va);
    if a <= 6 {
        class += 1.0;
    }
    class
}

/// Writes a big-endian `u16` at `off` when `table` is long enough.
fn write_u16(table: &mut [u8], off: usize, v: u16) {
    if let Some(field) = table.get_mut(off..).and_then(<[u8]>::first_chunk_mut::<2>) {
        *field = v.to_be_bytes();
    }
}

/// Sets the `OS/2` fields an instance sets: `xAvgCharWidth` (byte 2)
/// to `avg_char_width` when given, and `usWeightClass` (4) and
/// `usWidthClass` (6) from the `wght` and `wdth` locations in `axes`.
/// A field past the end of a short table is left out.
pub(super) fn patch_os2(os2: &mut [u8], avg_char_width: Option<u16>, axes: &AxisLocations) {
    if let Some(avg) = avg_char_width {
        let avg = clamp_i16(i32::from(avg));
        write_u16(os2, 2, avg as u16);
    }
    if let Some(weight) = location(axes, *b"wght") {
        // `roundf`, halves away from zero; the value is positive.
        write_u16(os2, 4, weight.clamp(1.0, 1000.0).round() as u16);
    }
    if let Some(width) = location(axes, *b"wdth") {
        write_u16(os2, 6, width_class(width).round() as u16);
    }
}

/// Sets `post.italicAngle` (bytes 4 to 7, 16.16) from the `slnt`
/// location in `axes`, when it has one.
pub(super) fn patch_post(post: &mut [u8], axes: &AxisLocations) {
    let Some(slant) = location(axes, *b"slnt") else {
        return;
    };
    // HarfBuzz's `set_float`: `roundf (f * 65536)`, halves away from
    // zero.
    let angle = (slant.clamp(-90.0, 90.0) * 65536.0).round() as i32;
    if let Some(field) = post.get_mut(4..).and_then(<[u8]>::first_chunk_mut::<4>) {
        *field = angle.to_be_bytes();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn width_classes_match_the_spec_table() {
        let table = [
            (50.0, 1.0),
            (62.5, 2.0),
            (75.0, 3.0),
            (87.5, 4.0),
            (100.0, 5.0),
            (112.5, 6.0),
            (125.0, 7.0),
            (150.0, 8.0),
            (200.0, 9.0),
        ];
        for (width, class) in table {
            assert_eq!(width_class(width), class, "{width}");
        }
        // Between classes, and past either end.
        assert_eq!(width_class(93.75), 4.5);
        assert_eq!(width_class(137.5), 7.5);
        assert_eq!(width_class(175.0), 8.5);
        assert_eq!(width_class(25.0), 1.0);
        assert_eq!(width_class(300.0), 9.0);
    }

    #[test]
    fn os2_takes_weight_width_and_average() {
        let mut os2 = [0u8; 8];
        let axes = [(*b"wght", Some(651.5)), (*b"wdth", Some(93.75))];
        patch_os2(&mut os2, Some(577), &axes);
        assert_eq!(os2, [0, 0, 0x02, 0x41, 0x02, 0x8C, 0, 5]);
        // Out of range, and a short table.
        let mut short = [0u8; 5];
        patch_os2(&mut short, Some(40_000), &[(*b"wght", Some(5000.0))]);
        assert_eq!(short, [0, 0, 0x7F, 0xFF, 0]);
    }

    #[test]
    fn kept_axes_leave_their_fields() {
        let mut os2 = [7u8; 8];
        patch_os2(&mut os2, None, &[(*b"wght", None), (*b"wdth", None)]);
        assert_eq!(os2, [7u8; 8]);
        let mut post = [7u8; 8];
        patch_post(&mut post, &[(*b"slnt", None)]);
        assert_eq!(post, [7u8; 8]);
    }

    #[test]
    fn post_takes_the_slant() {
        let mut post = [0u8; 8];
        patch_post(&mut post, &[(*b"slnt", Some(-7.5))]);
        assert_eq!(&post[4..], (-491_520i32).to_be_bytes());
        patch_post(&mut post, &[(*b"slnt", Some(-120.0))]);
        assert_eq!(&post[4..], (-90i32 * 65536).to_be_bytes());
    }
}
