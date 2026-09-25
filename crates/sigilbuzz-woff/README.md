# sigilbuzz-woff

WOFF1 and WOFF2 wrap and unwrap for [sigilbuzz](https://github.com/Oneiriq/sigilbuzz).

Browsers load web fonts as WOFF1 (the older format) or WOFF2 (the current one). The
sigilbuzz core reads plain SFNT fonts (TTF and OTF). This crate converts between them.

## What it does

- `unwrap_woff1(woff)` returns SFNT bytes. It reads the header and table directory and
  inflates zlib-compressed tables when the `woff1-deflate` feature is on (the
  default). Without the feature, compressed tables return `Unsupported`.
- `wrap_woff1(sfnt)` and `wrap_woff1_with_options(sfnt, opts)` return WOFF1 bytes.
  With `woff1-deflate`, each table is compressed when that makes it smaller and stored
  as-is otherwise. `WrapWoff1Options { deflate_quality }` sets the zlib level (`0..=9`,
  default `6`).
- `unwrap_woff2(woff2)` returns SFNT bytes. It reads the WOFF2 header and table
  directory, decompresses the Brotli payload, and reverses the `glyf` / `loca`
  transform that WOFF2 requires.
- `wrap_woff2(sfnt)` and `wrap_woff2_with_options(sfnt, opts)` return WOFF2 bytes,
  applying the `glyf` / `loca` transform and Brotli compression.

## Quick start

```rust,no_run
use sigilbuzz::Face;
use sigilbuzz_woff::unwrap_woff2;

let woff2 = std::fs::read("font.woff2").unwrap();
let sfnt = unwrap_woff2(&woff2).unwrap();
let face = Face::parse_bytes(&sfnt, 0).unwrap();
```

## Dependencies

`sigilbuzz`, plus `brotli` when the `woff2` feature is on and `miniz_oxide` when
`woff1-deflate` is on. Both features are on by default. Turn both off and the crate
has no dependencies beyond the core. `docs/deps.md` in the workspace root explains
each one.

## License

Apache-2.0. See the workspace root `LICENSE`.
