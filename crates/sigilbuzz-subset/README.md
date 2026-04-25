# sigilbuzz-subset

Font subsetter for [sigilbuzz](https://github.com/Oneiriq/sigilbuzz) —
takes a `Face` plus a glyph-id list and produces a smaller font
containing only those glyphs (plus any glyphs they reference
transitively through composites).

## What it does

Given a sigilbuzz `Face` and a `SubsetInput`, produces a `SubsetOutput`
holding the new font bytes and an `(old_gid, new_gid)` remap. Tables
that survive are rewritten so old glyph-id references resolve against
the new, compacted glyph order.

| Table        | Behaviour                                       |
|--------------|-------------------------------------------------|
| `cmap`       | Rebuilt as a single Windows BMP format-4 subtable. |
| `glyf` + `loca` | Rebuilt; composite components are auto-pulled by the closure walker. Loca format (short / long) auto-picked. |
| `hmtx` + `hhea` | Rewritten with the new gid order; `numberOfHMetrics` patched. |
| `maxp`, `head`  | `numGlyphs` and `indexToLocFormat` patched in place. |
| `post`       | Forced to format 3 (no glyph names).            |
| `name`, `OS/2` | Passed through verbatim.                      |

`GSUB`, `GPOS`, `GDEF`, `kern`, `vhea`, `vmtx`, `VORG`, `HVAR`,
`gvar`, `COLR`, `CPAL`, `morx`, `kerx`, `fvar`, and `avar` are
**dropped** by default. Set `drop_unhandled = false` to error
instead. CFF / CFF2 outlines are not yet supported.

Output is byte-deterministic: the same face + gid set yields a
byte-identical result.

## Quick start

```rust,no_run
use sigilbuzz::{Blob, Face};
use sigilbuzz_subset::{subset, SubsetInput};

let blob = Blob::from_path("./MyFont.ttf").unwrap();
let face = Face::parse_bytes(blob.as_bytes(), 0).unwrap();
let input = SubsetInput {
    gids: vec![0, 36, 37, 38], // .notdef + 'A' + 'B' + 'C'
    retain_hints: false,
    drop_unhandled: true,
};
let out = subset(&face, &input).unwrap();
std::fs::write("./MyFont.subset.ttf", &out.bytes).unwrap();
```

## Cargo features

| Feature | Default | What it does                            |
|---------|---------|-----------------------------------------|
| `std`   | yes     | `std::error::Error` impl on `SubsetError`. |

Disable default features for `no_std + alloc` builds.

## License

Apache-2.0. See the workspace root `LICENSE-APACHE`.
