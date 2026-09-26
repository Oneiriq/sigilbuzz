# Companion crates

The shaping core is the root crate. These crates build on it. Each one is a workspace
member, pulls in `sigilbuzz` with `workspace = true`, and has its own README.

| Crate | What it does |
|---|---|
| [`sigilbuzz-render`](sigilbuzz-render) | CPU rasterizer for outlines, color glyphs, SVG-in-OT, and embedded bitmaps. |
| [`sigilbuzz-paint`](sigilbuzz-paint) | COLRv1 paint evaluator that emits a flat list of draw commands. |
| [`sigilbuzz-gpu`](sigilbuzz-gpu) | Slug outline encoder for GPU rendering. |
| [`sigilbuzz-subset`](sigilbuzz-subset) | Font subsetter and variable-font instancer. |
| [`sigilbuzz-woff`](sigilbuzz-woff) | WOFF1 and WOFF2 wrap and unwrap. |
| [`sigilbuzz-svg`](sigilbuzz-svg) | Glyph outlines and COLRv1 glyphs as SVG. |
| [`sigilbuzz-pdf`](sigilbuzz-pdf) | Type 3, Type 1, and embedded OpenType fonts for PDF. |
| [`sigilbuzz-text-layout`](sigilbuzz-text-layout) | Line breaking, word wrap, and word boundaries. |
| [`sigilbuzz-hyphen`](sigilbuzz-hyphen) | Liang hyphenation. |
| [`sigilbuzz-capi`](sigilbuzz-capi) | HarfBuzz-compatible C API. |
| [`sigilbuzz-cli`](sigilbuzz-cli) | The `sigilbuzz` command-line tool. |

The crates are released together, but each keeps its own version number.
[docs/RELEASING.md](../docs/RELEASING.md) explains how versions move.

## Benchmarks

Criterion benchmarks live in `benches/` at the workspace root and in
`crates/sigilbuzz-gpu/benches/` and `crates/sigilbuzz-paint/benches/`. Criterion is a
dev-dependency everywhere, so it never ships in a release.

Run everything:

```sh
cargo bench --workspace
```

Run one shaping benchmark. Each one shapes the same 200-codepoint text with sigilbuzz
and rustybuzz back to back, so you can compare them in one run:

```sh
cargo bench --bench shape_latin
cargo bench --bench shape_arabic
cargo bench --bench shape_devanagari
cargo bench --bench shape_khmer
cargo bench --bench shape_hebrew
```

Run the encoder and paint benchmarks. rustybuzz has neither feature, so these have no
comparison:

```sh
cargo bench -p sigilbuzz-gpu --bench encode
cargo bench -p sigilbuzz-paint --bench evaluate
```

Each shaping benchmark defines a `sigilbuzz` and a `rustybuzz` function in the same
group, so Criterion's HTML report plots them side by side. The latest numbers are in
[docs/PERFORMANCE.md](../docs/PERFORMANCE.md).
