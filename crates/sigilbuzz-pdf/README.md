# sigilbuzz-pdf

PDF font output for [sigilbuzz](https://github.com/Oneiriq/sigilbuzz) glyph outlines:
Type 3, Type 1, and embedded OpenType or TrueType.

## What it does

It takes a sigilbuzz `Face` and a list of glyph IDs and builds one of three kinds of
PDF font:

- `emit_type3_font` returns a `Type3Font` with `FontMatrix`, `FontBBox`, `Encoding`,
  `Widths`, and one `CharProc` content stream per glyph. Each `PathOp` becomes a PDF
  drawing operator.
- `emit_type1_font` returns a `Type1Font` with the font dictionary, private dictionary,
  and `/CharStrings` as byte buffers. The charstrings are left unencrypted. The private
  dictionary declares `/lenIV -1`, which Adobe Reader and other current readers accept.
- `emit_otf_embedded_font` returns an `OtfEmbeddedFont`: a Type 0 font dictionary, a
  font descriptor, the unmodified font program, an Identity-H map, and per-glyph widths
  in 1000-unit space. To embed a smaller font, subset it first with
  `sigilbuzz-subset`.

The crate stops at these data structures. It has no `lopdf` or `printpdf` dependency,
so writing them into a PDF document is up to you. The output is deterministic: the
same inputs always give the same bytes.

## Quick start

```rust,no_run
use sigilbuzz::{Blob, Face};
use sigilbuzz_pdf::{emit_otf_embedded_font, emit_type1_font, emit_type3_font};

let blob = Blob::from_path("./MyFont.ttf").unwrap();
let face = Face::parse_bytes(blob.as_bytes(), 0).unwrap();

// Pick one:
let t3 = emit_type3_font(&face, &[65, 66, 67]); // drawn glyph procedures
let t1 = emit_type1_font(&face, &[65, 66, 67]).unwrap(); // PostScript charstrings
let otf = emit_otf_embedded_font(&face, blob.as_bytes(), &[65, 66, 67]); // /FontFile2
```

## Cargo features

| Feature | Default | What it does |
|---|---|---|
| `std` | yes | `Vec`-backed buffers and `String` output. |

Turn off default features for `no_std` with `alloc`.

## License

Apache-2.0. See the workspace root `LICENSE`.
