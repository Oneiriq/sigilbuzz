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

Blobs, faces, fonts, buffers (with HarfBuzz's buffer flags and cluster levels, which
default to `HB_BUFFER_FLAG_DEFAULT` and `HB_BUFFER_CLUSTER_LEVEL_MONOTONE_GRAPHEMES` as
in HarfBuzz), shaping (with HarfBuzz's glyph flags in `hb_glyph_info_t::mask`, read
with `hb_glyph_info_get_glyph_flags`), tags, directions, scripts, languages, and version queries. It also covers `hb_set_t`, the `hb_subset_*` functions, the
`hb_paint_funcs_t` paint callbacks, `hb_face_collect_unicodes`, and
`hb_ot_layout_collect_features`. `include/hb.h` declares exactly what is implemented.

The subset and paint functions sit behind the `subset` and `paint` cargo features. Both
are on by default. For a smaller library, build with
`--no-default-features --features std`.

## Painting color glyphs

The paint API matches HarfBuzz 8.0's `hb-paint.h`, plus the `color_glyph` callback
HarfBuzz added in 8.2:

- Setters are `hb_paint_funcs_set_X_func(funcs, func, user_data, destroy)` for
  `push_transform`, `pop_transform`, `color_glyph`, `push_clip_glyph`,
  `push_clip_rectangle`, `pop_clip`, `color`, `image`, `linear_gradient`,
  `radial_gradient`, `sweep_gradient`, `push_group`, `pop_group`, and
  `custom_palette_color`. Every
  callback gets its own `user_data` as its last argument. `destroy` runs when the
  callback is replaced, when the funcs object is freed, or right away for a NULL
  `func` or after `hb_paint_funcs_make_immutable`.
- `hb_color_t` packs blue in the high byte and alpha in the low byte (`HB_COLOR(b, g,
  r, a)`), and `hb_color_line_t` is HarfBuzz's public struct, so C code may call its
  function pointers directly.
- `hb_font_paint_glyph` fires callbacks in HarfBuzz 11's order: a clip rectangle at
  font scale (the glyph's ClipList box, or the bounds of its paint tree) and a root
  transform to font scale around COLRv1 glyphs, inverse-root / clip / root around each
  `PaintGlyph`, the `color_glyph` offer and then the ClipList box of every glyph a
  `PaintColrGlyph` references, one transform per transform paint, two groups per
  composite with the mode on `pop_group`, and sweep angles in radians as
  `(stored angle + 1) * pi`. A COLRv1 glyph whose paint no clip bounds paints nothing
  inside its root transform. COLRv0 layers and plain glyphs paint as
  `push_clip_glyph`, `color`, `pop_clip`.
- Colors resolve as in HarfBuzz: entry `0xFFFF` is the foreground (`is_foreground =
  1`), other entries try `custom_palette_color`, then the CPAL palette, then fall back
  to the foreground (`is_foreground = 0`). The paint alpha multiplies the alpha byte
  and is truncated.

Differences from HarfBuzz: there is no `hb_paint_funcs_get_empty`,
`hb_paint_funcs_set_user_data`, or `hb_paint_*` emitter functions, and the `image`
callback for SVG and bitmap glyphs is never fired.
`push_clip_glyph` expects the outline at font scale, as `hb_font_draw_glyph` would draw
it, but sigilbuzz does not export `hb_font_draw_glyph`, so callers bring their own
outlines.

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
  `hb_subset_input_t`, and `hb_paint_funcs_t`.
- Every `*_create` result and every `*_reference` call is one reference to destroy.
- A face references its blob and a font references its face, so you may destroy the
  blob or face right after building on it.
- `hb_subset_input_unicode_set` and `hb_subset_input_glyph_set` return a set owned by
  the input. The same pointer comes back on every call and stays valid until the input
  is destroyed. Never destroy it; take `hb_set_reference` if you need it longer.
- Referencing `NULL` returns `NULL`, and destroying `NULL` does nothing.
- Where HarfBuzz returns its inert empty object (for example from a zero-length
  `hb_blob_create`), sigilbuzz returns a fresh empty object. Destroy it as usual. The
  same code is correct with HarfBuzz, where destroying the inert object does nothing.

Earlier releases returned a new handle from every `hb_*_reference`. Each reference
still needs exactly one destroy, so balanced code keeps working; only the returned
pointer changed. Earlier releases also made the caller destroy the set a subset
accessor returned. Code written that way double-frees now, as it would under
HarfBuzz: drop those `hb_set_destroy` calls.

## Versioning

`hb_version()` returns `(8, 2, 0)`, the HarfBuzz release whose API this crate covers (8.2 added
the `color_glyph` paint callback).
`hb_version_string()` returns `"sigilbuzz X.Y.Z (hb-compatible)"`, so logs and bug
reports show which library is actually running.

## License

Apache-2.0. See the workspace root `LICENSE`.
