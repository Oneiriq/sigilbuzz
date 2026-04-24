# Companion crates

- `sigilbuzz-gpu` — Slug-algorithm outline encoder for GPU rasterisation. Depends on the outline-extraction APIs exposed by `sigilbuzz`.
- `sigilbuzz-paint` — COLRv1 paint evaluation helpers (transform composition, ColorLine sampling, Composite blending). Depends on `sigilbuzz` for paint-tree traversal.

Each crate is a workspace member and pulls `sigilbuzz` via `workspace = true` in its dependency list. Release cadence is independent; each companion carries its own semver.
