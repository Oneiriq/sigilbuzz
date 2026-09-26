//! `hb_color_line_t`: the color stops and extend mode a gradient
//! callback reads back.
//!
//! HarfBuzz makes `hb_color_line_t` a public struct: a `data` pointer,
//! a `get_color_stops` and a `get_extend` function with their own
//! `user_data`, and eight reserved pointers. The
//! `hb_color_line_get_color_stops` and `hb_color_line_get_extend`
//! accessors just call through those function pointers, so a caller
//! may build its own color line and pass it to them. sigilbuzz lays
//! the struct out the same way.
//!
//! The lines sigilbuzz hands to gradient callbacks resolve stop colors
//! lazily, when the callback asks for them, the same way HarfBuzz
//! does: `custom_palette_color` runs at that point, once per fetched
//! stop. A line is valid only while the callback that received it
//! runs.

use core::ffi::{c_uint, c_void};
use core::ptr;

use sigilbuzz_paint::walk::{ColorLineRef, StopRef};
use sigilbuzz_paint::Extend;

use super::color::hb_color_t;
use super::Colors;
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
    /// Packed color, stop alpha already applied.
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

type GetColorStopsFn = unsafe extern "C" fn(
    color_line: *mut hb_color_line_t,
    color_line_data: *mut c_void,
    start: c_uint,
    count: *mut c_uint,
    color_stops: *mut hb_color_stop_t,
    user_data: *mut c_void,
) -> c_uint;
type GetExtendFn = unsafe extern "C" fn(
    color_line: *mut hb_color_line_t,
    color_line_data: *mut c_void,
    user_data: *mut c_void,
) -> hb_paint_extend_t;

/// `hb_color_line_get_color_stops_func_t`.
pub type hb_color_line_get_color_stops_func_t = Option<GetColorStopsFn>;
/// `hb_color_line_get_extend_func_t`.
pub type hb_color_line_get_extend_func_t = Option<GetExtendFn>;

/// HarfBuzz's `hb_color_line_t`, field for field.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct hb_color_line_t {
    /// Passed back to both functions as `color_line_data`.
    pub data: *mut c_void,
    /// Copies stops out; see [`hb_color_line_get_color_stops`].
    pub get_color_stops: hb_color_line_get_color_stops_func_t,
    /// `user_data` for `get_color_stops`.
    pub get_color_stops_user_data: *mut c_void,
    /// Reports the extend mode.
    pub get_extend: hb_color_line_get_extend_func_t,
    /// `user_data` for `get_extend`.
    pub get_extend_user_data: *mut c_void,
    /// Reserved; HarfBuzz leaves these null.
    pub reserved0: *mut c_void,
    /// Reserved.
    pub reserved1: *mut c_void,
    /// Reserved.
    pub reserved2: *mut c_void,
    /// Reserved.
    pub reserved3: *mut c_void,
    /// Reserved.
    pub reserved5: *mut c_void,
    /// Reserved.
    pub reserved6: *mut c_void,
    /// Reserved.
    pub reserved7: *mut c_void,
    /// Reserved.
    pub reserved8: *mut c_void,
}

/// Copies up to `*count` stops starting at `start` into `color_stops`,
/// stores how many were copied in `*count`, and returns the total
/// number of stops on the line. When `count` or `color_stops` is null
/// nothing is copied and only the total is returned. Calls the line's
/// own `get_color_stops`, as HarfBuzz does. A null line, or one
/// without that function, has no stops.
///
/// # Safety
/// `color_line` must be null or point at a valid `hb_color_line_t`
/// whose function honors the contract above (every line sigilbuzz
/// hands a gradient callback does, until the callback returns). When
/// both `count` and `color_stops` are non-null, `color_stops` must
/// have room for `*count` stops.
#[no_mangle]
pub unsafe extern "C" fn hb_color_line_get_color_stops(
    color_line: *mut hb_color_line_t,
    start: c_uint,
    count: *mut c_uint,
    color_stops: *mut hb_color_stop_t,
) -> c_uint {
    let func = if color_line.is_null() {
        None
    } else {
        // SAFETY: the caller guarantees `color_line` is valid; these are
        // plain field reads, no reference outlives them.
        unsafe { (*color_line).get_color_stops }
    };
    let Some(func) = func else {
        if !count.is_null() {
            // SAFETY: the caller guarantees `count` is writable.
            unsafe { *count = 0 };
        }
        return 0;
    };
    // SAFETY: as above; the function receives the arguments HarfBuzz
    // passes it, and the caller's buffers.
    unsafe {
        let data = (*color_line).data;
        let user_data = (*color_line).get_color_stops_user_data;
        func(color_line, data, start, count, color_stops, user_data)
    }
}

/// Returns the line's extend mode through its own `get_extend`, as
/// HarfBuzz does. A null line, or one without that function, reports
/// `HB_PAINT_EXTEND_PAD`.
///
/// # Safety
/// `color_line` must be null or point at a valid `hb_color_line_t`.
#[no_mangle]
pub unsafe extern "C" fn hb_color_line_get_extend(
    color_line: *mut hb_color_line_t,
) -> hb_paint_extend_t {
    if color_line.is_null() {
        return HB_PAINT_EXTEND_PAD;
    }
    // SAFETY: the caller guarantees `color_line` is valid.
    unsafe {
        match (*color_line).get_extend {
            Some(func) => func(
                color_line,
                (*color_line).data,
                (*color_line).get_extend_user_data,
            ),
            None => HB_PAINT_EXTEND_PAD,
        }
    }
}

/// What a sigilbuzz color line's `data` points at.
struct LineData<'l> {
    stops: &'l [StopRef],
    extend: hb_paint_extend_t,
    colors: &'l Colors<'l>,
}

fn extend_to_hb(extend: Extend) -> hb_paint_extend_t {
    match extend {
        Extend::Pad => HB_PAINT_EXTEND_PAD,
        Extend::Repeat => HB_PAINT_EXTEND_REPEAT,
        Extend::Reflect => HB_PAINT_EXTEND_REFLECT,
    }
}

/// Builds the color line for `line` and runs `f` with a pointer to it.
/// The pointer is valid only inside `f`.
pub(crate) fn with_color_line(
    line: ColorLineRef<'_>,
    colors: &Colors<'_>,
    f: impl FnOnce(*mut hb_color_line_t),
) {
    let data = LineData {
        stops: line.stops,
        extend: extend_to_hb(line.extend),
        colors,
    };
    let mut color_line = hb_color_line_t {
        data: ptr::from_ref(&data).cast_mut().cast::<c_void>(),
        get_color_stops: Some(line_color_stops),
        get_color_stops_user_data: ptr::null_mut(),
        get_extend: Some(line_extend),
        get_extend_user_data: ptr::null_mut(),
        reserved0: ptr::null_mut(),
        reserved1: ptr::null_mut(),
        reserved2: ptr::null_mut(),
        reserved3: ptr::null_mut(),
        reserved5: ptr::null_mut(),
        reserved6: ptr::null_mut(),
        reserved7: ptr::null_mut(),
        reserved8: ptr::null_mut(),
    };
    f(&mut color_line);
}

/// `get_color_stops` for sigilbuzz's lines. Resolves each copied stop
/// through the paint context, so `custom_palette_color` fires here.
unsafe extern "C" fn line_color_stops(
    _color_line: *mut hb_color_line_t,
    data: *mut c_void,
    start: c_uint,
    count: *mut c_uint,
    color_stops: *mut hb_color_stop_t,
    _user_data: *mut c_void,
) -> c_uint {
    // SAFETY: `data` is the `LineData` `with_color_line` set up, alive
    // until the gradient callback returns; it is only read.
    let line = unsafe { &*data.cast::<LineData<'_>>() };
    if !count.is_null() && !color_stops.is_null() {
        // SAFETY: the caller guarantees `count` is readable.
        let capacity = unsafe { *count } as usize;
        let available = line.stops.get(start as usize..).unwrap_or(&[]);
        let n = capacity.min(available.len());
        for (i, stop) in available[..n].iter().enumerate() {
            let (is_foreground, color) = line.colors.get_color(stop.color);
            let out = hb_color_stop_t {
                offset: stop.offset,
                is_foreground,
                color,
            };
            // SAFETY: the caller guarantees room for `capacity` stops
            // and `i < n <= capacity`.
            unsafe { color_stops.add(i).write(out) };
        }
        // SAFETY: the caller guarantees `count` is writable.
        unsafe { *count = n as c_uint };
    }
    line.stops.len() as c_uint
}

/// `get_extend` for sigilbuzz's lines.
unsafe extern "C" fn line_extend(
    _color_line: *mut hb_color_line_t,
    data: *mut c_void,
    _user_data: *mut c_void,
) -> hb_paint_extend_t {
    // SAFETY: see `line_color_stops`.
    unsafe { &*data.cast::<LineData<'_>>() }.extend
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A caller-built line whose stops live in `data`, the way C code
    /// can construct one.
    unsafe extern "C" fn fixed_stops(
        _line: *mut hb_color_line_t,
        data: *mut c_void,
        start: c_uint,
        count: *mut c_uint,
        out: *mut hb_color_stop_t,
        user_data: *mut c_void,
    ) -> c_uint {
        assert_eq!(user_data as usize, 0xAB);
        let stops = unsafe { &*data.cast::<[hb_color_stop_t; 3]>() };
        if !count.is_null() && !out.is_null() {
            let avail = stops.get(start as usize..).unwrap_or(&[]);
            let n = (unsafe { *count } as usize).min(avail.len());
            unsafe {
                ptr::copy_nonoverlapping(avail.as_ptr(), out, n);
                *count = n as c_uint;
            }
        }
        3
    }

    unsafe extern "C" fn fixed_extend(
        _line: *mut hb_color_line_t,
        _data: *mut c_void,
        user_data: *mut c_void,
    ) -> hb_paint_extend_t {
        user_data as usize as hb_paint_extend_t
    }

    fn stop(offset: f32, color: hb_color_t) -> hb_color_stop_t {
        hb_color_stop_t {
            offset,
            is_foreground: 0,
            color,
        }
    }

    #[test]
    fn accessors_call_through_a_caller_built_line() {
        let mut stops = [stop(0.0, 1), stop(0.5, 2), stop(1.0, 3)];
        let mut line = hb_color_line_t {
            data: stops.as_mut_ptr().cast(),
            get_color_stops: Some(fixed_stops),
            get_color_stops_user_data: 0xAB as *mut c_void,
            get_extend: Some(fixed_extend),
            get_extend_user_data: HB_PAINT_EXTEND_REFLECT as usize as *mut c_void,
            reserved0: ptr::null_mut(),
            reserved1: ptr::null_mut(),
            reserved2: ptr::null_mut(),
            reserved3: ptr::null_mut(),
            reserved5: ptr::null_mut(),
            reserved6: ptr::null_mut(),
            reserved7: ptr::null_mut(),
            reserved8: ptr::null_mut(),
        };
        let mut out = [stop(9.0, 0); 4];
        let mut count: c_uint = 4;
        unsafe {
            let total = hb_color_line_get_color_stops(&mut line, 1, &mut count, out.as_mut_ptr());
            assert_eq!((total, count), (3, 2));
            assert_eq!(out[..2], [stop(0.5, 2), stop(1.0, 3)]);
            assert_eq!(
                hb_color_line_get_color_stops(&mut line, 0, ptr::null_mut(), ptr::null_mut()),
                3
            );
            assert_eq!(hb_color_line_get_extend(&mut line), HB_PAINT_EXTEND_REFLECT);
        }
    }

    #[test]
    fn null_line_and_null_functions_are_empty_pad() {
        let mut count: c_uint = 5;
        unsafe {
            assert_eq!(
                hb_color_line_get_color_stops(ptr::null_mut(), 0, &mut count, ptr::null_mut()),
                0
            );
            assert_eq!(count, 0);
            assert_eq!(
                hb_color_line_get_extend(ptr::null_mut()),
                HB_PAINT_EXTEND_PAD
            );
            let mut empty = hb_color_line_t {
                data: ptr::null_mut(),
                get_color_stops: None,
                get_color_stops_user_data: ptr::null_mut(),
                get_extend: None,
                get_extend_user_data: ptr::null_mut(),
                reserved0: ptr::null_mut(),
                reserved1: ptr::null_mut(),
                reserved2: ptr::null_mut(),
                reserved3: ptr::null_mut(),
                reserved5: ptr::null_mut(),
                reserved6: ptr::null_mut(),
                reserved7: ptr::null_mut(),
                reserved8: ptr::null_mut(),
            };
            let mut count: c_uint = 5;
            assert_eq!(
                hb_color_line_get_color_stops(&mut empty, 0, &mut count, ptr::null_mut()),
                0
            );
            assert_eq!(count, 0);
            assert_eq!(hb_color_line_get_extend(&mut empty), HB_PAINT_EXTEND_PAD);
        }
    }

    #[test]
    fn layout_is_five_fields_plus_eight_reserved_pointers() {
        assert_eq!(
            core::mem::size_of::<hb_color_line_t>(),
            13 * core::mem::size_of::<*mut c_void>()
        );
        assert_eq!(core::mem::size_of::<hb_color_stop_t>(), 12);
    }
}
