# sigilbuzz-pdf

PDF Type 3 font emitter for [sigilbuzz](https://github.com/Oneiriq/sigilbuzz)
glyph outlines.

## What it does

Translates a sigilbuzz `Face` plus a list of glyph IDs into a
`Type3Font` data structure: a `FontMatrix`, `FontBBox`, `Encoding`,
`Widths` array, and one `CharProc` content stream per glyph. Each
`PathOp` from `Face::glyph_outline` becomes a PDF drawing operator
written into the `CharProc`. The crate stops at the data structure —
it has no `lopdf` / `printpdf` dependency, so the consumer is in
charge of serialising the dict into a real PDF document. Output is
deterministic: same `Face` + same gid list yields a byte-identical
`Type3Font`.

## Quick start

```rust,no_run
use sigilbuzz::{Blob, Face};
use sigilbuzz_pdf::emit_type3_font;

let blob = Blob::from_path("./MyFont.ttf").unwrap();
let face = Face::parse_bytes(blob.as_bytes(), 0).unwrap();
let font = emit_type3_font(&face, &[65, 66, 67]); // 'A','B','C'
// font.char_procs / font.widths / font.encoding → into your PDF writer
```

## Cargo features

| Feature | Default | What it does                            |
|---------|---------|-----------------------------------------|
| `std`   | yes     | `Vec`-backed buffers and `String` ops.  |

Disable default features for `no_std + alloc` builds.

## License

Apache-2.0. See the workspace root `LICENSE-APACHE`.
