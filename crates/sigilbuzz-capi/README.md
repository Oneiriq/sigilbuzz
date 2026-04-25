# sigilbuzz-capi

A HarfBuzz-symbol-compatible C API for [sigilbuzz](https://github.com/Oneiriq/sigilbuzz).

The crate compiles to a `cdylib` (`libsigilbuzz.dylib` / `.so` / `.dll`)
and a `staticlib` that exports the exact symbol names HarfBuzz uses —
`hb_blob_create`, `hb_face_create`, `hb_shape`, etc. A consumer that
already links against `-lharfbuzz` swaps the link line for `-lsigilbuzz`
and recompiles; no source-level changes are required.

## Surface

The implemented subset of `hb.h` covers blob / face / font / buffer /
shape / tag / direction / script / language / version. Subset and
paint bridges are gated behind cargo features and follow on. See
`include/hb.h` for the canonical header — the file mirrors HarfBuzz's
public API one declaration at a time.

## ABI

Every enum value (`HB_DIRECTION_LTR == 4`, `HB_SCRIPT_LATIN ==
HB_TAG('L','a','t','n')`, …) and struct layout matches HarfBuzz's
spec, so a binary previously compiled against the original `hb.h`
links and runs against this library without recompilation. The
opaque types (`hb_blob_t`, `hb_face_t`, `hb_font_t`, `hb_buffer_t`)
are reference-counted via Rust's `Arc`; `hb_*_destroy` decrements the
count and `hb_*_reference` increments it, matching HarfBuzz's
manual-refcount discipline.

## Versioning

`hb_version()` returns `(8, 0, 0)` to advertise compatibility with
HarfBuzz 8.x's stable ABI surface. `hb_version_string()` returns
`"sigilbuzz X.Y.Z (hb-compatible)"` so logging and bug reports
identify the actual implementation.

## Distribution

`sigilbuzz.pc.in` and `cmake/SigilbuzzConfig.cmake.in` ship alongside
the crate so packagers can produce `pkg-config` and CMake find-module
artifacts pointing at the installed cdylib. The Rust-level rlib
target is for in-workspace tests only.
