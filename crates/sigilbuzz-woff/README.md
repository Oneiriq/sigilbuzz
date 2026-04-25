# sigilbuzz-woff

WOFF1 and WOFF2 wrapping / unwrapping for [sigilbuzz](https://github.com/Oneiriq/sigilbuzz).

Browsers serve fonts as WOFF1 (legacy) and WOFF2 (current). The
sigilbuzz core ingests SFNT (TTF / OTF). This crate bridges the gap.

## What it does

- `unwrap_woff1(woff)` -> SFNT bytes. Header + per-table directory,
  zlib-decompressed table bodies (when the default `woff1-deflate`
  feature is on; otherwise compressed tables are rejected with
  `Unsupported`).
- `wrap_woff1(sfnt)` / `wrap_woff1_with_options(sfnt, opts)` -> WOFF1
  bytes. With `woff1-deflate` enabled, each table is emitted
  zlib-compressed when deflate saves space and uncompressed
  otherwise. `WrapWoff1Options { deflate_quality }` exposes the
  zlib level (`0..=9`, default `6`).
- `unwrap_woff2(woff2)` -> SFNT bytes. Parses the WOFF2 header and
  table directory (with the 5-bit known-tag encoding and the
  Base-128 length fields), Brotli-decompresses the payload, and
  applies the inverse `glyf` / `loca` transform that WOFF2
  mandates.
- `wrap_woff2(sfnt)` / `wrap_woff2_with_options(sfnt, opts)` ->
  WOFF2 bytes (forward `glyf` / `loca` transform + Brotli).

## Dependencies

`sigilbuzz` (workspace), `brotli` (only when the default `woff2`
feature is on), and `miniz_oxide` (only when the default
`woff1-deflate` feature is on). Disabling both feature flags drops
every runtime dep beyond the shaper itself. See `docs/deps.md` for
the rationale on each.
