# sigilbuzz-render

Software CPU rasterizer for [sigilbuzz](https://github.com/Oneiriq/sigilbuzz):
turns glyph outlines (TrueType, CFF, CFF2, VARC) into 8-bit alpha pixmaps
and composes COLRv0 layered colour glyphs against a CPAL palette.

## What it does

- Walks `Face::glyph_outline_at_coords` and flattens the resulting
  quadratic / cubic Bezier outline.
- Runs a non-zero-winding trapezoid scanline rasterizer with 256-level
  anti-aliasing. Pure Rust, zero runtime dependencies beyond
  `sigilbuzz` and `sigilbuzz-paint`.
- For colour fonts, composes COLRv0 layers (`gid` × palette entry)
  via `over` blending into a premultiplied RGBA pixmap.
- Variable-font aware: thread normalized axis coords through every
  entry point.

COLRv1, SVG-in-OT and CBDT/sbix bitmaps are out of scope here and land
in 0.15.0+.

## Quick start

```rust,no_run
use sigilbuzz::{Blob, Face};
use sigilbuzz_render::Rasterizer;

let blob = Blob::from_path("./MyFont.ttf").unwrap();
let face = Face::parse_bytes(blob.as_bytes(), 0).unwrap();
let rast = Rasterizer::new();
let pix = rast.rasterize_glyph(&face, 42, 48.0, &[]).unwrap();
// pix.data: Vec<u8> of length pix.width * pix.height (alpha).
```

## Cargo features

| Feature | Default | What it does                            |
|---------|---------|-----------------------------------------|
| `std`   | yes     | Standard-library conveniences.          |

Disable default features for `no_std + alloc` builds.

## License

Apache-2.0. See the workspace root `LICENSE-APACHE`.
