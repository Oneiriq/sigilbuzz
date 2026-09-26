//! Tag, direction, script and language helpers, the version entry
//! points, and the internal direction mapping and language interning.

#[cfg(feature = "std")]
use alloc::boxed::Box;
use alloc::string::String;
#[cfg(feature = "std")]
use alloc::vec::Vec;
use core::ffi::{c_char, c_int, c_uint};
use core::ptr;
use core::slice;

use sigilbuzz::Direction;

use crate::{
    hb_direction_t, hb_language_t, hb_script_t, hb_tag_t, HB_DIRECTION_BTT, HB_DIRECTION_INVALID,
    HB_DIRECTION_LTR, HB_DIRECTION_RTL, HB_DIRECTION_TTB,
};

// ---------------------------------------------------------------------------
// Tag / Direction / Script / Language helpers
// ---------------------------------------------------------------------------

/// # Safety
/// `s` must point to at least `len` bytes (or be NUL-terminated when
/// `len == -1`).
#[no_mangle]
pub unsafe extern "C" fn hb_tag_from_string(s: *const c_char, len: c_int) -> hb_tag_t {
    if s.is_null() {
        return 0;
    }
    let bytes: &[u8] = if len < 0 {
        // SAFETY: caller asserts NUL-terminated.
        unsafe { core::ffi::CStr::from_ptr(s) }.to_bytes()
    } else {
        // SAFETY: caller asserts (s, len).
        unsafe { slice::from_raw_parts(s.cast::<u8>(), len as usize) }
    };
    let mut buf = [b' '; 4];
    for (i, b) in bytes.iter().take(4).enumerate() {
        buf[i] = *b;
    }
    u32::from_be_bytes(buf)
}

/// Writes the four ASCII bytes of `tag` into `buf`. `buf` must point
/// to at least four writable bytes. HarfBuzz's `hb_tag_to_string`
/// signature does not include a length argument; the caller is
/// expected to size the buffer.
///
/// # Safety
/// `buf` must be writeable for at least four bytes.
#[no_mangle]
pub unsafe extern "C" fn hb_tag_to_string(tag: hb_tag_t, buf: *mut c_char) {
    if buf.is_null() {
        return;
    }
    let bytes = tag.to_be_bytes();
    for (i, b) in bytes.iter().enumerate() {
        // SAFETY: caller asserts buf has 4 writable bytes.
        unsafe { *buf.add(i) = *b as c_char };
    }
}

/// # Safety
/// `s` must point to at least `len` bytes (or be NUL-terminated when
/// `len == -1`).
#[no_mangle]
pub unsafe extern "C" fn hb_direction_from_string(s: *const c_char, len: c_int) -> hb_direction_t {
    if s.is_null() {
        return HB_DIRECTION_INVALID;
    }
    let bytes: &[u8] = if len < 0 {
        // SAFETY: caller asserts NUL-terminated.
        unsafe { core::ffi::CStr::from_ptr(s) }.to_bytes()
    } else {
        // SAFETY: caller asserts (s, len).
        unsafe { slice::from_raw_parts(s.cast::<u8>(), len as usize) }
    };
    if bytes.is_empty() {
        return HB_DIRECTION_INVALID;
    }
    // HarfBuzz only inspects the first character (case-insensitive).
    match bytes[0].to_ascii_lowercase() {
        b'l' => HB_DIRECTION_LTR,
        b'r' => HB_DIRECTION_RTL,
        b't' => HB_DIRECTION_TTB,
        b'b' => HB_DIRECTION_BTT,
        _ => HB_DIRECTION_INVALID,
    }
}

/// HarfBuzz's `hb_script_from_iso15924_tag` is a passthrough: the
/// tag IS the script identifier.
#[no_mangle]
pub extern "C" fn hb_script_from_iso15924_tag(tag: hb_tag_t) -> hb_script_t {
    tag
}

/// Languages are pointer-interned. We leak a CString the first time
/// we see a given normalized language tag; subsequent lookups return
/// the same pointer.
///
/// # Safety
/// `s` must point to at least `len` bytes (or be NUL-terminated when
/// `len == -1`).
#[no_mangle]
pub unsafe extern "C" fn hb_language_from_string(s: *const c_char, len: c_int) -> hb_language_t {
    if s.is_null() {
        return ptr::null();
    }
    let bytes: &[u8] = if len < 0 {
        // SAFETY: caller asserts NUL-terminated.
        unsafe { core::ffi::CStr::from_ptr(s) }.to_bytes()
    } else {
        // SAFETY: caller asserts (s, len).
        unsafe { slice::from_raw_parts(s.cast::<u8>(), len as usize) }
    };
    if bytes.is_empty() {
        return ptr::null();
    }
    let normalised: String = bytes
        .iter()
        .map(|b| b.to_ascii_lowercase() as char)
        .collect();
    intern_language(&normalised)
}

// ---------------------------------------------------------------------------
// Version
// ---------------------------------------------------------------------------

/// HarfBuzz ABI version sigilbuzz advertises: 8.2.0, the release that
/// added the `color_glyph` paint callback, the newest piece of the
/// hb-paint surface this crate implements. Consumers that gate on
/// `hb_version_atleast(8, 2, 0)` find it. The actual sigilbuzz
/// version is exposed via `hb_version_string()`.
const HB_COMPAT_MAJOR: c_uint = 8;
const HB_COMPAT_MINOR: c_uint = 2;
const HB_COMPAT_MICRO: c_uint = 0;

/// # Safety
/// All pointers, when non-null, must be writeable.
#[no_mangle]
pub unsafe extern "C" fn hb_version(major: *mut c_uint, minor: *mut c_uint, micro: *mut c_uint) {
    if !major.is_null() {
        // SAFETY: caller asserts writeable.
        unsafe { *major = HB_COMPAT_MAJOR };
    }
    if !minor.is_null() {
        // SAFETY: caller asserts writeable.
        unsafe { *minor = HB_COMPAT_MINOR };
    }
    if !micro.is_null() {
        // SAFETY: caller asserts writeable.
        unsafe { *micro = HB_COMPAT_MICRO };
    }
}

/// Returns a static, NUL-terminated string identifying the
/// implementation. The pointer is valid for the lifetime of the
/// process.
#[no_mangle]
pub extern "C" fn hb_version_string() -> *const c_char {
    // Compose at compile time.
    // Format: "sigilbuzz X.Y.Z (hb-compatible)"
    static VERSION: &[u8] = concat!(
        "sigilbuzz ",
        env!("CARGO_PKG_VERSION"),
        " (hb-compatible)\0"
    )
    .as_bytes();
    VERSION.as_ptr().cast::<c_char>()
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// The core direction for a valid `hb_direction_t`, `None` for
/// `HB_DIRECTION_INVALID` and every other value HarfBuzz's
/// `HB_DIRECTION_IS_VALID` rejects.
pub(crate) fn map_direction_in(d: hb_direction_t) -> Option<Direction> {
    match d {
        HB_DIRECTION_LTR => Some(Direction::Ltr),
        HB_DIRECTION_RTL => Some(Direction::Rtl),
        HB_DIRECTION_TTB => Some(Direction::Ttb),
        HB_DIRECTION_BTT => Some(Direction::Btt),
        _ => None,
    }
}

// Language interning. A handful of well-known tags are pinned at
// startup; new tags are interned via a simple Mutex<Vec<&'static
// CStr>>.

#[cfg(feature = "std")]
fn intern_language(tag: &str) -> hb_language_t {
    use std::sync::{Mutex, OnceLock};
    static INTERN: OnceLock<Mutex<Vec<&'static core::ffi::CStr>>> = OnceLock::new();
    let intern = INTERN.get_or_init(|| Mutex::new(Vec::new()));
    let mut guard = intern.lock().unwrap();
    for cstr in guard.iter() {
        if cstr.to_bytes() == tag.as_bytes() {
            return cstr.as_ptr();
        }
    }
    // Leak a fresh CString: language tags survive the lifetime of
    // the process, just as they do in HarfBuzz itself.
    let owned = std::ffi::CString::new(tag).unwrap_or_else(|_| std::ffi::CString::default());
    let leaked: &'static core::ffi::CStr = Box::leak(owned.into_boxed_c_str());
    guard.push(leaked);
    leaked.as_ptr()
}

#[cfg(not(feature = "std"))]
fn intern_language(_tag: &str) -> hb_language_t {
    // Without std we cannot maintain a runtime intern table; return
    // the canonical "und" so consumers get a stable, non-null
    // pointer. Real applications that care about language-tag
    // dispatch link with std on.
    lang_und()
}

pub(crate) fn lang_und() -> hb_language_t {
    static UND: &[u8] = b"und\0";
    UND.as_ptr().cast::<c_char>()
}
