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
| `vmtx`, `vhea`, `VORG` | Rewritten for the new glyph order, so vertical text keeps its advances and origins. A malformed one is left out with a warning. |
| `post` | Written as format 3 (no glyph names). |
| `name`, `OS/2`, `STAT` | Passed through unchanged. |
| `BASE` | Kept, with the reference glyph of each format 2 coordinate renumbered. A coordinate whose reference glyph is dropped becomes format 1 with the same value. A malformed `BASE` is left out with a warning. Off with `retain_layout: false`, like the other layout tables. |
| `GSUB`, `GPOS`, `GDEF` | Kept as-is when every glyph survives, rewritten otherwise, FeatureVariations included. Off with `retain_layout: false`. |
| `fvar`, `avar`, `gvar`, `HVAR`, `VVAR`, `VARC` | Kept. `fvar` and `avar` pass through, the others are rebuilt for the new glyph order. Off with `retain_variations: false`, which drops `MVAR` too. |
| `kern`, `kerx`, `morx` and the other AAT tables, `COLR`, `CPAL`, `MVAR`, `cvar`, `CBDT`, `CBLC`, `EBDT`, `EBLC`, `EBSC`, `sbix`, `SVG `, `MATH`, `JSTF`, `gasp`, `hdmx`, `LTSH`, `VDMX`, `DSIG`, and any table not named above | Dropped by default. With `drop_unhandled: false` they return an error instead. |

CFF and CFF2 fonts are supported too, including CID-keyed CFF. When glyphs are dropped,
the `CFF ` or `CFF2` table is rebuilt around the kept ones, and every other table follows
the same rules as above. A `CFF2` table keeps its own variation data. A kept `CFF ` glyph
drawn by `seac` (an accented character) keeps its base and accent glyphs, as `hb-subset`
does.

When every glyph is kept, a CFF or CFF2 font passes through instead, and only the table
directory is rebuilt. Every table is copied unchanged except `kern`, `kerx` and `morx`,
and the layout tables (`BASE` included) and variation tables (`MVAR` included) that
`retain_layout: false` or `retain_variations: false` drop. Tables the list above drops,
such as `COLR`, `CPAL`, `MVAR` and `DSIG`, stay, since no glyph ID changes, but a copied
`DSIG` signature no longer matches the file. Nothing is read, so no warnings come back.

`instance` handles variable fonts. It bakes a set of axis coordinates into a static
font, or pins some axes and leaves the rest variable. It moves `glyf` outlines (inferred
points and composite offsets included), the advances and side bearings their phantom
points give, and the `BASE` coordinates the store varies, the way HarfBuzz's instancer
does.

A malformed piece of a layout table (a GDEF list, a GSUB or GPOS lookup or subtable,
a Device table, an anchor), or a malformed vertical metrics table (`vhea`, `vmtx`,
`VORG`, `VVAR`), `BASE` or `STAT`, is left out of a subset instead of failing the whole
run, the way HarfBuzz handles it. `instance` does the same for the vertical metrics
tables, for `BASE`, and for the GDEF and FeatureVariations data it rebuilds. Every piece
left out is reported in `SubsetOutput::warnings` (or `InstancedOutput::warnings`) with
its table, byte offset, and reason.

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

The crate's own code needs only `alloc`, but it depends on `sigilbuzz` with that crate's
default `std` feature. Turning off default features here therefore does not make it
`no_std`, and it still needs a target with `std`.

## License

Apache-2.0. See the workspace root `LICENSE`.
