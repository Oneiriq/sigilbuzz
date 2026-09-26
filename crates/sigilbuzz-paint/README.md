# sigilbuzz-paint

COLRv1 paint evaluator for [sigilbuzz](https://github.com/Oneiriq/sigilbuzz).

## What it does

sigilbuzz parses a COLRv1 paint tree into a borrowed enum
(`sigilbuzz::tables::colr::ColrPaint`). This crate walks that tree and emits a flat
list of `DrawCmd`s that a renderer can turn into pixels. Along the way it:

- Combines nested transforms into one 2x3 matrix per leaf. A transform below a
  `PaintGlyph` moves only the paint: the fill keeps the glyph's outline in place and
  carries the gradient geometry into the outline's space.
- Resolves `ColorLine` stops against the selected CPAL palette, including per-stop alpha.
- Applies variation deltas from the COLR table's own item variation store, through its
  DeltaSetIndexMap when it has one, as HarfBuzz does.
- Keeps the foreground color apart. Solid fills and gradient stops that use COLR palette
  entry `0xFFFF` (the text color) carry `is_foreground == true`.
- Wraps each `PaintComposite` in an isolating `PushLayer` / `PopLayer` pair holding the
  backdrop and a nested pair, with the composite mode, holding the source.
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
palette, and the foreground color used for palette entry `0xFFFF` (opaque black unless
you set one):

```rust,no_run
use sigilbuzz::Face;
use sigilbuzz_paint::{evaluate_with, Color, EvalOptions};

# fn demo(face: &Face<'_>, coords: &[f32]) {
let options = EvalOptions::new()
    .with_coords(coords)
    .with_palette_index(1)
    .with_foreground(Color::new(1.0, 1.0, 1.0, 1.0));
let cmds = evaluate_with(face, 42, &options);
# let _ = cmds;
# }
```

Palette lookups that fail follow HarfBuzz: when the font lacks the requested palette,
lacks the palette entry, or has no `CPAL` table, the fill uses the foreground color. Such
fills keep `is_foreground == false`, since they did not ask for the text color.

Sweep gradient angles come out in radians. COLRv1 stores them with a half-turn bias, so
a stored angle `a` is `(a + 1) * pi` radians, the value HarfBuzz reports too.

## Cargo features

| Feature | Default | What it does |
|---|---|---|
| `std` | yes | Builds the crate against `std`. It adds no API. |

The crate's own code needs only `alloc`, but it depends on `sigilbuzz` with that crate's
default `std` feature. Turning off default features here therefore does not make it
`no_std`, and it still needs a target with `std`.

## License

Apache-2.0. See the workspace root `LICENSE`.
