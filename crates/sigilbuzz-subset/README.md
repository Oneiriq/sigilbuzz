# sigilbuzz-subset

Font subsetter and variable-font instancer for
[sigilbuzz](https://github.com/Oneiriq/sigilbuzz). It does the job of `hb-subset`.

## What it does

`subset` takes a `Face` and the glyph IDs you want to keep, and returns a smaller font
with only those glyphs, plus any glyphs they pull in through composites and ligatures.
The result comes back as a `SubsetOutput` with the new font bytes and an
`(old_gid, new_gid)` map. Every table that survives is rewritten so glyph references
point at the new glyph order.

For TrueType fonts:

| Tables | What happens |
|---|---|
| `cmap` | Rebuilt as a single format 4 subtable. |
| `glyf`, `loca` | Rebuilt. Composite components are pulled in automatically. Short or long `loca` is picked to fit. |
| `hmtx`, `hhea`, `maxp`, `head` | Rewritten for the new glyph order. |
| `post` | Written as format 3 (no glyph names). |
| `name`, `OS/2` | Passed through unchanged. |
| `GSUB`, `GPOS`, `GDEF` | Kept as-is when every glyph survives, rewritten otherwise. Off with `retain_layout: false`. |
| `fvar`, `avar`, `gvar`, `HVAR`, `VARC` | Kept. `fvar` and `avar` pass through, the others are rebuilt for the new glyph order. Off with `retain_variations: false`. |
| `kern`, `vhea`, `vmtx`, `VORG`, `COLR`, `CPAL`, `morx`, `kerx` | Dropped by default. With `drop_unhandled: false` they return an error instead. |

CFF and CFF2 fonts are supported too, including CID-keyed CFF. When you subset one of
them down to fewer glyphs, its layout and variation tables are dropped for now.

`instance` handles variable fonts. It bakes a set of axis coordinates into a static
font, or pins some axes and leaves the rest variable.

A malformed piece of a layout table (a GDEF list, a GSUB or GPOS lookup or subtable,
a Device table, an anchor) is left out instead of failing the whole run, the way
HarfBuzz handles it. Every piece left out is reported in `SubsetOutput::warnings` (or
`InstancedOutput::warnings`) with its table, byte offset, and reason.

The output is deterministic: the same face and glyph set always give the same bytes.

## Quick start

```rust,no_run
use sigilbuzz::{Blob, Face};
use sigilbuzz_subset::{subset, SubsetInput};

let blob = Blob::from_path("./MyFont.ttf").unwrap();
let face = Face::parse_bytes(blob.as_bytes(), 0).unwrap();
let input = SubsetInput {
    gids: vec![0, 36, 37, 38], // .notdef, A, B, C
    ..SubsetInput::default()
};
let out = subset(&face, &input).unwrap();
std::fs::write("./MyFont.subset.ttf", &out.bytes).unwrap();
```

## Cargo features

| Feature | Default | What it does |
|---|---|---|
| `std` | yes | Implements `std::error::Error` for `SubsetError`. |

Turn off default features for `no_std` with `alloc`.

## License

Apache-2.0. See the workspace root `LICENSE`.
