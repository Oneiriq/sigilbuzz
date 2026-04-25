# External dependencies

The sigilbuzz core (`crates/sigilbuzz`) ships with **zero runtime
dependencies**. Every byte of parsing, shaping, and outline math is
in-house. Companion crates may pull in narrow, justified runtime deps
provided each one earns its place here.

The bar:

1. The dep replaces a body of work that is impractical to write
   in-house at this stage of the project.
2. The dep has a permissive OSI license (Apache-2.0 / MIT / BSD).
3. The dep is itself near-leaf: no large transitive closures.
4. The dep is gated behind a cargo feature so consumers who don't
   need it pay nothing.

## `brotli` (in `sigilbuzz-woff`)

- **Version:** `8.x`.
- **License:** BSD-3-Clause / MIT (dual).
- **Where:** `crates/sigilbuzz-woff/Cargo.toml`, gated behind the
  `woff2` cargo feature (which is on by default but trivially
  disablable for WOFF1-only consumers).
- **Why:** WOFF2 mandates Brotli for the single payload block that
  holds every table back-to-back — decompression on the unwrap path
  (`unwrap_woff2`) and compression on the wrap path (`wrap_woff2`).
  Brotli is its own RFC (RFC 7932); a from-scratch encoder dwarfs
  the rest of the WOFF2 wrapper. The `brotli` crate from the
  Dropbox / Brotli ecosystem ships *both* directions in one crate
  and is the canonical Rust port — `brotli-decompressor` (which
  we used through 0.6.0 for unwrap-only) is published from the
  same workspace by the same author.
- **Transitive footprint:** `alloc-no-stdlib` and `alloc-stdlib` —
  both single-purpose helper crates by the same author, both
  no_std-capable, both BSD-3 / MIT. The `brotli` crate adds the
  encoder modules but keeps the same transitive set as
  `brotli-decompressor`.
- **Migration note (0.6.0 → 0.7.0):** the dep was previously
  `brotli-decompressor = "5"`. 0.7.0 substitutes `brotli = "8"`
  because the encoder for `wrap_woff2` lives in the umbrella
  crate. The decompressor API (`brotli::BrotliDecompress`) is
  re-exported and behaves identically to the standalone crate; the
  switch is API-compatible for `unwrap_woff2` callers.

## `clap` (in `sigilbuzz-cli`)

- **Version:** `4.x`, with the `derive` feature.
- **License:** MIT / Apache-2.0 (dual).
- **Where:** `crates/sigilbuzz-cli/Cargo.toml` only. Never appears in
  any library crate's dep closure — `sigilbuzz-cli` is a binary-only
  crate (`[[bin]]` with no `[lib]`) so consumers of the library
  surface (`sigilbuzz`, `sigilbuzz-subset`, …) never link against it.
- **Why:** the CLI exposes eight subcommands with rich argument
  shapes (range parsing, comma lists, sub-subcommands for `woff` and
  `pdf`). A from-scratch parser would be roughly the size of the
  rest of the binary, and clap's `derive` feature collapses the
  argument schema to one struct per subcommand — the same pattern
  every Rust CLI in the ecosystem uses (`cargo`, `rustup`,
  `ripgrep`, …). It is the canonical choice for argument parsing in
  Rust and there is no in-house alternative that would carry its
  weight.
- **Transitive footprint:** `clap_builder`, `clap_derive`,
  `clap_lex`, `anstream`, `anstyle*`, `colorchoice`, `strsim`,
  `heck`, `is_terminal_polyfill`, `once_cell_polyfill`,
  `utf8parse`. All MIT / Apache-2.0 dual-licensed, all maintained
  by the clap-rs org. None of them propagate into any sigilbuzz
  *library* crate; they are entirely scoped to the binary.
- **Cargo feature gating:** the `derive` feature is the one we
  rely on (every subcommand is a `#[derive(Args)]` struct). Other
  clap features (`color`, `cargo`, `env`, `unicode`) stay at their
  defaults — none of them push extra deps into the closure beyond
  the listed transitive set.
