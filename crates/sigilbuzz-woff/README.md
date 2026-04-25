# sigilbuzz-woff

WOFF1 and WOFF2 wrapping / unwrapping for [sigilbuzz](https://github.com/Oneiriq/sigilbuzz).

Browsers serve fonts as WOFF1 (legacy) and WOFF2 (current). The
sigilbuzz core ingests SFNT (TTF / OTF). This crate bridges the gap.

## What it does

- `unwrap_woff1(woff)` -> SFNT bytes. Header + per-table directory,
  zlib-decompressed table bodies. The current implementation handles
  the uncompressed pass-through path; a follow-up will plug in zlib
  proper (most modern WOFF1 producers ship Brotli via WOFF2 anyway,
  leaving compressed WOFF1 in legacy territory).
- `wrap_woff1(sfnt)` -> WOFF1 bytes, uncompressed pass-through.
- `unwrap_woff2(woff2)` -> SFNT bytes. Parses the WOFF2 header and
  table directory (with the 5-bit known-tag encoding and the
  Base-128 length fields), Brotli-decompresses the payload, and
  applies the inverse `glyf` / `loca` transform that WOFF2
  mandates.

`wrap_woff2` (forward transform + Brotli encoding) is intentionally
out of scope for this crate's first release — it's a meaningfully
larger project and is targeted for 0.7.0.

## Dependencies

`sigilbuzz` (workspace) and `brotli-decompressor` (only when the
default `woff2` feature is on). Disabling `woff2` makes the crate
fall back to WOFF1 only and pulls in zero runtime deps beyond the
shaper. See `docs/deps.md` for the rationale.
