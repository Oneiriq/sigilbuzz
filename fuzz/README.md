# Fuzzing

cargo-fuzz targets for every part of sigilbuzz that reads untrusted input. They
look for panics, hangs, and runaway memory use on malformed fonts, images, and
text.

| Target | What it feeds | Entry points |
|---|---|---|
| `face` | font bytes | every `Face` table accessor, outlines, bounds, bitmaps, SVG documents, COLR paint, `OwnedFace`, collections |
| `shape` | font bytes | `shape()` for every script, direction, and a mix of features and variation coordinates |
| `shape_text` | text | `shape()` with real fonts, `BidiInfo`, `Buffer::set_text_bidi`, `BidiMap` |
| `subset` | font bytes | `subset()` and `instance()`, then re-parses the output |
| `woff` | bytes | WOFF1 and WOFF2 unwrap and wrap |
| `render` | font bytes | every `Rasterizer` path: outline, COLRv0, COLRv1, SVG-in-OT, bitmaps |
| `images` | bytes | the PNG, JPEG, and TIFF decoders, the PNG encoder, bilinear rescale |
| `outputs` | font bytes | COLRv1 paint evaluation, SVG output, the three PDF font emitters, the GPU encoder |
| `text` | text | line breaking, word boundaries, line wrapping, hyphenation, pattern parsing |

The font targets read a short control prefix (16 or 24 bytes) before the font data.
It picks glyph ids, variation coordinates, sizes, features, and flags.

## Running

cargo-fuzz needs nightly Rust and a Linux or macOS host.

```sh
cargo install cargo-fuzz
cd fuzz
cargo +nightly fuzz run -O -a face -- -max_len=65536 -rss_limit_mb=2048 -timeout=10
```

Keep `-a`. It turns on overflow checks and debug assertions in the optimized build,
and integer overflow in offset math is one of the main things these targets look
for.

Good starting seeds are the small fonts in `tests/fixtures/` and `tests/fonts/` with
16 zero bytes in front (24 for `subset`).

On Windows, run it in Docker:

```sh
docker run --rm -it -v "%cd%:/src" rust:1-trixie bash
rustup toolchain install nightly --profile minimal
cargo +nightly install cargo-fuzz
cd /src/fuzz && cargo +nightly fuzz run -O -a face
```

## When a target finds a crash

libFuzzer writes the input to `fuzz/artifacts/<target>/`. Shrink it with
`cargo +nightly fuzz tmin -O -a <target> <file>`, fix the bug, and add a regression
test to the crate that owns the code, built from the minimized bytes.
