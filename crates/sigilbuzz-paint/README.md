# sigilbuzz-paint

COLRv1 paint evaluator for [sigilbuzz](https://github.com/Oneiriq/sigilbuzz).

## What it does

sigilbuzz parses the COLRv1 paint tree as a borrowed enum
(`sigilbuzz::tables::colr::ColrPaint`); this companion crate walks
that tree and emits a flat `DrawCmd` stream a renderer can turn into
pixels. The walker composes nested affine transforms into a single
2x3 matrix per leaf, resolves `ColorLine` stops against the active
CPAL palette (with per-stop alpha), brackets `PaintComposite` children
with `PushLayer` / `PopLayer` for blend-mode-aware compositing, and
detects cycles through `ColrGlyph` references with a visited-set.
Malformed input never panics — bad sub-offsets and unknown formats
truncate the stream rather than producing a partial paint.

## Quick start

```rust,no_run
use sigilbuzz::Face;
use sigilbuzz_paint::{evaluate, DrawCmd};

# fn demo(face: &Face<'_>) {
let cmds: Vec<DrawCmd> = evaluate(face, 42);
for cmd in &cmds {
    match cmd {
        DrawCmd::FillGlyph { gid, transform, paint } => { /* rasterise */ }
        DrawCmd::PushLayer { composite_mode } => {}
        DrawCmd::PopLayer => {}
    }
}
# }
```

## Cargo features

| Feature | Default | What it does                            |
|---------|---------|-----------------------------------------|
| `std`   | yes     | `Vec`-backed `DrawCmd` streams.         |

Disable default features for `no_std + alloc` builds.

## License

Apache-2.0. See the workspace root `LICENSE-APACHE`.
