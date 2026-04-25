# sigilbuzz-pdf

PDF font emitters for [sigilbuzz](https://github.com/Oneiriq/sigilbuzz)
glyph outlines — Type 3, Type 1, and OTF/TrueType embedded.

## What it does

Translates a sigilbuzz `Face` plus a list of glyph IDs into one of
three PDF font flavours:

- `emit_type3_font` → a `Type3Font` data structure (`FontMatrix`,
  `FontBBox`, `Encoding`, `Widths`, one `CharProc` content stream
  per glyph). Each `PathOp` becomes a PDF drawing operator.
- `emit_type1_font` → a `Type1Font` with separate font dict, private
  dict, and `/CharStrings` byte buffers. Charstrings are *cleartext*
  (eexec encryption is intentionally skipped — the private dict
  declares `/lenIV -1`, which Adobe Reader and modern consumers
  honour).
- `emit_otf_embedded_font` → an `OtfEmbeddedFont` wrapper containing
  a Type 0 font dict, font descriptor, the unmodified font program,
  a 256-CID Identity-H map, and per-glyph widths in 1000-unit space.
  Subsetting is the parallel `sigilbuzz-subset` crate's job.

The crate stops at the data structure — it has no `lopdf` /
`printpdf` dependency, so the consumer is in charge of serialising
into a real PDF document. Output is deterministic: same inputs yield
byte-identical output.

## Quick start

```rust,no_run
use sigilbuzz::{Blob, Face};
use sigilbuzz_pdf::{emit_type3_font, emit_type1_font, emit_otf_embedded_font};

let blob = Blob::from_path("./MyFont.ttf").unwrap();
let face = Face::parse_bytes(blob.as_bytes(), 0).unwrap();

// Pick a flavour:
let t3 = emit_type3_font(&face, &[65, 66, 67]); // user-defined CharProcs
let t1 = emit_type1_font(&face, &[65, 66, 67]).unwrap(); // PostScript charstrings
let otf = emit_otf_embedded_font(&face, blob.as_bytes(), &[65, 66, 67]); // /FontFile2
```

## Cargo features

| Feature | Default | What it does                            |
|---------|---------|-----------------------------------------|
| `std`   | yes     | `Vec`-backed buffers and `String` ops.  |

Disable default features for `no_std + alloc` builds.

## License

Apache-2.0. See the workspace root `LICENSE-APACHE`.
