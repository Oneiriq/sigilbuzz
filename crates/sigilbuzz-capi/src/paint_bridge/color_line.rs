//! `hb_color_line_t`: the color stops and extend mode a gradient
//! callback reads back through `hb_color_line_get_color_stops` and
//! `hb_color_line_get_extend`.
//!
//! HarfBuzz hands every gradient callback a color line and lets the
//! callee pull the stops out of it. The line is only valid for the
//! duration of the callback. sigilbuzz resolves the stops (palette,
//! alpha, foreground) before firing the callback, so the line is a
//! plain array the accessors copy from.

extern crate alloc;

use alloc::vec::Vec;
use core::ffi::c_uint;
use core::ptr;

use sigilbuzz_paint::Extend;

use super::hb_color_t;
use crate::hb_bool_t;

/// One resolved color stop. Layout matches HarfBuzz's
/// `hb_color_stop_t`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct hb_color_stop_t {
    /// Position along the color line.
    pub offset: f32,
    /// Nonzero when the stop uses the foreground color passed to
    /// `hb_font_paint_glyph` (COLR palette entry `0xFFFF`).
    pub is_foreground: hb_bool_t,
    /// Packed BGRA color, alpha already applied.
    pub color: hb_color_t,
}

/// HarfBuzz's `hb_paint_extend_t`.
pub type hb_paint_extend_t = c_uint;
/// Replicate the first / last color outward.
pub const HB_PAINT_EXTEND_PAD: hb_paint_extend_t = 0;
/// Repeat the gradient.
pub const HB_PAINT_EXTEND_REPEAT: hb_paint_extend_t = 1;
/// Repeat the gradient, reflecting every other copy.
pub const HB_PAINT_EXTEND_REFLECT: hb_paint_extend_t = 2;

/// Opaque color-line handle passed to the gradient callbacks. Read it
/// with [`hb_color_line_get_color_stops`] and
/// [`hb_color_line_get_extend`]; it is valid only while the callback
/// that received it runs.
#[repr(C)]
pub struct hb_color_line_t {
    _opaque: [u8; 0],
}

/// The data behind an `hb_color_line_t` pointer. Lives on the stack of
/// the dispatcher for the duration of one gradient callback.
pub(crate) struct ResolvedColorLine {
    stops: Vec<hb_color_stop_t>,
    extend: hb_paint_extend_t,
}

impl ResolvedColorLine {
    pub(crate) fn new(stops: Vec<hb_color_stop_t>, extend: Extend) -> Self {
        let extend = match extend {
            Extend::Pad => HB_PAINT_EXTEND_PAD,
            Extend::Repeat => HB_PAINT_EXTEND_REPEAT,
            Extend::Reflect => HB_PAINT_EXTEND_REFLECT,
        };
        Self { stops, extend }
    }

    /// The opaque pointer handed to C. Valid while `self` is.
    pub(crate) fn as_ptr(&self) -> *const hb_color_line_t {
        ptr::from_ref(self).cast::<hb_color_line_t>()
    }
}

/// Copies up to `*count` stops starting at `start` into `color_stops`,
/// stores how many were copied in `*count`, and returns the total
/// number of stops on the line. When `count` or `color_stops` is null
/// nothing is copied and only the total is returned, as in HarfBuzz.
///
/// # Safety
/// `color_line` must be null or the pointer a gradient callback
/// received, used before that callback returns. When both `count` and
/// `color_stops` are non-null, `color_stops` must have room for
/// `*count` stops.
#[no_mangle]
pub unsafe extern "C" fn hb_color_line_get_color_stops(
    color_line: *const hb_color_line_t,
    start: c_uint,
    count: *mut c_uint,
    color_stops: *mut hb_color_stop_t,
) -> c_uint {
    if color_line.is_null() {
        if !count.is_null() {
            // SAFETY: caller asserts `count` is writable.
            unsafe { *count = 0 };
        }
        return 0;
    }
    // SAFETY: caller asserts the pointer came from a live callback, so
    // it points at a `ResolvedColorLine` on the dispatcher's stack.
    let line = unsafe { &*color_line.cast::<ResolvedColorLine>() };
    let total = line.stops.len();
    if !count.is_null() && !color_stops.is_null() {
        // SAFETY: caller asserts `count` is readable and writable.
        let capacity = unsafe { *count } as usize;
        let available = line.stops.get(start as usize..).unwrap_or(&[]);
        let n = capacity.min(available.len());
        // SAFETY: caller asserts `color_stops` has room for `capacity`
        // stops and `n <= capacity`; the source is a live slice that
        // cannot overlap caller memory.
        unsafe {
            ptr::copy_nonoverlapping(available.as_ptr(), color_stops, n);
            *count = n as c_uint;
        }
    }
    total as c_uint
}

/// Returns the line's extend mode. A null line reports
/// `HB_PAINT_EXTEND_PAD`.
///
/// # Safety
/// `color_line` must be null or the pointer a gradient callback
/// received, used before that callback returns.
#[no_mangle]
pub unsafe extern "C" fn hb_color_line_get_extend(
    color_line: *const hb_color_line_t,
) -> hb_paint_extend_t {
    if color_line.is_null() {
        return HB_PAINT_EXTEND_PAD;
    }
    // SAFETY: see `hb_color_line_get_color_stops`.
    let line = unsafe { &*color_line.cast::<ResolvedColorLine>() };
    line.extend
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stop(offset: f32, color: hb_color_t, is_foreground: bool) -> hb_color_stop_t {
        hb_color_stop_t {
            offset,
            is_foreground: hb_bool_t::from(is_foreground),
            color,
        }
    }

    fn sample_line() -> ResolvedColorLine {
        ResolvedColorLine::new(
            alloc::vec![
                stop(0.0, 0xFF00_00FF, false),
                stop(0.5, 0x80FF_FFFF, true),
                stop(1.0, 0xFF00_FF00, false),
            ],
            Extend::Reflect,
        )
    }

    #[test]
    fn total_only_when_count_or_buffer_is_null() {
        let line = sample_line();
        let mut out = [stop(9.0, 0, false); 2];
        let mut count: c_uint = 2;
        unsafe {
            assert_eq!(
                hb_color_line_get_color_stops(line.as_ptr(), 0, ptr::null_mut(), out.as_mut_ptr()),
                3
            );
            assert_eq!(
                hb_color_line_get_color_stops(line.as_ptr(), 0, &mut count, ptr::null_mut()),
                3
            );
        }
        // Nothing written, count untouched.
        assert_eq!(count, 2);
        assert_eq!(out[0].offset, 9.0);
    }

    #[test]
    fn copies_a_window_and_reports_count() {
        let line = sample_line();
        let mut out = [stop(9.0, 0, false); 4];
        let mut count: c_uint = 4;
        let total = unsafe {
            hb_color_line_get_color_stops(line.as_ptr(), 1, &mut count, out.as_mut_ptr())
        };
        assert_eq!(total, 3);
        assert_eq!(count, 2, "two stops remain after start = 1");
        assert_eq!(out[0], stop(0.5, 0x80FF_FFFF, true));
        assert_eq!(out[1], stop(1.0, 0xFF00_FF00, false));
        assert_eq!(out[2].offset, 9.0, "untouched past count");

        let mut count: c_uint = 1;
        unsafe { hb_color_line_get_color_stops(line.as_ptr(), 0, &mut count, out.as_mut_ptr()) };
        assert_eq!(count, 1);
        assert_eq!(out[0], stop(0.0, 0xFF00_00FF, false));

        let mut count: c_uint = 4;
        unsafe { hb_color_line_get_color_stops(line.as_ptr(), 7, &mut count, out.as_mut_ptr()) };
        assert_eq!(count, 0, "start past the end copies nothing");
    }

    #[test]
    fn extend_maps_to_harfbuzz_values() {
        for (extend, want) in [
            (Extend::Pad, HB_PAINT_EXTEND_PAD),
            (Extend::Repeat, HB_PAINT_EXTEND_REPEAT),
            (Extend::Reflect, HB_PAINT_EXTEND_REFLECT),
        ] {
            let line = ResolvedColorLine::new(alloc::vec::Vec::new(), extend);
            assert_eq!(unsafe { hb_color_line_get_extend(line.as_ptr()) }, want);
        }
    }

    #[test]
    fn null_line_is_empty_pad() {
        let mut count: c_uint = 5;
        unsafe {
            assert_eq!(
                hb_color_line_get_color_stops(ptr::null(), 0, &mut count, ptr::null_mut()),
                0
            );
            assert_eq!(hb_color_line_get_extend(ptr::null()), HB_PAINT_EXTEND_PAD);
        }
        assert_eq!(count, 0);
    }
}
