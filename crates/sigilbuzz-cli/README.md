# sigilbuzz-cli

The command-line tool for the [sigilbuzz](https://github.com/Oneiriq/sigilbuzz)
workspace. It works like `hb-shape`, but covers the whole stack: shaping, subsetting,
COLRv1 paint evaluation, GPU (Slug) encoding, WOFF wrap and unwrap, PDF font output,
SVG glyph output, and font info.

The binary is called `sigilbuzz`. The crate is called `sigilbuzz-cli` so it sits next
to the other companion crates.

## Install

```sh
cargo install --path crates/sigilbuzz-cli
```

That puts a `sigilbuzz` binary on your `PATH`. Its only external dependency is `clap`.
`docs/deps.md` in the workspace root explains why.

## Commands

### `sigilbuzz shape`

Shape text with a font and print the glyphs.

```sh
sigilbuzz shape FONT.ttf "Hello, world"
sigilbuzz shape FONT.ttf "Hello" --features liga,-kern,smcp=1
sigilbuzz shape FONT.ttf "مرحبا" --direction rtl
sigilbuzz shape FONT.ttf "abc مرحبا 123" --bidi
sigilbuzz shape FONT.ttf "Hi" --json
```

`--bidi` treats the text as a bidirectional paragraph: each run of one embedding level
is shaped in its own direction, the runs come out in visual order, and clusters stay
byte offsets into the text. `--direction` then sets the paragraph direction (`ltr` or
`rtl`) instead of guessing it from the first strong character.

Default output:

```text
gid=43 advance=1511 cluster=0
gid=76 advance=518 cluster=1
```

`--json` prints a single-line array of `{gid, cluster, x_advance, y_advance, x_offset,
y_offset}` records. Feature syntax matches `hb-shape`: `tag=value`, `+tag` to turn a
feature on, and `-tag` to turn it off.

### `sigilbuzz subset`

Cut a font down to a set of glyphs or codepoints.

```sh
sigilbuzz subset FONT.ttf OUT.ttf --gids 1,2,3
sigilbuzz subset FONT.ttf OUT.ttf --gids 0..=255
sigilbuzz subset FONT.ttf OUT.ttf --unicodes A,B,U+1F600
sigilbuzz subset FONT.ttf OUT.ttf --gids 0,1 --unicodes 'H,i' --drop-layout
```

You can combine `--gids` and `--unicodes`. They are merged before subsetting.
`--drop-layout` and `--drop-variations` drop tables that are kept by default.
`--retain-hints` keeps hinting instructions, which are dropped by default.

### `sigilbuzz paint`

Walk the COLRv1 paint tree for a glyph and print one `DrawCmd` per line.

```sh
sigilbuzz paint FONT.ttf 42
```

Output:

```text
FillGlyph gid=42 transform=Transform2D { ... } paint=Solid { color: ..., is_foreground: false }
PushLayer mode=SrcOver
FillGlyph gid=43 transform=...
PopLayer
```

A glyph with no paint tree prints a note to stderr and exits with `0`. Paint data is
optional, so a missing tree is not an error.

### `sigilbuzz slug`

Encode a glyph for GPU rendering with the Slug algorithm.

```sh
sigilbuzz slug FONT.ttf 42
sigilbuzz slug FONT.ttf 42 --bands 8 --cubic-tolerance 0.5
```

The output is JSON:

```json
{"bbox":{"xmin":201,"ymin":0,"xmax":1311,"ymax":1462},
 "bands":[{"segment_offset":0,"segment_count":6}, ...],
 "segments":[{"p0":[1311,0],"p1":[1226,0],"p2":[1141,0]}, ...]}
```

### `sigilbuzz woff`

Wrap or unwrap WOFF1 and WOFF2 files.

```sh
sigilbuzz woff wrap FONT.ttf FONT.woff2
sigilbuzz woff wrap FONT.ttf FONT.woff1 --format woff1
sigilbuzz woff unwrap FONT.woff2 FONT.ttf
```

`unwrap` detects WOFF1 or WOFF2 from the file's first four bytes.

### `sigilbuzz pdf`

Emit PDF font fragments. Only Type 3 is available from the command line so far. The
`sigilbuzz-pdf` library also writes Type 1 and embedded OpenType fonts.

```sh
sigilbuzz pdf type3 FONT.ttf OUT.pdf-fragment --gids 0..=255
```

The output is a labeled UTF-8 dump of the FontBBox, FontMatrix, Encoding, and
CharProcs. You assemble those into a PDF object stream yourself.

### `sigilbuzz svg`

Write one glyph as SVG.

```sh
sigilbuzz svg FONT.ttf 42 OUT.svg            # outline only
sigilbuzz svg FONT.ttf 42 OUT.svg --color    # COLRv1 when the glyph has it
```

With `--color`, a glyph that has no paint tree falls back to its outline, so the
command always produces something useful.

### `sigilbuzz info`

Print font metadata: the SFNT version, glyph count, units per em, the sorted table
list, and the GSUB and GPOS feature tags.

```sh
sigilbuzz info FONT.ttf
```

Sample output for Open Sans:

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

Apache-2.0, like the rest of the workspace.
