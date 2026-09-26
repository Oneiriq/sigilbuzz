# Changelog

Every release of the sigilbuzz workspace, newest first. Numbers in parentheses are
GitHub issues and pull requests. sigilbuzz is pre-1.0, so a minor release may change
the API. See [docs/STABILITY.md](docs/STABILITY.md) for what is covered by the
stability commitment.

## 0.22.0 (unreleased)

Added:

- `OwnedFace`, a face that owns its font bytes behind an `Arc<[u8]>` and has no
  lifetime parameter. It parses the table directory once and hands out `Face` views
  through `as_face()`. It is `Send + Sync` and cheap to clone, so you can keep parsed
  faces in a cache or share them across worker threads.
- TrueType Collection (`.ttc`) support. `Face::parse` and `OwnedFace::parse` now take a
  member index into a collection. They used to reject collections as unsupported. The
  new `fonts_in_collection` returns the member count, or `None` for a plain TTF or OTF.
- `BidiMap`. After `Buffer::set_text_bidi`, `Buffer::bidi_map` maps byte offsets
  between visual order (what `Glyph::cluster` indexes) and logical order (the source
  text), and reports the embedding level at each position. Use it to put carets and
  selections back into the original text.
- `fuzz/`: cargo-fuzz targets for every part of the workspace that reads untrusted
  input. See [fuzz/README.md](fuzz/README.md).

Changed:

- The minimum supported Rust version is now 1.81. The core crate already needed 1.81
  for `core::error::Error`, so the old `rust-version = "1.75"` was wrong.
- `sigilbuzz-woff` 0.3.1 and `sigilbuzz-render` 0.8.1 move to `miniz_oxide` 0.9.
- `sigilbuzz-capi` 0.2.2 installs with `cargo cinstall` from cargo-c. That puts
  `libsigilbuzz`, the header (`include/sigilbuzz/hb.h`), a generated `sigilbuzz.pc`, and
  a CMake package in place in one step. The old pkg-config and CMake templates had to be
  filled in by hand and looked for a `libsigilbuzz` that `cargo build` never produced
  (it builds `libsigilbuzz_capi`). They are gone.
- The companion crate benchmarks moved to Criterion 0.8.
- A full `LICENSE` file now sits at the repo root, and `NOTICE` spells out the
  attribution terms. The license is still Apache-2.0.
- The documentation was rewritten, and the release history moved out of
  `docs/ROADMAP.md` into this file.
- CI and the pre-push hook lint and test the whole workspace. They used to cover only
  the root crate. CI also checks the minimum Rust version, including every `no_std`
  build.
- Every companion crate gets a patch release for the fixes below: `sigilbuzz-subset`
  0.11.1, `sigilbuzz-paint` 0.1.1, `sigilbuzz-svg` 0.1.2, `sigilbuzz-pdf` 0.2.2,
  `sigilbuzz-gpu` 0.1.1, `sigilbuzz-text-layout` 0.1.1, `sigilbuzz-hyphen` 0.1.1, and
  `sigilbuzz-cli` 0.1.1.

Fixed:

A hardening pass for hostile input. Fonts, images, and text can come from anywhere,
and a malformed one must not crash, hang, or exhaust memory. Shipped code no longer
contains `unwrap`, `expect`, or panic macros, and every fix has a regression test.
Fuzzing found the first bugs, and a review of every crate found the rest. Output for
valid input is unchanged except where noted.

- Panics on malformed fonts in CFF (INDEX offsets, charstring operands, subroutine
  indexes), AAT `morx`, the GSUB and GPOS skip iterator, the JPEG decoder, and WOFF2
  wrapping. One panic was reachable with an ordinary font and ordinary text: an Arabic
  letter after a decomposed Thai vowel crashed the Arabic joining step, and on longer
  text it misaligned the joining forms.
- Allocations sized from counts in the file without checking the data behind them: up
  to 17 GB in CFF2, 200 GB in `morx`, 32 GB in `MultiItemVariationStore`, 25 GB in
  contextual rule sets, 8.6 GB in `gvar` subsetting, and 30 GB in the rasterizer.
  Decompression in WOFF1, WOFF2, PNG, JPEG, and TIFF is now capped by what the input
  can plausibly hold.
- Hangs and runaway work: CFF subroutine bombs, composite glyphs that fan out (`glyf`,
  VARC, EBDT), cyclic `morx` chains, nested GSUB and GPOS lookups (now bounded per
  `shape()` call, like HarfBuzz), unbounded buffer growth from multiple substitution and
  `morx` insertion, SVG `<use>` fan-out, COLR paint graphs, and quadratic passes in
  bidi resolution, Indic and USE reordering, line wrapping, and subsetting.
  Hyphenation checked all 4,938 US English patterns at every letter. It now checks
  only the patterns that start with that letter, about 10 times faster with the same
  result.
- Subsetting a large font could produce broken layout tables. Rewritten GSUB and GPOS
  tables over 64 KB wrapped their 16-bit offsets. They now use Extension lookups when
  they need to.
- `BASE` offsets past 64 KB were truncated, so baseline tags were read from the wrong
  place.
- The `no_std` builds did not compile on Rust 1.81, the declared minimum.
- `sigilbuzz-capi`: `hb_set_t` was not safe to share between threads,
  `hb_font_paint_glyph` truncated glyph ids above 65535, and a language string with an
  embedded NUL leaked memory on every call. Every `unsafe` block now says why it is
  sound.
- `sigilbuzz-cli`: writing to a closed pipe panicked. It now reports an error.
- `sigilbuzz-woff`: the `woff2` feature did not build without the default features.

The new limits only affect fonts far beyond anything real, for example a glyph with
more than 65,536 points, or a `shape()` call that needs more than 64 lookup
applications per glyph (never fewer than 16,384 in total).

## 0.21.0 (2026-04-25)

`sigilbuzz-render` gains two more embedded-image formats and finishes SVG masks and
text on a path.

- sbix TIFF decoder (#240): baseline TIFF with II/MM headers, a single IFD, 8-bit RGB
  and RGBA, no compression or PackBits, strip-organized, chunky planar. CCITT, LZW,
  JPEG-in-TIFF, multiple IFDs, and other photometrics return `Unsupported`.
- Progressive JPEG (#241): SOF2 decoding with DC first and refinement scans, plus AC
  first scans with EOB-run tracking. AC refinement scans return a `BadJpeg` error for
  now.
- SVG `mask-type="alpha"` and `maskUnits="objectBoundingBox"` (#242). Nested masks are
  still unsupported.
- SVG `<textPath>` (#243): places glyph runs the caller has already shaped along a
  path by arc length. Tangent rotation and `side="right"` are not done yet.

Fixed:

- A mask that referenced itself could overflow the stack. A depth guard now stops it
  (#244).

## 0.20.0 (2026-04-25)

- sbix JPEG decoder (#237): a small baseline JPEG decoder with Huffman decoding, a
  float IDCT, and YCbCr to RGB conversion. Handles 3-component and grayscale images at
  4:4:4, 4:2:2, and 4:2:0 sampling.
- SVG `<mask>` (#236), using luminance from BT.709.
- Stable crate-root names (#238). These items were only reachable through hidden
  `ot::*` and `unicode::*` paths. They are now exported from the crate root and covered
  by the stability commitment: the `feature` tag constants, `JoiningForm`,
  `UnicodeScript`, `script_of`, `is_hangul_jamo`, `BidiInfo`, `BidiClass`,
  `bidi_class`, and `JoiningType`.
- `clippy::pedantic` now applies to every crate in the workspace, with each allowed lint
  justified in `Cargo.toml` (#239).

## 0.19.0 (2026-04-25)

- SVG filter primitives (#233): `feGaussianBlur`, `feColorMatrix`, `feOffset`,
  `feFlood`, and `feMerge`. A drop-shadow chain works end to end.
- Stroke dashes now follow true Bezier arc length (#234). Straight-segment paths render
  exactly as before.
- Pre-publish API audit (#235). About 75 exports are documented as stable in
  `docs/STABILITY.md`. Internal modules are marked `#[doc(hidden)]`, and
  `missing_docs = warn` applies to every crate.

Fixed:

- The PNG decoder accepted a malformed grayscale `tRNS` chunk (#231, #232).

## 0.18.0 (2026-04-25)

- `flatten_grouped()` in `sigilbuzz-render` returns flattened segments grouped by the
  curve they came from, which MSDF generators need for edge coloring (#218, #224).
- SVG `<polygon>`, `<polyline>`, `<line>`, `stroke-dasharray`, and
  `stroke-dashoffset` (#227).
- EBDT formats 8 and 9, composite monochrome bitmaps (#229).
- VARC subsetting now drops unused variation regions (#228).

Fixed:

- Rasterizing SVG or bitmap glyphs at huge sizes could panic while allocating the
  pixel buffer. Output is now capped at 16384 pixels per side and returns `BadSize`
  (#225, #226, #230).

## 0.17.0 (2026-04-25)

- PNG encoder in `sigilbuzz-render`: `encode_png` and `encode_png_alpha` write
  deterministic PNGs (#217).
- EBDT/EBLC monochrome bitmaps and sbix `dupe` glyphs (#221). Other sbix image types
  return `UnsupportedBitmap` instead of panicking.
- SVG strokes, linear and radial gradients, `<use>`, basic `clipPath`, `<rect>`,
  `<circle>`, and `<ellipse>` (#219).
- VARC subsetting prunes unused variation data (#220).

Fixed:

- `sigilbuzz-svg` and `sigilbuzz-pdf` wrote `NaN` and `inf` into their output for
  non-finite numbers. Both now write 0 (#216, #222).

## 0.16.0 (2026-04-25)

Three gaps found while moving oniq's MSDF glyph generator onto sigilbuzz.

- `flatten()`, `Segment`, and `DEFAULT_TOLERANCE` are public in `sigilbuzz-render`
  (#208, #211).
- `Face::glyph_outline` works for CFF2 fonts (#209, #213). This also fixed two CFF2
  bugs: INDEX counts were read as 16-bit instead of 32-bit, and `blend` could corrupt
  the operand stack at default coordinates.
- `name` table parser with `Face::name()` (#210, #212).

## 0.15.0 (2026-04-25)

`sigilbuzz-render` now handles every kind of glyph a modern emoji font ships.

- COLRv1 rendering (#204): linear, radial, and sweep gradients with pad, repeat, and
  reflect, Porter-Duff compositing, clipping, nested color glyphs, and variations.
- SVG-in-OT rendering (#205): paths, transforms, and fills. Gzipped SVG documents
  return `SvgGzipped`.
- CBDT and sbix PNG bitmaps (#207), with a small PNG decoder that uses `miniz_oxide`
  for inflate.

Fixed:

- The rasterizer could panic on non-finite or extreme coordinates (#202).
- COLRv0 rendering accepted an out-of-range palette index when every layer used the
  foreground color (#203).

## 0.14.0 (2026-04-25)

- Partial instancing of `gvar` fonts (#194) and CFF2 fonts (#195): pin some axes and
  keep the rest variable.
- VARC subsetting (#193).
- New crate `sigilbuzz-render` (#199): a CPU rasterizer for outlines and COLRv0 color
  glyphs, with 8x vertical supersampling and variable-font support.
- Non-finite inputs no longer break partial instancing math (#185, #186, #192).

Fixed:

- VARC component glyph IDs above 0xFFFF were silently truncated (#196).
- CFF2 operand decoding missed the `shortint` form (#197).
- A CFF2 `return` inside an inlined subroutine cleared the caller's stack tracking
  (#198).

## 0.13.0 (2026-04-25)

- VARC variable composite glyphs from OpenType 1.10 (#184), including the new
  `MultiItemVariationStore`. Outlines resolve through `Face::glyph_outline_at_coords`.
- Partial instancing API, `AxisPin::{Pin, Keep}` (#183), with `fvar`, `avar`, and
  variation-store rewriting (#190).
- `instance()` now folds GPOS variation deltas into static values (#175, #188).
- The C API test build uses a cross-process file lock, so parallel test runs no
  longer race (#172).

Fixed:

- CFF operand re-encoding corrupted 5-byte values outside the i16 range. It now
  returns `Unsupported` (#187, #189).

## 0.12.0 (2026-04-25)

- `instance()` bakes CFF2 `blend` operators and applies `VVAR` and `MVAR` metrics
  (#163).
- AAT `kerx` format 4 control-point and anchor-point actions, with a new `ankr` parser
  and `Face::glyph_points` (#166).
- CFF subsetting now prunes unused subroutines (#167).
- OpenType `BASE` table (#170).
- New crate `sigilbuzz-hyphen` (#171): Liang hyphenation with bundled US English
  patterns and an optional bridge to `sigilbuzz-text-layout`.

Fixed:

- Two bugs in the instance bake: dropped VVAR advance deltas (#176, #177) and
  duplicate MVAR tags applied twice (#178, #179).

## 0.11.0 (2026-04-25)

- Variable-font instancing (#161): `sigilbuzz_subset::instance` bakes a set of axis
  coordinates into a static font.
- `MVAR` and `VVAR` variable metrics (#162).
- The OpenType `MATH` table, all five subtables (#164). Math layout itself is up to
  the caller.
- UAX 9 paired-bracket handling (#148), CFF2 subsetting for single-FD fonts, and
  `kerx` format 4 inline-coordinate actions (#165).

## 0.10.0 (2026-04-25)

- Full UAX 9 bidi ordering (#147), with `BidiInfo` and `Buffer::set_text_bidi`.
- New crate `sigilbuzz-text-layout` (#149): UAX 14 line breaking, width-based line
  wrapping, and a simplified UAX 29 word iterator.
- Real CFF1 and CFF2 fixture fonts for the subsetter tests (#150).
- AAT `kerx` format 6, `morx` formats 4 and 5, and parsing for `kerx` format 4 (#151).

Fixed:

- FSI direction was hard-coded to RTL (#155).
- `wrap_lines` reported widths that ignored its own trailing-space trim (#153), and
  trimmed only 3 of the 10 UAX 14 space characters (#159).
- `kerx` format 6 was missing a bounds check (#157).

## 0.9.0 (2026-04-25)

- WOFF1 zlib compression and decompression through `miniz_oxide`, behind the
  `woff1-deflate` feature (#137).
- CFF subsetting handles global subroutines shared across font DICTs (#138).
- AAT `kerx` format 1 state-machine kerning (#139).
- New crate `sigilbuzz-cli` (#141): the `sigilbuzz` binary with `shape`, `subset`,
  `paint`, `slug`, `svg`, `woff`, `pdf`, and `info` subcommands.

Fixed:

- A WOFF2 test panicked under `--no-default-features` (#142, #143).
- CLI glyph-range parsing was off by one (#144, #145).

## 0.8.0 (2026-04-25)

- C API additions: `hb_set_t`, `hb_subset_*`, `hb_paint_funcs_t` with
  `hb_font_paint_glyph`, `hb_face_collect_unicodes`, and
  `hb_ot_layout_collect_features` (#102, #103, #104).
- The subsetter rewrites every GSUB and GPOS lookup type at the byte level (#109,
  #112, #121, #127, #128, #131).
- Full CFF subsetting: non-CID CFF1, CID-keyed CFF1, and CFF2 (#120, #135).
- Brahmi, Sharada, Khojki, Tirhuta, and Modi through the Universal Shaping Engine
  (#123). `nukt` and `akhn` were added to the USE basic features.

Fixed:

- Mongolian ligatures advanced the cursor by the input span instead of the output
  span (#118, #134).
- Mark advances are zeroed at the right stage for each script, found through Limbu
  (#115, #124).
- Cham `pref` ordering (#116) and N'Ko cursive joining (#114).
- The CFF charset and Encoding writers overflowed or truncated on large inputs (#129,
  #130, #132, #133).

## 0.7.0 (2026-04-25)

- New crate `sigilbuzz-capi`: a C library exporting HarfBuzz's `hb_*` symbols, so C
  programs can link it in place of HarfBuzz. Ships with a header, pkg-config template,
  and CMake module.
- CFF emitter building blocks for the subsetter, and byte-level GSUB and GDEF
  rewriting for the first lookup types.
- `wrap_woff2`, the WOFF2 compression direction.
- CBDT/CBLC and sbix color bitmap tables, and the `SVG ` table.
- Tibetan, Mongolian, and eight more USE scripts: N'Ko, Buginese, Tai Tham, Balinese,
  Sundanese, Lepcha, Limbu, and Cham.
- Each GSUB lookup now runs once per pass, matching HarfBuzz.

## 0.6.0 (2026-04-24)

- The subsetter keeps GSUB, GPOS, and GDEF when it is safe to, and keeps variable-font
  tables by default.
- Complex-script shaping got much faster (#74, #75, #76). Devanagari went from 173x
  slower than rustybuzz to 4.98x, Arabic from 32x to 2.72x, and Khmer from 17x to
  1.88x. Latin and Hebrew are now faster than rustybuzz. See
  [docs/PERFORMANCE.md](docs/PERFORMANCE.md).
- New crate `sigilbuzz-woff`: WOFF1 and WOFF2 unwrap, plus uncompressed WOFF1 wrap.
  This added the workspace's first runtime dependency (Brotli), confined to this crate.

Fixed:

- The Coverage writer wasted bytes on non-identity inputs (#94, #95).
- WOFF2 empty-glyph and bbox-bitmap alignment (#96, #97), and `transformVersion`
  validation (#98, #99).

## 0.5.0 (2026-04-24)

- New crate `sigilbuzz-subset`, the `hb-subset` equivalent for TrueType fonts.
- `sigilbuzz-pdf` gains Type 1 and embedded OpenType fonts.
- Criterion benchmarks against rustybuzz.
- Every crate is ready for crates.io, with full metadata and the Apache 2.0 license
  text.
- Composite glyphs anchored to phantom points resolve correctly.

Fixed:

- Type 3 PDF fonts returned an infinite font matrix for malformed faces.

## 0.4.0 (2026-04-24)

- Amiri `rlig` parity (#21). GSUB now applies lookups the way HarfBuzz does. Amiri
  parity with rustybuzz went to 6710 of 6710.
- AAT `kerx` format 2.
- New crate `sigilbuzz-svg`: glyph outlines and COLRv1 glyphs as SVG.
- New crate `sigilbuzz-pdf`: Type 3 font output.

Fixed:

- Composite glyphs with a 2x2 transform read the matrix in the wrong order.
- COLRv1 variable alpha and color-stop values were scaled wrong, and fonts that keep
  their variation store in GDEF ignored variations.
- An SVG path list could misalign when a glyph had no outline, and one malformed
  `kerx` subtable could spoil the whole table.

## 0.3.0 (2026-04-24)

- The repository became a Cargo workspace.
- Glyph outlines from `glyf` (including composites and `gvar`), CFF, and CFF2, through
  `Face::glyph_outline` and `Face::glyph_outline_at_coords`.
- New crate `sigilbuzz-gpu`: Slug outline encoding for GPU rendering.
- New crate `sigilbuzz-paint`: COLRv1 paint evaluation.
- Myanmar kinzi (#44), Thai and Lao AM decomposition (#45), and Old Hangul in mixed
  buffers (#46).

Fixed:

- CFF stack depth cap, Slug handling of NaN and zero tolerance, and a CFF `hflex1`
  operand index.

## 0.2.0 (2026-04-24)

- Universal Shaping Engine with Khmer, Myanmar, Thai, Lao, and Old Hangul.
- The rest of the Indic family: Bengali, Gurmukhi, Gujarati, Oriya, Tamil, Telugu,
  Kannada, Malayalam, and Sinhala.
- Hebrew, including niqqud and cantillation marks.
- Mixed-script buffers split into script runs automatically.
- AAT `morx` and `kerx` fallback when a font has no GSUB or GPOS.
- GPOS variation deltas in variable fonts (#13).

## 0.1.0 (2026-04-24)

The first release.

- Core API: `Blob`, `Face`, `Font`, `Buffer`, `Glyph`, and `shape`.
- Parsers for `head`, `maxp`, `hhea`, `hmtx`, `cmap`, `GDEF`, `loca`, and `glyf`.
- Every GSUB and GPOS lookup type, lookup flags, and script and language selection.
- Arabic joining and Devanagari reordering.
- Vertical text and the legacy `kern` table.
- Variable fonts: `fvar`, `avar`, `gvar`, `HVAR`, and `Font::with_coords`.
- COLRv0, COLRv1, and CPAL.
- Latin shaping matches rustybuzz output on Open Sans.
