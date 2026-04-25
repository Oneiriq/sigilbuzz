# Companion crates

- `sigilbuzz-gpu` — Slug-algorithm outline encoder for GPU rasterisation. Depends on the outline-extraction APIs exposed by `sigilbuzz`.
- `sigilbuzz-paint` — COLRv1 paint evaluation helpers (transform composition, ColorLine sampling, Composite blending). Depends on `sigilbuzz` for paint-tree traversal.

Each crate is a workspace member and pulls `sigilbuzz` via `workspace = true` in its dependency list. Release cadence is independent; each companion carries its own semver.

# Benchmarks

Criterion benches live at the workspace root (`benches/`) and inside
each companion crate (`crates/sigilbuzz-{gpu,paint}/benches/`). They
run by default under `cargo bench` and are dev-only — Criterion is a
`[dev-dependencies]` entry on every crate that ships benches and
never enters the runtime closure.

Run everything:

```sh
cargo bench --workspace
```

Run a single shape bench (each one shapes a 200-codepoint corpus
through both sigilbuzz and rustybuzz back-to-back so you can compare
ratios in one shot):

```sh
cargo bench --bench shape_latin
cargo bench --bench shape_arabic
cargo bench --bench shape_devanagari
cargo bench --bench shape_khmer
cargo bench --bench shape_hebrew
```

Run the encoder / paint benches (no rustybuzz baseline — rustybuzz
does not ship either of these subsystems):

```sh
cargo bench -p sigilbuzz-gpu --bench encode
cargo bench -p sigilbuzz-paint --bench evaluate
```

Comparing against rustybuzz: the shape benches define two functions
per group (`sigilbuzz` and `rustybuzz`) so Criterion's HTML report
plots them side by side. The latest captured numbers, ratios, and
follow-up issues for any regression worse than 2x rustybuzz live in
[`docs/PERFORMANCE.md`](../docs/PERFORMANCE.md).
