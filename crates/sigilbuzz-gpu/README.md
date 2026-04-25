# sigilbuzz-gpu

GPU outline encoder for [sigilbuzz](https://github.com/Oneiriq/sigilbuzz),
following Eric Lengyel's *Slug* algorithm (Loop-Blinn family).

## What it does

Consumes per-glyph outlines from `sigilbuzz::Face::glyph_outline` and
packs them into a flat, GPU-friendly representation: a band table
plus a quadratic-segment buffer that a fragment shader can walk to
compute coverage. Cubics are flattened to quadratics; bands tile the
glyph bounding box along the y-axis. The shader side is deliberately
out of scope — this crate only produces the encoded data so the
consumer can upload it as SSBOs or texture buffers.

## Quick start

```rust,no_run
use sigilbuzz::{Blob, Face};
use sigilbuzz_gpu::{encode_glyph, SlugOptions};

let blob = Blob::from_path("./MyFont.ttf").unwrap();
let face = Face::parse_bytes(blob.as_bytes(), 0).unwrap();
let glyph = encode_glyph(&face, 42, &SlugOptions::default()).unwrap();
// glyph.bands and glyph.segments → upload to GPU
```

## Cargo features

| Feature | Default | What it does                            |
|---------|---------|-----------------------------------------|
| `std`   | yes     | `Vec`-backed buffers and IO helpers.    |

Disable default features for `no_std + alloc` builds.

## License

Apache-2.0. See the workspace root `LICENSE-APACHE`.
