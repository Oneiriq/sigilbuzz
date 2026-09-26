# sigilbuzz-svg

SVG output for [sigilbuzz](https://github.com/Oneiriq/sigilbuzz) glyph outlines and
COLRv1 color glyphs.

## What it does

It turns sigilbuzz's `PathOp` outline stream into a complete `<svg>` element you can
drop into a document, a preview tool, or a font debugging page. With the `color`
feature it does the same for COLRv1 glyphs, using the `DrawCmd` list from
`sigilbuzz-paint`.

The output is plain text written with `core::fmt`, using one fixed number precision, so
the same input always gives the same bytes. SVG 1.1 has no sweep (conic) gradient, so a
COLRv1 sweep gradient becomes a `<linearGradient>` across the gradient's bounding box,
with a comment in the output noting the substitution.

Color glyph layers drawn in the text color (COLR palette entry `0xFFFF`) are filled with
`currentColor`, so an inline SVG glyph takes the CSS `color` of the text around it.

## Quick start

```rust,no_run
use sigilbuzz::Face;
use sigilbuzz_svg::glyph_to_svg;

# fn demo(face: &Face<'_>) {
if let Some(svg) = glyph_to_svg(face, 42) {
    // svg is a complete `<svg ...>...</svg>` document.
    println!("{svg}");
}
# }
```

For color glyphs, use `glyph_to_svg_color`. For variable fonts,
`glyph_to_svg_at_coords` takes normalized axis coordinates.

## Cargo features

| Feature | Default | What it does |
|---|---|---|
| `std` | yes | `String`-backed output. |
| `color` | yes | Pulls in `sigilbuzz-paint` for COLRv1 output. Turn it off for outlines only. |

Turn off default features to build without the paint dependency.

## License

Apache-2.0. See the workspace root `LICENSE`.
