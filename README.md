# sigilbuzz

A modern, pure-Rust text shaping engine — clean-room, zero-dependency, and built to take advantage of the 2026 HarfBuzz release.

> A **sigil** is an inscribed mark that carries meaning. sigilbuzz turns runs of codepoints into positioned glyphs so the marks you put on screen are actually the marks you meant.

## Status

Pre-alpha, private repository. The plan is to harden sigilbuzz by
dogfooding it as the shaping backend for [oniq](https://github.com/Oneiriq/oniq)
until it reaches a **very capable 0.1.0**, then open it up publicly.
Nothing is published to crates.io and `Cargo.toml` carries
`publish = false` so the lock cannot be bypassed by accident.

## Goals

- **Pure Rust.** No C, no FFI, no transitive C toolchain requirement. Works everywhere stable Rust does, including `wasm32-unknown-unknown`.
- **Zero external dependencies** in the core crate. Every byte of TTF/OTF parsing, OpenType feature evaluation, and glyph positioning is sigilbuzz code. Dependencies are reviewed case-by-case and justified in `docs/deps.md`.
- **`no_std` friendly.** The default build pulls `std` for ergonomics, but the core shaping path compiles and runs under `#![no_std] + alloc`.
- **Deterministic.** Same inputs — font bytes, feature set, direction, script — always produce the same shaped output. Byte-for-byte. This matters for replays, lockstep networking, and golden-file tests.
- **2026 HarfBuzz parity, eventually.** Including the new GPU rasterizer, `hb_gpu_paint_t` color-glyph encoder, and the PDF/SVG paint surfaces. Those will ship as optional modules on top of the shaping core.

## Non-goals

- Being a drop-in C-level replacement for libharfbuzz. The API is HarfBuzz-shaped (Blob → Face → Font → Buffer → shape), but the ergonomics are Rust's, not C's.
- 100 % bug-for-bug parity with old HarfBuzz versions. Where HarfBuzz has historical cruft, sigilbuzz picks the shape that's easier to reason about.

## Why another one

- rustybuzz has not published a release since November 2024.
- harfbuzz-rs has not moved meaningfully since 2021 and is pinned to a HarfBuzz 2.x era.
- The 2026 HarfBuzz release introduces a GPU rasterizer, COLR paint, and PDF/SVG output — none of which are reachable from Rust today.

If the Rust typography stack wants those capabilities, someone has to write them. This is that project.

## Layout

```
src/
├── lib.rs          public surface, re-exports
├── error.rs        Error + Result types
├── blob.rs         owned/borrowed font-data container
├── face.rs         parsed SFNT directory, table access
├── font.rs         Face + size → metrics source
├── buffer.rs       text run → shaped-glyph pipeline
├── shape.rs        the shape() entry point
├── tables/         SFNT / OpenType table parsers (BE, no deps)
├── unicode/        Unicode property data needed for shaping
└── ot/             OpenType feature evaluation (GSUB / GPOS / ...)
```

## License

Apache-2.0. See `LICENSE-APACHE`.
