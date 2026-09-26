# sigilbuzz-paint

COLRv1 paint evaluator for [sigilbuzz](https://github.com/Oneiriq/sigilbuzz).

## What it does

sigilbuzz parses a COLRv1 paint tree into a borrowed enum
(`sigilbuzz::tables::colr::ColrPaint`). This crate walks that tree and emits a flat
list of `DrawCmd`s that a renderer can turn into pixels. Along the way it:

- Combines nested transforms into one 2x3 matrix per leaf.
- Resolves `ColorLine` stops against the selected CPAL palette, including per-stop alpha.
- Keeps the foreground color apart. Solid fills and gradient stops that use COLR palette
  entry `0xFFFF` (the text color) carry `is_foreground == true`.
- Wraps `PaintComposite` children in `PushLayer` / `PopLayer` so the renderer can
  blend them.
- Follows `ColrGlyph` references and stops on cycles.

Malformed fonts never cause a panic. A bad offset or an unknown paint format ends the
command list early instead of producing a half-built paint.

## Quick start

```rust,no_run
use sigilbuzz::Face;
use sigilbuzz_paint::{evaluate, DrawCmd};

# fn demo(face: &Face<'_>) {
let cmds: Vec<DrawCmd> = evaluate(face, 42);
for cmd in &cmds {
    match cmd {
        DrawCmd::FillGlyph { gid, transform, paint } => { /* rasterize */ }
        DrawCmd::PushLayer { composite_mode } => {}
        DrawCmd::PopLayer => {}
    }
}
# }
```

For variable fonts, `evaluate_at_coords` takes normalized axis coordinates.

`evaluate_with` takes an `EvalOptions` that picks the variation coordinates, the CPAL
palette (palette 0 when the font has no such palette), and the foreground color used
for palette entry `0xFFFF` (opaque white unless you set one):

```rust,no_run
use sigilbuzz::Face;
use sigilbuzz_paint::{evaluate_with, Color, EvalOptions};

# fn demo(face: &Face<'_>, coords: &[f32]) {
let options = EvalOptions::new()
    .with_coords(coords)
    .with_palette_index(1)
    .with_foreground(Color::new(0.0, 0.0, 0.0, 1.0));
let cmds = evaluate_with(face, 42, &options);
# let _ = cmds;
# }
```

Sweep gradient angles come out in radians. COLRv1 stores them with a half-turn bias, so
a stored angle `a` is `(a + 1) * pi` radians, the value HarfBuzz reports too.

## Cargo features

| Feature | Default | What it does |
|---|---|---|
| `std` | yes | `Vec`-backed `DrawCmd` lists. |

Turn off default features for `no_std` with `alloc`.

## License

Apache-2.0. See the workspace root `LICENSE`.
