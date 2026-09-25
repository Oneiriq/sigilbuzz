# sigilbuzz-gpu

GPU outline encoder for [sigilbuzz](https://github.com/Oneiriq/sigilbuzz), based on
Eric Lengyel's Slug algorithm.

## What it does

It takes a glyph outline from `sigilbuzz::Face::glyph_outline` and packs it into a flat
layout a GPU can read: a band table and a buffer of quadratic segments. A fragment
shader walks the segments in a pixel's band to compute coverage. Cubic curves are
converted to quadratics, and the bands tile the glyph's bounding box along the y axis.

The crate only produces the data. Uploading it (as storage buffers or texture buffers)
and writing the shader are up to you.

## Quick start

```rust,no_run
use sigilbuzz::{Blob, Face};
use sigilbuzz_gpu::{encode_glyph, SlugOptions};

let blob = Blob::from_path("./MyFont.ttf").unwrap();
let face = Face::parse_bytes(blob.as_bytes(), 0).unwrap();
let glyph = encode_glyph(&face, 42, &SlugOptions::default()).unwrap();
// Upload glyph.bands and glyph.segments to the GPU.
```

## Cargo features

| Feature | Default | What it does |
|---|---|---|
| `std` | yes | `Vec`-backed buffers and IO helpers. |

Turn off default features for `no_std` with `alloc`.

## License

Apache-2.0. See the workspace root `LICENSE`.
