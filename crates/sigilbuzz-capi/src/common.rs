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

/// Reads a C string argument as bytes. A negative `len` means the
/// string is NUL-terminated.
///
/// # Safety
/// `s` must be non-null. When `len >= 0` it must point to `len`
/// readable bytes, otherwise to a NUL-terminated string. The bytes
/// must stay valid and unmodified for `'a`.
pub(crate) unsafe fn c_str_bytes<'a>(s: *const c_char, len: c_int) -> &'a [u8] {
    match usize::try_from(len) {
        // SAFETY: the caller guarantees `len` readable bytes at `s`.
        Ok(len) => unsafe { slice::from_raw_parts(s.cast::<u8>(), len) },
        // SAFETY: the caller guarantees a NUL-terminated string at `s`
        // when `len` is negative.
        Err(_) => unsafe { core::ffi::CStr::from_ptr(s) }.to_bytes(),
    }
}

/// True when `shaper_list` is null (the default list) or names the
/// `ot` shaper.
///
/// # Safety
/// `shaper_list` must be null or point to an array of NUL-terminated
/// strings that ends with a null pointer.
pub(crate) unsafe fn shaper_list_names_ot(shaper_list: *const *const c_char) -> bool {
    if shaper_list.is_null() {
        return true;
    }
    let mut i = 0usize;
    loop {
        // SAFETY: the caller guarantees a null-terminated array, and
        // the loop stops at the terminator, so index `i` is in bounds.
        let name = unsafe { *shaper_list.add(i) };
        if name.is_null() {
            return false;
        }
        // SAFETY: every entry before the terminator is a
        // NUL-terminated string, per the caller's contract.
        if unsafe { core::ffi::CStr::from_ptr(name) }.to_bytes() == b"ot" {
            return true;
        }
        i += 1;
    }
}

/// # Safety
/// `s` must be null, or point to at least `len` bytes (or be
/// NUL-terminated when `len == -1`).
#[no_mangle]
pub unsafe extern "C" fn hb_tag_from_string(s: *const c_char, len: c_int) -> hb_tag_t {
    if s.is_null() {
        return 0;
    }
    // SAFETY: `s` is non-null and the caller guarantees the length
    // contract of `c_str_bytes`.
    let bytes = unsafe { c_str_bytes(s, len) };
    let mut buf = [b' '; 4];
    for (dst, src) in buf.iter_mut().zip(bytes) {
        *dst = *src;
    }
    u32::from_be_bytes(buf)
}

/// Writes the four ASCII bytes of `tag` into `buf`. `buf` must point
/// to at least four writable bytes. HarfBuzz's `hb_tag_to_string`
/// signature does not include a length argument; the caller is
/// expected to size the buffer.
///
/// # Safety
/// `buf` must be null or writeable for at least four bytes.
#[no_mangle]
pub unsafe extern "C" fn hb_tag_to_string(tag: hb_tag_t, buf: *mut c_char) {
    if buf.is_null() {
        return;
    }
    let bytes = tag.to_be_bytes();
    for (i, b) in bytes.iter().enumerate() {
        // SAFETY: `buf` is non-null and the caller guarantees four
        // writable bytes. `i` is below 4.
        unsafe { *buf.add(i) = *b as c_char };
    }
}

/// # Safety
/// `s` must be null, or point to at least `len` bytes (or be
/// NUL-terminated when `len == -1`).
#[no_mangle]
pub unsafe extern "C" fn hb_direction_from_string(s: *const c_char, len: c_int) -> hb_direction_t {
    if s.is_null() {
        return HB_DIRECTION_INVALID;
    }
    // SAFETY: `s` is non-null and the caller guarantees the length
    // contract of `c_str_bytes`.
    let bytes = unsafe { c_str_bytes(s, len) };
    // HarfBuzz only inspects the first character (case-insensitive).
    let Some(first) = bytes.first() else {
        return HB_DIRECTION_INVALID;
    };
    match first.to_ascii_lowercase() {
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
/// the same pointer. As in C, the tag ends at the first NUL byte even
/// when `len` counts past it.
///
/// # Safety
/// `s` must be null, or point to at least `len` bytes (or be
/// NUL-terminated when `len == -1`).
#[no_mangle]
pub unsafe extern "C" fn hb_language_from_string(s: *const c_char, len: c_int) -> hb_language_t {
    if s.is_null() {
        return ptr::null();
    }
    // SAFETY: `s` is non-null and the caller guarantees the length
    // contract of `c_str_bytes`.
    let bytes = unsafe { c_str_bytes(s, len) };
    let bytes = bytes.split(|&b| b == 0).next().unwrap_or_default();
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
        // SAFETY: `major` is non-null and the caller guarantees it
        // points to a writable `unsigned int`.
        unsafe { *major = HB_COMPAT_MAJOR };
    }
    if !minor.is_null() {
        // SAFETY: `minor` is non-null and the caller guarantees it
        // points to a writable `unsigned int`.
        unsafe { *minor = HB_COMPAT_MINOR };
    }
    if !micro.is_null() {
        // SAFETY: `micro` is non-null and the caller guarantees it
        // points to a writable `unsigned int`.
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

// Language interning. Each distinct tag is leaked once into a
// Mutex<Vec<&'static CStr>> and looked up by value afterward.

#[cfg(feature = "std")]
fn intern_language(tag: &str) -> hb_language_t {
    use std::sync::{Mutex, OnceLock, PoisonError};
    static INTERN: OnceLock<Mutex<Vec<&'static core::ffi::CStr>>> = OnceLock::new();
    let intern = INTERN.get_or_init(|| Mutex::new(Vec::new()));
    // The table stays consistent even if a holder panicked: entries
    // are only ever appended whole.
    let mut guard = intern.lock().unwrap_or_else(PoisonError::into_inner);
    for cstr in guard.iter() {
        if cstr.to_bytes() == tag.as_bytes() {
            return cstr.as_ptr();
        }
    }
    // A tag with a NUL byte would be stored under a different key
    // than it is looked up by, and leak again on every call. The
    // caller strips NUL bytes, so this only guards the invariant.
    let Ok(owned) = std::ffi::CString::new(tag) else {
        return lang_und();
    };
    // Leak a fresh CString: language tags survive the lifetime of
    // the process, just as they do in HarfBuzz itself.
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
