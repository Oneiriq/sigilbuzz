# sigilbuzz-capi

A C library for [sigilbuzz](https://github.com/Oneiriq/sigilbuzz) that exports the
same symbols as HarfBuzz: `hb_blob_create`, `hb_face_create`, `hb_shape`, and so on.
C and C++ code written against HarfBuzz can link against it with no source changes.

## Installing

Install with [cargo-c](https://github.com/lu-zero/cargo-c) (0.10.0 or later):

```sh
cargo install cargo-c
cargo cinstall --release -p sigilbuzz-capi --prefix /usr/local
```

That installs everything a C or C++ project needs:

- `libsigilbuzz`, shared and static (`sigilbuzz.dll` plus import library on Windows)
- the header, as `include/sigilbuzz/hb.h`
- `sigilbuzz.pc` for pkg-config
- `SigilbuzzConfig.cmake` for CMake, in `share/cmake/Sigilbuzz/`

To stage the files somewhere else first (for a distro package, say), add
`--destdir <dir>`.

## Using it

With pkg-config, swap `harfbuzz` for `sigilbuzz`:

```sh
cc main.c $(pkg-config --cflags --libs sigilbuzz)
```

`--cflags` points at the `sigilbuzz` include directory, so `#include <hb.h>` works the
same as it does with HarfBuzz.

With CMake:

```cmake
find_package(Sigilbuzz REQUIRED)
target_link_libraries(myapp PRIVATE Sigilbuzz::Sigilbuzz)
```

The CMake package reads its flags from `sigilbuzz.pc`, so it needs pkg-config (or
pkgconf) installed. If you installed to a custom prefix, pass
`-DCMAKE_PREFIX_PATH=<prefix>`.

## Building without installing

`cargo build --release -p sigilbuzz-capi` also builds the library, but it names the
output `libsigilbuzz_capi` (`sigilbuzz_capi.dll` on Windows), because the name
`sigilbuzz` belongs to the core Rust crate. Link that with `-lsigilbuzz_capi` and
`-I crates/sigilbuzz-capi/include`. This is how the crate's own C tests build.

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
`hb.h` links and runs against this library without recompiling.

## Ownership

Reference counting follows HarfBuzz exactly, so code written for HarfBuzz neither
leaks nor double-frees here:

- The pointer is the object. `hb_*_reference(p)` adds a reference and returns `p`
  itself; `hb_*_destroy(p)` drops one and frees the object when the last one goes.
  This holds for `hb_blob_t`, `hb_face_t`, `hb_font_t`, `hb_buffer_t`, `hb_set_t`,
  and `hb_paint_funcs_t`.
- Every `*_create` result and every `*_reference` call is one reference to destroy.
- A face references its blob and a font references its face, so you may destroy the
  blob or face right after building on it.
- Referencing `NULL` returns `NULL`, and destroying `NULL` does nothing.
- Where HarfBuzz returns its inert empty object (for example from a zero-length
  `hb_blob_create`), sigilbuzz returns a fresh empty object. Destroy it as usual. The
  same code is correct with HarfBuzz, where destroying the inert object does nothing.

Earlier releases returned a new handle from every `hb_*_reference`. Each reference
still needs exactly one destroy, so balanced code keeps working; only the returned
pointer changed.

## Versioning

`hb_version()` returns `(8, 0, 0)` to signal compatibility with the HarfBuzz 8.x ABI.
`hb_version_string()` returns `"sigilbuzz X.Y.Z (hb-compatible)"`, so logs and bug
reports show which library is actually running.

## License

Apache-2.0. See the workspace root `LICENSE`.
