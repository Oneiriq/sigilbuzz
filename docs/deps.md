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

## `brotli-decompressor` (in `sigilbuzz-woff`)

- **Version:** `5.x`.
- **License:** BSD-3-Clause / MIT (dual).
- **Where:** `crates/sigilbuzz-woff/Cargo.toml`, gated behind the
  `woff2` cargo feature (which is on by default but trivially
  disablable for WOFF1-only consumers).
- **Why:** WOFF2 mandates Brotli decompression for the single
  payload block that holds every table back-to-back. Brotli is its
  own RFC (RFC 7932) — implementing it from scratch is a separate
  project measured in thousands of lines and would dwarf the rest of
  the WOFF2 unwrapper. `brotli-decompressor` is the canonical Rust
  port maintained by the Dropbox / Brotli ecosystem and is what
  every other Rust WOFF2 implementation rolls under.
- **Transitive footprint:** `alloc-no-stdlib` and `alloc-stdlib` —
  both single-purpose helper crates by the same author, both
  no_std-capable, both BSD-3 / MIT.
- **Forward path:** if Brotli encoding is ever needed for
  `wrap_woff2`, the companion `brotli` crate (same author) covers
  it. Both decoder and encoder land in `sigilbuzz-woff` only — the
  shaping core stays dep-free.
