# External dependencies

The sigilbuzz core (the root crate) has no runtime dependencies. All of the parsing,
shaping, and outline math is written in this repo. Companion crates may take a narrow
runtime dependency when it earns its place, and every one of them is listed here.

A dependency has to clear this bar:

1. It replaces work that would be impractical to write in-house at this stage.
2. It has a permissive OSI license (Apache-2.0, MIT, or BSD).
3. It is close to a leaf, with no large transitive tree.
4. It sits behind a cargo feature where possible, so consumers who don't need it pay
   nothing.

## `brotli` (in `sigilbuzz-woff`)

- Version: `8.x`.
- License: BSD-3-Clause or MIT.
- Where: `crates/sigilbuzz-woff/Cargo.toml`, behind the `woff2` feature. The feature
  is on by default. Turn it off if you only need WOFF1.
- Why: WOFF2 compresses all of its tables into one Brotli stream. `unwrap_woff2` needs
  to decompress it and `wrap_woff2` needs to compress it. Brotli is its own spec (RFC
  7932), and an encoder written from scratch would dwarf the rest of the WOFF2 code.
  The `brotli` crate is the standard Rust port and handles both directions.
- Transitive dependencies: `alloc-no-stdlib` and `alloc-stdlib`. Both are small,
  `no_std`-capable, BSD-3 or MIT, and from the same author.
- History: through 0.6.0 the crate used `brotli-decompressor` 5, which only
  decompresses. 0.7.0 switched to `brotli` 8 to get the encoder for `wrap_woff2`. It
  comes from the same project, and the decompression API behaves the same, so nothing
  changed for `unwrap_woff2` callers.

## `miniz_oxide` (in `sigilbuzz-woff` and `sigilbuzz-render`)

- Version: `0.9.x`, built with `default-features = false, features = ["with-alloc"]`.
- License: MIT, Apache-2.0, or Zlib.
- Transitive dependencies: `adler2`, a single-file Adler-32 checksum (MIT, Apache-2.0,
  or Zlib) with no dependencies of its own. The optional `simd-adler32` backend stays
  off. The checksums in WOFF1 and PNG streams are short enough that the scalar version
  is the right default.

In `sigilbuzz-woff` it sits behind the `woff1-deflate` feature, which is on by default.
WOFF1 zlib-compresses each table whose compressed form is smaller than the original.
Unwrapping needs to inflate those tables, and compressing on wrap is what makes WOFF1
worth sending over the network. A deflate codec (RFC 1950 and RFC 1951, plus Adler-32)
is a project of its own and has nothing to do with fonts. Turn the feature off and
`unwrap_woff1` rejects compressed tables, while `wrap_woff1` writes uncompressed
tables only.

In `sigilbuzz-render` it is always on. The rasterizer decodes PNG images stored in
`CBDT` and `sbix` glyphs, and PNG image data is a zlib stream. The rest of the PNG
decoder (chunk walking, row filters, color type expansion) is written in-house.
`miniz_oxide` only does the inflate step, and the PNG encoder uses it for deflate.

Why `miniz_oxide` directly and not `flate2`: `flate2`'s pure-Rust backend is
`miniz_oxide`. Using it directly skips `flate2`'s wrapper along with its optional C
backends (libz and zlib-ng).

Why not the `image` or `png` crates: `image` brings decoders we don't need (JPEG, GIF,
WebP, BMP, and more). The `png` crate is well made, but it pulls in `miniz_oxide`
anyway, plus handling we don't want for small embedded images. Writing the PNG walk
ourselves keeps the public API small and reuses the inflater the WOFF crate already
uses.

## `clap` (in `sigilbuzz-cli`)

- Version: `4.x`, with the `derive` feature.
- License: MIT or Apache-2.0.
- Where: `crates/sigilbuzz-cli/Cargo.toml` only. `sigilbuzz-cli` is a binary crate
  with no library target, so nothing that depends on a sigilbuzz library ever links
  clap.
- Why: the CLI has eight subcommands with ranges, comma-separated lists, and nested
  subcommands for `woff` and `pdf`. A hand-written parser would be about the size of the
  rest of the binary. With clap's `derive` feature, each subcommand's arguments are one
  struct. It is the standard choice for Rust command-line tools.
- Transitive dependencies: `clap_builder`, `clap_derive`, `clap_lex`, `anstream`,
  `anstyle*`, `colorchoice`, `strsim`, `heck`, `is_terminal_polyfill`,
  `once_cell_polyfill`, and `utf8parse`. All are MIT or Apache-2.0, and all are
  maintained by the clap-rs project. None of them reach any sigilbuzz library crate.
- Features: only `derive` is turned on explicitly. The other clap features stay at
  their defaults and add nothing beyond the list above.

## Dev-dependencies

These are used by tests and benchmarks only. They never end up in a published crate's
dependency tree.

- `rustybuzz` and `ttf-parser`: the reference implementations for the shaping and
  outline parity tests.
- `criterion`: benchmarks.
- `cc` and `fd-lock` (in `sigilbuzz-capi`): compile the C test programs and serialize
  the library build across test processes.
