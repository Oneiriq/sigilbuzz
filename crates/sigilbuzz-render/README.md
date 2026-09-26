# sigilbuzz-render

A software rasterizer for [sigilbuzz](https://github.com/Oneiriq/sigilbuzz). It turns
glyphs into pixels on the CPU, for every kind of glyph a modern font can carry.

## What it does

- Outlines from TrueType, CFF, CFF2, and VARC fonts become 8-bit alpha pixmaps. The
  rasterizer is a non-zero-winding trapezoid scanline algorithm with 256-level
  anti-aliasing.
- COLRv0 and COLRv1 color glyphs render into premultiplied RGBA pixmaps, including
  gradients, compositing, clipping, and variations.
- SVG-in-OT glyphs render too: paths, shapes, strokes and dashes, gradients, `<use>`,
  clip paths, masks, a set of filter primitives, and `<textPath>`.
- Embedded bitmaps from CBDT, sbix, and EBDT tables are decoded and scaled. PNG, JPEG
  (baseline and progressive), and TIFF images are supported.
- Every entry point takes normalized axis coordinates, so variable fonts work
  throughout.

It also exposes the pieces it is built from: `flatten` and `flatten_grouped` turn
curves into line segments (the grouped form is what MSDF generators need), and
`encode_png` / `decode_png` handle PNG.

## Quick start

```rust,no_run
use sigilbuzz::{Blob, Face};
use sigilbuzz_render::Rasterizer;

let blob = Blob::from_path("./MyFont.ttf").unwrap();
let face = Face::parse_bytes(blob.as_bytes(), 0).unwrap();
let rast = Rasterizer::new();
let pix = rast.rasterize_glyph(&face, 42, 48.0, &[]).unwrap();
// pix.data is a Vec<u8> of pix.width * pix.height alpha values.
```

## Dependencies

`sigilbuzz`, `sigilbuzz-paint` for COLRv1, and `miniz_oxide` for PNG compression.
`docs/deps.md` in the workspace root explains the last one.

## Cargo features

| Feature | Default | What it does |
|---|---|---|
| `std` | yes | Implements `std::error::Error` for `RenderError`. |

The crate's own code needs only `alloc`, but it depends on `sigilbuzz` with that crate's
default `std` feature. Turning off default features here therefore does not make it
`no_std`, and it still needs a target with `std`.

## License

Apache-2.0. See the workspace root `LICENSE`.
