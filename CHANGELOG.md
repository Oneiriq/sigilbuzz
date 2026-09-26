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
- Buffer script, language and context. `Buffer::set_script`, `set_language`,
  `set_pre_context` and `set_post_context` (with getters and `Buffer::CONTEXT_LENGTH`)
  now reach shaping. A set script shapes the whole buffer as that script. The language
  picks the OpenType language system in GSUB and GPOS, and the context text lets
  Arabic, N'Ko and Mongolian letters at the buffer edges join across it.
- `Language`, a BCP 47 tag that maps to OpenType language system tags (at most three,
  as in HarfBuzz). The table is generated from the OpenType language tag registry and
  SIL's ISO 639 data. `cargo test --test language_table_gen -- --ignored` regenerates
  it.
- `Buffer::unset_direction`, `Buffer::has_explicit_direction`, and
  `Buffer::set_insert_dotted_circle`. Broken Indic, Khmer, Myanmar and USE syllables now
  get a U+25CC dotted circle, as in HarfBuzz.
- `UnicodeScript::{iso15924_tag, from_iso15924_tag, horizontal_direction}` and
  `Direction::horizontal_for_script`.
- `ShapedRun` is re-exported from the crate root.
- GPOS cursive attachment (lookup type 3). `curs` runs by default on horizontal runs.
  It never ran before.
- `PairPos::lookup_with_device_base`, which also returns the bytes the records' Device
  and VariationIndex offsets are measured from (the PairSet in format 1, the subtable in
  format 2), and `PairPos::value_format2`.
- `sigilbuzz-capi`: `hb_buffer_add_utf32`, `hb_buffer_add_codepoints`,
  `hb_buffer_add_latin1`, `hb_subset_input_reference`, `hb_subset_input_create_or_fail`,
  `hb_paint_funcs_reference`, `hb_paint_funcs_make_immutable` and `_is_immutable`, the
  `push_clip_rectangle`, `image` and `custom_palette_color` paint callbacks,
  `hb_color_line_get_color_stops`, `hb_color_line_get_extend`, and `hb_color_get_*`.
- `sigilbuzz-paint`: `evaluate_with` and `EvalOptions` choose the variation coordinates,
  the CPAL palette and the foreground color in one call.
- `sigilbuzz-render`: `Rasterizer::with_foreground` picks the COLR foreground color.
- COLR `ClipList` parsing: `tables::colr::{ClipList, ClipBox}` and
  `Colr::{clip_list, clip_box, clip_list_offset, var_index_map_offset}`.
- `sigilbuzz-capi`: `hb_paint_funcs_set_color_glyph_func`. `hb_version` reports 8.2.0,
  the HarfBuzz release that added that callback.
- `sigilbuzz-paint`: `Transform2D::inverse`.
- `sigilbuzz-subset` 0.12.0: `SubsetWarning`, returned in `SubsetOutput::warnings` and
  `InstancedOutput::warnings`. Every malformed layout or variation structure the
  subsetter leaves out, instead of failing the run, is reported with its table, byte
  offset and reason, up to 65,536 warnings. The new fields break struct literals.
- `ClassDef::empty` and `ClassDef::parse_at`.
- `fuzz/`: cargo-fuzz targets for every part of the workspace that reads untrusted
  input. See [fuzz/README.md](fuzz/README.md).

Changed:

- `shape()` returns right-to-left and bottom-to-top runs in visual order, reversed after
  positioning as HarfBuzz does. It used to return them in logical order, although
  `ShapedRun` promised visual order. Mark offsets use HarfBuzz's direction-aware rules.
  Code that reversed RTL output itself must stop doing so.
- Text in a direction that is not its script's own (Hebrew marked LTR, Latin marked RTL,
  bottom-to-top) is shaped the way HarfBuzz does it: grapheme clusters are reversed, the
  run is shaped in its native direction, and the result is reversed back.
- `Buffer::set_text_bidi` now sets an explicit left-to-right shaping direction, because
  the text it stores is already in visual order. The paragraph direction is still
  available from `bidi_map().paragraph_direction()`.
- Mongolian switches to vertical layout only when no direction was set.
  `set_direction(Direction::Ltr)` now gives horizontal Mongolian (it used to need RTL).
- Bottom-to-top runs report a negative `y_advance`, like top-to-bottom ones.
- GSUB and GPOS take every feature from one script and one language system, as
  HarfBuzz does, and a language system's required feature always applies. Latin,
  Greek, Cyrillic, Han and kana runs try their own script tag (`latn`, `grek`, `cyrl`,
  `hani`, `kana`) before `DFLT`, so fonts that keep their kerning or ligatures under the
  script tag get them now. Rubik VF, for example, had no Latin kerning before.
- GPOS runs as one stage in lookup order: `abvm` and `blwm` run by default, a lookup
  shared by several features runs once, the first matching subtable wins, and `kern`,
  `dist` and `curs` run by default only on horizontal runs.
- `locl` and `ccmp` run first, and `locl` is applied at all (it never was, so a
  language system choice changed nothing).
- Mark attachment follows HarfBuzz: GSUB records ligature components, mark-to-mark
  checks components, base searches skip default ignorables, and the lookup's mark
  coverage (not the GDEF class) decides what attaches. Attachment offsets are resolved
  once, after all positioning, so marks follow kerning on their base.
- Mark widths are zeroed in both axes, per HarfBuzz's shaper for each script.
- Legacy `kern` and `kerx` pair values are split across both glyphs as in hb-kern.hh,
  and `kerx` is preferred over `kern`.
- Every Unicode default-ignorable character is hidden, after positioning rather than
  before GSUB: its real glyph goes through GSUB and becomes the space glyph at the end.
- Right-to-left runs mirror characters that have a mirror glyph and apply `rtlm`.
- Joining types are generated from Unicode 17.0 `ArabicShaping.txt`, so marks and format
  characters are transparent.
- Leading digits and punctuation join the script run that follows them, and text with
  no script shapes under `DFLT` (this changes `Buffer::script_runs`).
- `Buffer::clear` also resets script, language, context and the explicit direction.
- `tables::Anchor` has new public fields for its device offsets. Building one with a
  struct literal needs `..Anchor::default()`.
- `sigilbuzz-capi` 0.3.0 follows HarfBuzz's object rules, which breaks C code written
  against the old ones:
  - `hb_*_reference(p)` returns `p` and adds a reference, for every object type. A
    reference followed by two destroys used to double-free.
  - `hb_subset_input_unicode_set` and `hb_subset_input_glyph_set` return a set owned by
    the input. Do not destroy it.
  - A face keeps its blob alive and a font keeps its face alive. The blob's destroy
    callback runs when HarfBuzz runs it.
  - The paint API matches HarfBuzz 8.0's `hb-paint.h`: setters take `user_data` and
    `destroy`, callbacks receive their `user_data`, `push_clip_glyph` receives the font,
    and groups are `push_group` / `pop_group`. `HB_COLOR` packs blue high and alpha low.
  - `hb_buffer_clear_contents` resets direction, script, language and context, and
    `hb_buffer_set_direction(HB_DIRECTION_INVALID)` unsets the direction.
  - `hb_buffer_guess_segment_properties` guesses the script from the Unicode Script
    property and takes the direction from it.
- `hb_font_set_ppem` is documented as a no-op. sigilbuzz does not hint, so nothing read
  the value.
- `sigilbuzz-paint` 0.2.0 keeps the COLR foreground color apart instead of painting it
  white: `PaintSource::Solid` is `Solid { color, is_foreground }` and `ColorStop` has an
  `is_foreground` field. The default foreground is opaque black, and a palette or entry
  the font lacks resolves to the foreground, as in HarfBuzz.
- `sigilbuzz-render` 0.9.0 paints COLRv1 foreground layers black by default (they were
  white), the same as COLRv0.
- `sigilbuzz-svg` 0.2.0 writes foreground paints as `currentColor`.
- COLRv1 glyphs are clipped to their ClipList box, or to bounds computed from the paint
  tree, as in HarfBuzz. A glyph with unbounded paint renders empty. This holds in
  `sigilbuzz-render` (pixmaps are sized to the clip box), `sigilbuzz-svg` and
  `hb_font_paint_glyph`, which also emits HarfBuzz's root clip rectangle and offers
  referenced glyphs to `color_glyph`.
- `sigilbuzz-paint` reads COLR variation deltas only from the COLR table's own
  ItemVariationStore and DeltaSetIndexMap. It used to fall back to GDEF's store and
  read an index map from a made-up GDEF field. The renderers and the SVG writer now
  draw from the paint walk, and `evaluate_with` emits isolated groups for composites.
- `rasterize_colrv0_glyph` paints palette entries the font cannot supply in the
  foreground color instead of returning `NoCpal` or `BadPaletteIndex`, and SVG-in-OT
  `currentColor` follows `Rasterizer::with_foreground`.
- The CLI's `--script` and `--language` flags take effect.
- The companion crate READMEs no longer claim `no_std`. Every companion crate enables the
  core crate's `std` feature.
- The minimum supported Rust version is now 1.81. The core crate already needed 1.81
  for `core::error::Error`, so the old `rust-version = "1.75"` was wrong.
- `sigilbuzz-woff` 0.3.1 and `sigilbuzz-render` 0.9.0 move to `miniz_oxide` 0.9.
  `sigilbuzz-woff` 0.3.1 also moves to `brotli` 9.
- `sigilbuzz-capi` 0.3.0 installs with `cargo cinstall` from cargo-c. That puts
  `libsigilbuzz`, the header (`include/sigilbuzz/hb.h`), a generated `sigilbuzz.pc`, and
  a CMake package in place in one step. The old pkg-config and CMake templates had to be
  filled in by hand and looked for a `libsigilbuzz` that `cargo build` never produced
  (it builds `libsigilbuzz_capi`). They are gone.
- All benchmarks moved to Criterion 0.8.
- A full `LICENSE` file now sits at the repo root, and `NOTICE` spells out the
  attribution terms. The license is still Apache-2.0.
- The documentation was rewritten, and the release history moved out of
  `docs/ROADMAP.md` into this file.
- CI and the pre-push hook lint and test the whole workspace. They used to cover only
  the root crate. CI also checks the minimum Rust version, including every `no_std`
  build.
- Companion crate releases: `sigilbuzz-capi` 0.3.0, `sigilbuzz-paint` 0.2.0,
  `sigilbuzz-render` 0.9.0, `sigilbuzz-subset` 0.12.0, and `sigilbuzz-svg` 0.2.0 carry
  the breaking changes above. `sigilbuzz-pdf` 0.2.2, `sigilbuzz-gpu` 0.1.1,
  `sigilbuzz-text-layout` 0.1.1, `sigilbuzz-hyphen` 0.1.1, and `sigilbuzz-cli` 0.1.1 are
  patch releases for the fixes below.

Fixed:

A hardening pass for hostile input. Fonts, images, and text can come from anywhere,
and a malformed one must not crash, hang, or exhaust memory. Shipped code no longer
contains `unwrap`, `expect`, or panic macros, and every fix has a regression test.
Fuzzing found the first bugs, and a review of every crate found the rest. Output for
valid input is unchanged except where noted.

- Panics on malformed fonts in CFF (INDEX offsets, charstring operands, subroutine
  indexes), AAT `morx`, the GSUB and GPOS skip iterator, the JPEG decoder, and WOFF2
  wrapping. One panic was reachable with an ordinary font and ordinary text: an Arabic
  letter after a decomposed Thai vowel crashed the Arabic joining step (in `shape()` and
  `hb_shape`), and on longer text it misaligned the joining forms.
- Allocations sized from counts in the file without checking the data behind them: up
  to 17 GB in CFF2, 200 GB in `morx`, 32 GB in `MultiItemVariationStore`, 25 GB in
  contextual rule sets, 8.6 GB in `gvar` subsetting, and 30 GB in the rasterizer.
  Decompression in WOFF1, WOFF2, PNG, JPEG, and TIFF is now capped by what the input
  can plausibly hold.
- Hangs and runaway work: CFF subroutine bombs, composite glyphs that fan out (`glyf`,
  VARC, EBDT), cyclic `morx` chains, nested GSUB and GPOS lookups (now bounded per
  `shape()` call, like HarfBuzz), unbounded buffer growth from multiple substitution and
  `morx` insertion, SVG `<use>` fan-out, COLR paint graphs, and quadratic passes in
  bidi resolution, Indic and USE reordering, mark attachment, line wrapping, and
  subsetting. Hyphenation checked all 4,938 US English patterns at every letter. It now
  checks only the patterns that start with that letter, about 10 times faster with the
  same result.
- `BASE` offsets past 64 KB were truncated, so baseline tags were read from the wrong
  place.
- The `no_std` builds did not compile on Rust 1.81, the declared minimum.
- `sigilbuzz-capi`: `hb_font_paint_glyph` truncated glyph ids above 65535, and a
  language string with an embedded NUL leaked memory on every call. Every `unsafe`
  block now says why it is sound.
- `sigilbuzz-cli`: writing to a closed pipe panicked. It now reports an error.
- `sigilbuzz-woff`: the `woff2` feature did not build without the default features.

The new limits only affect fonts far beyond anything real, for example a glyph with
more than 65,536 points, or a `shape()` call that needs more than 64 lookup
applications per glyph (never fewer than 16,384 in total).

Settings and table data that were read and then ignored:

- Subsetting with `retain_hints` dropped `cvt `, `fpgm`, and `prep`. CFF and CFF2 fonts
  ignored `retain_layout`, `retain_variations`, and `drop_unhandled`, so OTF subsets
  lost GSUB, GPOS, `fvar`, and `HVAR`.
- `sigilbuzz-capi`: `hb_font_set_scale` did not change the output, `hb_blob_create`
  ignored its memory mode and dropped the destroy callback for empty blobs, and
  `hb_shape_full` ignored the shaper list.
- `sigilbuzz-woff`: WOFF1 wrapping wrote the input length as `totalSfntSize` instead of
  the padded size.
- `sigilbuzz-text-layout`: `break_at_word_boundaries = false` did nothing, mandatory
  breaks ignored `max_width`, newline characters counted toward the line width, and a
  lone CR produced no mandatory line break.
- `sigilbuzz-render` SVG glyphs: `stop-opacity` inside a `style` attribute was ignored,
  a trailing `;` in `style` dropped the whole gradient stop, and
  `stroke-linejoin="bevel"` left a notch at every outer corner instead of drawing the
  bevel.

Output that differed from HarfBuzz:

- Mark positioning ignored AnchorFormat3 device tables, so marks in variable fonts stayed
  at the default instance. VariationIndex deltas now apply to mark, mark-to-mark,
  mark-to-ligature and cursive anchors.
- PairPos format 1 device offsets are measured from the PairSet, as the spec says
  (variable kerning in fonts like Rubik was wrong). The `var_kern.ttf` fixture is
  rebuilt spec-correct by a Rust builder.
- Default ignorables were hidden by matching cluster values, so a visible glyph that
  shared a cluster with one lost its advance. `Glyph::unicode_props` was written but
  never read. It now carries the per-glyph flag, and a GSUB substitution un-hides the
  glyph, as in HarfBuzz.
- Indic and USE shaping of runs that do not start the text.
- The bidi pairs for the tick square brackets (U+298D to U+2990).
- Vertical runs now start each glyph from its vertical origin.
- `sigilbuzz-capi`: `hb_buffer_set_script` and `hb_buffer_set_language` did nothing.
  `hb_buffer_add_utf8` and `hb_buffer_add_utf16` reported clusters in the wrong units,
  ignored the text around the item, and dropped a whole call on malformed input instead
  of replacing bad code units with U+FFFD.
- `sigilbuzz-capi`: `hb_set_t` kept its contents in a `RefCell` while claiming to be
  thread-safe, so two threads reading one set (two `hb_subset_or_fail` calls sharing an
  input) could corrupt it and abort. It now sits behind a lock.
- `sigilbuzz-capi`: `hb_font_paint_glyph` ignored `palette_index`, `foreground_color`
  and the font's variation coordinates, fired nothing for glyphs without color data,
  and did not paint COLRv0 layers.
- `sigilbuzz-paint`: sweep gradient angles lacked the COLRv1 half-turn bias, and
  variable scale, rotate, skew, affine and sweep-angle deltas were added in raw units.
- `sigilbuzz-render`: `rasterize_colrv1_glyph` ignored its `palette_index`, and sweep
  gradients were mirrored.
- The COLR v1 header was read with four offsets instead of five, so
  `Colr::var_store_offset` returned the DeltaSetIndexMap offset and variable COLRv1 fonts
  built to the spec got no deltas. A v1 table cut short before its last offset is now
  rejected.
- COLRv1 rendering in `sigilbuzz-render`, `sigilbuzz-svg` and `evaluate_with`: a
  transform below a `PaintGlyph` distorted the glyph outline (visible on gradient emoji),
  composites blended into earlier layers instead of isolated groups, linear gradients
  ignored their rotation point p2, and radial gradients under a non-uniform scale were
  approximated.
- `sigilbuzz-subset`: the subsetter dropped GDEF `MarkGlyphSetsDef`, `AttachList`,
  `LigCaretList` and the ItemVariationStore. Dropping the mark glyph sets broke every
  lookup that uses a mark filtering set, and dropping the store left subset variable
  fonts without their kerning deltas.
- `sigilbuzz-subset`: the instancer resolved AnchorFormat3 and PairSet device offsets
  against the wrong table, so instanced fonts kept default-instance anchors and kerning,
  and it now folds ligature caret variations too.
- `sigilbuzz-subset`: GPOS Device and VariationIndex tables were not copied into
  subsets, leaving dangling offsets. Static subsets now leave VariationIndex tables out.
- `sigilbuzz-subset`: rebuilt GSUB and GPOS tables over 64 KiB wrapped their 16-bit
  offsets and silently corrupted their lookups and subtables. Lookups are promoted to
  Extension lookups, oversized mark and PairPos format 1 subtables are split the way
  HarfBuzz splits them, and any other overflow is reported as an error.
- `sigilbuzz-subset`: context rules with no lookup records (`ignore sub`,
  `ignore pos`) were dropped, so guarded rules such as Amiri's Allah ligature shaped
  differently after subsetting.
- `sigilbuzz-subset`: a malformed GDEF piece is left out instead of failing the whole
  subset. There are no more panics on truncated mark arrays or SinglePos headers, or on
  32-bit targets from crafted GDEF offsets. Full and partial instancing rebuild GDEF
  instead of truncating it at the store, and partial instancing renumbers
  VariationIndex rows.
- `sigilbuzz-subset`: subsets and instances of variable fonts keep GSUB and GPOS
  FeatureVariations. Subsets remap their indices, full instances apply the record that
  matches their coordinates, and partial instances settle pinned-axis conditions and
  renumber the kept axes. They used to be written as version 1.0 and lose them.
- `sigilbuzz-subset`: the closure keeps GSUB reverse chaining (type 8) substitutes.
- `sigilbuzz-subset`: partial instancing checks every ItemVariationStore, HVAR, VVAR
  and MVAR offset and size (no wraparound on 32-bit targets). A table it cannot rebuild
  is dropped and reported instead of carried through with stale axes, and MVAR records
  past 64 KiB are an error.
- A null ClassDef offset is read as every glyph in class 0, as in HarfBuzz. The shaper
  used to parse the subtable itself as the ClassDef, so chained context format 2
  subtables with a null backtrack ClassDef (as fontmake writes them) failed to parse or
  matched invented classes. PairPos format 2 and the subsetter's class-based rewriters
  had the same bug.

Removed:

- The root crate's `alloc` feature. It gated nothing: the crate always needs `alloc`.
  `std` now only adds filesystem helpers such as `Blob::from_path`.

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

Three gaps found while moving an MSDF glyph generator onto sigilbuzz.

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
