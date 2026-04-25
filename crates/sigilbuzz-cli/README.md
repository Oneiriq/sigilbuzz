# sigilbuzz-cli

Command-line driver for the [sigilbuzz](https://github.com/Oneiriq/sigilbuzz)
workspace. Think `hb-shape`, but for the whole stack: shaping, subsetting,
COLRv1 paint evaluation, GPU/Slug encoding, WOFF wrap/unwrap, PDF font
emission, SVG glyph emission, and font-info dumps.

The binary is named `sigilbuzz` (the crate is named `sigilbuzz-cli` so it
sits next to the other companion crates in the workspace).

## Install

```sh
cargo install --path crates/sigilbuzz-cli
```

This drops a `sigilbuzz` binary on your `$PATH`. The only external
runtime dependency is `clap`, scoped to this crate (see
`docs/deps.md` in the workspace root for the full justification).

## Subcommands

### `sigilbuzz shape`

Shape text against a font and print the resulting glyph stream.

```sh
sigilbuzz shape FONT.ttf "Hello, world"
sigilbuzz shape FONT.ttf "Hello" --features liga,-kern,smcp=1
sigilbuzz shape FONT.ttf "مرحبا" --direction rtl
sigilbuzz shape FONT.ttf "Hi" --json
```

Output (default):

```text
gid=43 advance=1511 cluster=0
gid=76 advance=518 cluster=1
```

`--json` produces a single-line array of `{gid, cluster, x_advance,
y_advance, x_offset, y_offset}` records. Feature syntax mirrors
`hb-shape`: `tag=value`, `+tag` to enable, `-tag` to disable.

### `sigilbuzz subset`

Subset a font down to a chosen glyph or codepoint set.

```sh
sigilbuzz subset FONT.ttf OUT.ttf --gids 1,2,3
sigilbuzz subset FONT.ttf OUT.ttf --gids 0..=255
sigilbuzz subset FONT.ttf OUT.ttf --unicodes A,B,U+1F600
sigilbuzz subset FONT.ttf OUT.ttf --gids 0,1 --unicodes 'H,i' --drop-layout
```

Both selectors merge before the subsetter runs so a caller can mix-
and-match. `--drop-layout` and `--drop-variations` invert the matching
defaults; `--retain-hints` opts back into instructions / hints.

### `sigilbuzz paint`

Walk the COLRv1 paint tree for a glyph and print one `DrawCmd` per
line.

```sh
sigilbuzz paint FONT.ttf 42
```

Output:

```text
FillGlyph gid=42 transform=Transform2D { ... } paint=Solid(...)
PushLayer mode=SrcOver
FillGlyph gid=43 transform=...
PopLayer
```

Glyphs without a paint tree exit `0` with a friendly stderr message
rather than erroring — paint is optional metadata.

### `sigilbuzz slug`

Encode a glyph for GPU rendering via the Slug algorithm.

```sh
sigilbuzz slug FONT.ttf 42
sigilbuzz slug FONT.ttf 42 --bands 8 --cubic-tolerance 0.5
```

Output is hand-rolled JSON:

```json
{"bbox":{"xmin":201,"ymin":0,"xmax":1311,"ymax":1462},
 "bands":[{"segment_offset":0,"segment_count":6}, ...],
 "segments":[{"p0":[1311,0],"p1":[1226,0],"p2":[1141,0]}, ...]}
```

(`serde_json` is a future-PR commitment; see `docs/deps.md`.)

### `sigilbuzz woff`

Wrap or unwrap WOFF1 / WOFF2 envelopes.

```sh
sigilbuzz woff wrap FONT.ttf FONT.woff2
sigilbuzz woff wrap FONT.ttf FONT.woff1 --format woff1
sigilbuzz woff unwrap FONT.woff2 FONT.ttf
```

Unwrap auto-detects WOFF1 vs WOFF2 from the input's 4-byte magic.

### `sigilbuzz pdf`

Emit PDF font fragments. Today the only flavour is Type 3:

```sh
sigilbuzz pdf type3 FONT.ttf OUT.pdf-fragment --gids 0..=255
```

Output is a labelled UTF-8 dump of the FontBBox / FontMatrix /
Encoding / CharProcs that the consumer assembles into a complete
PDF object stream.

### `sigilbuzz svg`

Emit SVG for a single glyph.

```sh
sigilbuzz svg FONT.ttf 42 OUT.svg            # outline-only
sigilbuzz svg FONT.ttf 42 OUT.svg --color    # COLRv1 when present
```

`--color` falls back to outline-only when the glyph has no paint
tree, so a single invocation produces a useful artifact regardless
of font.

### `sigilbuzz info`

Dump font metadata: face version, num_glyphs, units_per_em, the OT
table list (sorted), and the deduped GSUB + GPOS feature list.

```sh
sigilbuzz info FONT.ttf
```

Sample output (Open Sans):

```text
path: tests/fixtures/opensans_regular.ttf
sfnt_version: 0x00010000 (\x00\x01\x00\x00)
num_glyphs: 938
units_per_em: 2048
tables (19):
  DSIG
  GDEF
  GPOS
  GSUB
  ...
features (10):
  liga
  lnum
  ...
```

## License

Apache-2.0, matching the rest of the sigilbuzz workspace.
