# sigilbuzz-svg

SVG serialiser for [sigilbuzz](https://github.com/Oneiriq/sigilbuzz)
glyph outlines and COLRv1 color glyphs.

## What it does

Turns sigilbuzz's flat `PathOp` outline stream — and, with the `color`
feature, the `sigilbuzz_paint::DrawCmd` stream — into a self-contained
`<svg>` element ready to drop into a document, a preview tool, or a
font-debug page. Output is plain text emitted via `core::fmt` with a
single deterministic-precision policy: same input always produces the
same byte sequence. SVG 1.1 has no conic gradient, so COLRv1 sweep
gradients degrade to a `<linearGradient>` across the gradient's
bounding box with an explanatory comment.

## Quick start

```rust,no_run
use sigilbuzz::Face;
use sigilbuzz_svg::glyph_to_svg;

# fn demo(face: &Face<'_>) {
if let Some(svg) = glyph_to_svg(face, 42) {
    // svg is a complete `<svg ...>...</svg>` document
    println!("{svg}");
}
# }
```

## Cargo features

| Feature | Default | What it does                                                     |
|---------|---------|------------------------------------------------------------------|
| `std`   | yes     | `String`-backed serialisation.                                   |
| `color` | yes     | Pulls in `sigilbuzz-paint` for COLRv1 emission. Drop for outline-only output. |

Disable default features for outline-only builds without the paint
dependency.

## License

Apache-2.0. See the workspace root `LICENSE-APACHE`.
