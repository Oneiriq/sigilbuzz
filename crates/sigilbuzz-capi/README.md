# sigilbuzz-capi

A C library for [sigilbuzz](https://github.com/Oneiriq/sigilbuzz) that exports the
same symbols as HarfBuzz: `hb_blob_create`, `hb_face_create`, `hb_shape`, and so on.
C and C++ code written against HarfBuzz can link against it with no source changes.

## Building and linking

The crate builds a shared library (`cdylib`) and a static library (`staticlib`):

```sh
cargo build --release -p sigilbuzz-capi
```

Cargo names the output `libsigilbuzz_capi` (`sigilbuzz_capi.dll` on Windows). The
bundled pkg-config file (`sigilbuzz.pc.in`) and CMake module
(`cmake/SigilbuzzConfig.cmake.in`) expect the library to be installed as
`libsigilbuzz`, so rename it when you install it. After that, swap `-lharfbuzz` for
`-lsigilbuzz` on your link line and use the header in `include/hb.h`.

The Rust `rlib` target exists only for the crate's own tests.

## What it covers

Blobs, faces, fonts, buffers, shaping, tags, directions, scripts, languages, and
version queries. It also covers `hb_set_t`, the `hb_subset_*` functions, the
`hb_paint_funcs_t` paint callbacks, `hb_face_collect_unicodes`, and
`hb_ot_layout_collect_features`. `include/hb.h` declares exactly what is implemented.

The subset and paint functions sit behind the `subset` and `paint` cargo features. Both
are on by default. For a smaller library, build with
`--no-default-features --features std`.

## ABI

Enum values (`HB_DIRECTION_LTR == 4`, `HB_SCRIPT_LATIN == HB_TAG('L','a','t','n')`, and
the rest) and struct layouts match HarfBuzz, so a binary compiled against the real
`hb.h` links and runs against this library without recompiling. The opaque types
(`hb_blob_t`, `hb_face_t`, `hb_font_t`, `hb_buffer_t`) are reference-counted with
Rust's `Arc`. `hb_*_destroy` drops a reference and `hb_*_reference` adds one, the same
manual refcounting HarfBuzz uses.

## Versioning

`hb_version()` returns `(8, 0, 0)` to signal compatibility with the HarfBuzz 8.x ABI.
`hb_version_string()` returns `"sigilbuzz X.Y.Z (hb-compatible)"`, so logs and bug
reports show which library is actually running.

## License

Apache-2.0. See the workspace root `LICENSE`.
