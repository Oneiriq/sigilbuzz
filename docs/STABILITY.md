# Stability

This page says which sigilbuzz APIs you can rely on, which ones are public only for
technical reasons, and the rules I follow when adding or changing public items.

## Where things stand

Every crate in the workspace is below 1.0, so a minor release may break the API. The
crates are released together. Each release bumps the crates that changed, and one git
tag covers the whole set. Companion crates keep their own version numbers, so pin
versions that came out of the same release. For 0.23.1 that means `sigilbuzz = "0.23"`
with, for example, `sigilbuzz-render = "0.10"` and `sigilbuzz-paint = "0.3"`.
[RELEASING.md](RELEASING.md) has the release checklist.

The target is 1.0 in 2026, with a stable shaping API and a documented path for
renderers built on top of it.

## Three tiers

### Tier 1: stable public API

These items are documented and used by real consumers. Once the workspace reaches
1.0 they follow normal semver. Removals go through a `#[deprecated]` release first, and
signatures only change in a major version.

`sigilbuzz` (root crate):

- `Blob`, `Face`, `Font`, `Buffer`, `Glyph`, `Direction`, `Feature`
- `ShapedRun`: the positioned glyph run `shape` returns (`glyphs`, `len`, `is_empty`)
- `OwnedFace`: a face that owns its bytes, for caching and sharing across threads
- `BidiParagraph`: text of one or more paragraphs with its UAX 9 embedding levels,
  shaped run by run in logical order, HarfBuzz style (`BidiParagraph::{new, text,
  direction, base_level, paragraphs, paragraph_at, runs, level_at, run_at, line_runs,
  visual_runs, reorder_visual, shape_run, shape_line, shape}`), with `BidiRun`
  (`range`, `level`, `is_rtl`, `direction`), `BidiParagraphSpan` (`range`, `level`,
  `is_rtl`, `direction`), and `ShapedBidiRun` (`run`, `glyphs`). Glyph clusters are
  byte offsets into the text.
- `shape`
- `Face::glyph_outline`, `Face::glyph_outline_at_coords`, `Face::glyph_bounds`,
  `Face::name`, `Face::parse`, `Face::parse_bytes`
- `OwnedFace::parse`, `OwnedFace::as_face`, `OwnedFace::data`, `OwnedFace::data_arc`
- `fonts_in_collection`: the member count of a TrueType Collection (`Face::parse` and
  `OwnedFace::parse` take the member index)
- `Font::new`, `Font::with_coords`
- `tables::{Outline, OutlineSink, PathOp}`: outline iteration
- `tables::{Cmap, Glyf, Cff, Cff2, Head, Hhea, Hmtx, Maxp, Name, NameRecord}`: the
  main SFNT table views
- `tables::{Colr, ColrPaint, Cpal, Color}`: color glyphs
- `tables::{Fvar, VariationAxis, Avar, Hvar, Vvar, Mvar}`: variations
- `tables::tag::*`: table tag constants
- `Error`, `Result`, `VERSION`
- Added in 0.20.0 (#235). These used to be reachable only through the hidden `ot::*`
  and `unicode::*` paths:
  - `feature`: OpenType feature tag constants (re-export of `ot::feature`)
  - `JoiningForm`: the Arabic joining-form enum (re-export of
    `ot::arabic::JoiningForm`)
  - `UnicodeScript`: a coarse script bucket (re-export of `unicode::Script`, renamed to
    leave room for a future `ot::Script` tag enum)
  - `script_of`, `is_hangul_jamo`: character classifiers
  - `BidiInfo`: the UAX 9 result type
  - `BidiClass`, `bidi_class`: the UCD `Bidi_Class` enum and its lookup
  - `JoiningType`: the Arabic and Mongolian joining-type enum

  The deep `ot::*` and `unicode::*` paths stay hidden (Tier 2). Only the names above
  are stable.
- Added in 0.22.0, so that a buffer's script, language, and surrounding text reach
  shaping:
  - `Language`: a normalized BCP 47 tag (`Language::{new, as_str,
    ot_language_tags}`), mapped to OpenType language system tags by a table generated
    from the OpenType language tag registry
  - `Buffer::{set_script, script}`: shape the whole buffer as one script
  - `Buffer::{set_language, language}`: select the OpenType language system
  - `Buffer::{set_pre_context, pre_context, set_post_context, post_context}` and
    `Buffer::CONTEXT_LENGTH`: text around the run that cursive joining consults
  - `UnicodeScript::{iso15924_tag, from_iso15924_tag, horizontal_direction}` and
    `Direction::horizontal_for_script`: ISO 15924 codes and each script's horizontal
    direction
  - `UnicodeScript` buckets for Syriac and every other script HarfBuzz gives a shaper
    of its own. `UnicodeScript` is `#[non_exhaustive]` from 0.22.0: a later bucket is
    an additive change, and a `match` on it outside the crate needs a wildcard arm
  - `Buffer::unset_direction`: forget the caller's direction, like HarfBuzz's
    `hb_buffer_set_direction(buffer, HB_DIRECTION_INVALID)`
  - `BufferFlags` and `Buffer::{set_flags, flags}`: HarfBuzz's buffer flags with
    HarfBuzz's values (`BOT`, `EOT`, `PRESERVE_DEFAULT_IGNORABLES`,
    `REMOVE_DEFAULT_IGNORABLES`, `DO_NOT_INSERT_DOTTED_CIRCLE`). They replace the
    `Buffer::{set_insert_dotted_circle, insert_dotted_circle}` pair that 0.22.0
    development builds had.
  - `ClusterLevel` and `Buffer::{set_cluster_level, cluster_level}`: HarfBuzz's four
    cluster levels. A Rust `Buffer` defaults to `MonotoneCharacters`; the C API
    defaults to HarfBuzz's `MONOTONE_GRAPHEMES`. Both settings survive
    `Buffer::clear`.
- Added in 0.24.0:
  - `Font::with_variations`: bind a variable instance from user-space axis values, as
    HarfBuzz's `hb_font_set_variations` does. `Font::coords` now returns a slice that
    borrows the font, since such a font owns its coords.
  - `Face::glyph_outlines` and `GlyphOutlines::{outline, draw}`: draw many glyphs at
    one instance with the outline tables read once.

`sigilbuzz-render`:

- `Rasterizer`, `Pixmap`, `ColorPixmap`, `RenderError`
- `Affine`, `flatten`, `flatten_grouped`, `FlattenedCurve`, `Segment`,
  `DEFAULT_TOLERANCE`. `flatten` and `flatten_grouped` return the edges a path draws:
  a contour without `Close` stays open, and only `Close` emits a closing edge. That is
  part of the contract. The rasterizer closes open contours on its own fill paths.
- Added in 0.10.0: `Placement` and the `_placed` twin of each `Rasterizer` entry point
  (`rasterize_glyph_placed`, `rasterize_colrv0_glyph_placed`,
  `rasterize_colrv1_glyph_placed`, `rasterize_bitmap_glyph_placed`,
  `rasterize_svg_glyph_placed`, `rasterize_svg_glyph_with_text_paths_placed`), plus the
  free function `rasterize_bitmap_glyph_placed`. Each returns the image with its offset
  from the pen position on the baseline, in whole pixels with y down.
- `encode_png`, `encode_png_alpha`
- `decode_ebdt_mono`, `decode_png`, `rasterize_bitmap_glyph`, `rescale_bilinear`

`sigilbuzz-paint`:

- `evaluate`, `evaluate_at_coords`, `evaluate_with`, `EvalOptions`, `DrawCmd`, `GlyphId`,
  `PaintSource`
- `Color`, `Gradient`, `GradientKind`, `ColorStop`, `Extend`, `Transform2D`,
  `CompositeMode`

`sigilbuzz-gpu`:

- `encode_glyph`, `encode_glyph_at_coords`, `SlugOptions`
- `SlugGlyph`, `Band`, `QuadSegment`, `Bbox`, `Vec2`

`sigilbuzz-pdf`:

- `emit_type3_font`, `emit_type1_font`, `emit_otf_embedded_font`
- `Type1Font`, `OtfEmbeddedFont`
- `emit_d1_prologue`, `emit_fill_epilogue`, `emit_path_ops`, `outline_bbox`

`sigilbuzz-svg`:

- The whole crate: outlines to SVG and, with the `color` feature, COLRv1 to SVG.

`sigilbuzz-subset`:

- `subset`, `SubsetInput`, `SubsetOutput`
- `instance`, `AxisPin`, `F2Dot14`, `InstanceInput`, `InstancedOutput`
- `SubsetWarning`: a malformed piece of the source font that a subset or instance left
  out instead of failing, returned in `SubsetOutput::warnings` and
  `InstancedOutput::warnings`
- `compute_closure`, `subset_cff1_non_identity`, `subset_cff2_non_identity`,
  `emit_classdef`, `emit_coverage_from_glyphs`, `emit_coverage_from_pairs`

`sigilbuzz-woff`:

- `unwrap_woff1`, `wrap_woff1`, `wrap_woff1_with_options`, `WrapWoff1Options`
- `unwrap_woff2`, `wrap_woff2`, `wrap_woff2_with_options`, `WrapOptions`
- `WoffError`, `Result`

`sigilbuzz-text-layout`:

- `line_break_opportunities`, `LineBreakIter`, `BreakOpportunity`, `LineBreakClass`
- `wrap_lines`, `LineRange`, `WrapOptions`
- `word_breaks`
- Added in 0.2.0:
  - `line_break_opportunities_with` and `WordBreak` (`Normal`, `KeepAll`, `BreakAll`):
    the CSS `word-break` tailorings of the line breaking rules. `WordBreak` is
    `#[non_exhaustive]`, so a later CSS value such as `auto-phrase` is an additive
    change, and a `match` on it outside the crate needs a wildcard arm
  - `WrapOptions::word_break`, which `wrap_lines` passes on, and the builder methods
    `WrapOptions::with_max_width`, `with_word_break` and `with_break_at_word_boundaries`.
    Build options with the builders, or a struct literal with `..WrapOptions::default()`,
    so a later field does not break your code
  - `line_break_class`: the `Line_Break` property of a character
  - `WordBreakIter`: the iterator `word_breaks` returns, now nameable
  - `LineBreakClass` has every UAX 14 class and is `#[non_exhaustive]` from 0.2.0: a
    class a later Unicode version adds is an additive change, and a `match` on it
    outside the crate needs a wildcard arm
- The breaks follow UAX 14 and UAX 29 of the Unicode version the tables come from
  (17.0.0 in 0.2.0). A newer Unicode version can move breaks in a minor release.

`sigilbuzz-hyphen`:

- `hyphenate`, `Patterns`, `ParseError`, `Language`
- `break_opportunities_with_hyphens`, `HyphenatedBreak` (with the
  `text-layout-integration` feature)

`sigilbuzz-capi`:

- The C ABI declared in `include/hb.h`. It uses the same symbol names as HarfBuzz, and
  `tests/c_link.rs` checks that C code links against it.

### Tier 2: public but hidden (`#[doc(hidden)]`)

These items are reachable from outside the crate because a sibling crate in the
workspace needs them. They are not part of the stability commitment. They are hidden
from rustdoc so nobody pins against them by accident, and they may change in any minor
release.

- `sigilbuzz::ot::*`: the script shaping internals (the Arabic, Indic, USE, Mongolian,
  and Tibetan state machines, plus feature tag constants). The shaping API is
  `shape()`. The consumer-facing parts (`feature`, `JoiningForm`) are exported from the
  crate root and are Tier 1.
- `sigilbuzz::unicode::*`: the Unicode property tables. The consumer-facing parts are
  exported from the crate root and are Tier 1 (`UnicodeScript`, `script_of`,
  `is_hangul_jamo`, `BidiInfo`, `BidiClass`, `bidi_class`, `JoiningType`). The rest
  (bidi class tables, normalization helpers, bracket pairs, the joining-type table)
  stays hidden because the underlying Unicode data changes with each Unicode release.
- `sigilbuzz::tables::{ankr, avar, base, cbdt, cblc, cff, cff2, cmap, ebdt, eblc,
  fvar, gdef, glyf, gpos, gsub, gvar, head, hhea, hmtx, hvar, kern, kerx, layout, loca,
  math, maxp, morx, multi_var_store, mvar, name, parse, sbix, svg_table, varc,
  variation_store, vhea, vmtx, vorg, vvar}`: the per-table parser modules. The main
  types are exported from `tables::` and are stable. Helper types you can only reach
  through the module path (for example `tables::layout::FeatureList` or
  `tables::gpos::ChainContextPos`) are not.
- `sigilbuzz_paint::walk`: the paint-tree walk in HarfBuzz's callback order
  (`paint_glyph`, `paint_glyph_unclipped`, `PaintSink`, `RootClip`, `Resolver`,
  `ColorRef`, `StopRef`, `ColorLineRef`, `Painted`). sigilbuzz-capi drives its
  `hb_paint_funcs_t` bridge from it, and sigilbuzz-render and sigilbuzz-svg draw from
  it. Other renderers should use `evaluate_with`.

### Tier 3: internal

Anything in a module without `pub`, and anything `pub(crate)`, is internal. Each crate
keeps its modules private under `src/`, and the `pub use` lines in `lib.rs` decide what
consumers see.

## When to use `#[doc(hidden)]`

`#[doc(hidden)]` marks an item as public for technical reasons only. It means:

> This item compiles and links from outside the crate. Its name, signature, and
> behavior may change in the next minor release. Depend on it at your own risk.

The rules:

1. If a sibling crate in the workspace needs an item, the item stays `pub` and gets
   `#[doc(hidden)]`.
2. If no sibling needs it and it leaked out through an over-broad `pub`, it becomes
   `pub(crate)`.
3. If a whole module is sibling-only and likely to change before 1.0, the module gets
   `#[doc(hidden)]` instead of each item in it.

So the rustdoc at <https://docs.rs/sigilbuzz> is the source of truth for the public
API. If you can't see it there, it may break.

## Lints

`missing_docs = warn` applies to the whole workspace, so every `pub` item needs a doc
comment. A missing one shows up as a warning and then fails CI. The lint settings live
in `[workspace.lints]` in the root `Cargo.toml`, and each crate opts in with
`lints.workspace = true`.

`clippy::missing_docs_in_private_items` is allowed on purpose. Docs on private helpers
are welcome but not part of the contract.

## Adding a public item

1. Add it with `pub` and a real doc comment: at least a one-line summary, and an
   example when the signature is not obvious. `missing_docs` catches a missing
   comment.
2. If only sibling crates need it, put `#[doc(hidden)]` on the item or its module, and
   note which sibling uses it.
3. If it belongs in the 1.0 API, add it to the right tier on this page. Reviews check
   for the entry.
4. If it is experimental, say so in its rustdoc. Better yet, keep it behind a cargo
   feature or out of the public API until it settles.

## Removing or renaming a public item

Before 1.0, a public item can be renamed or removed without a deprecation release, but
only in a minor version bump and only with an entry in
[CHANGELOG.md](../CHANGELOG.md). Breaking a Tier 1 item should be a deliberate choice,
called out in the pull request, and checked against the known consumers listed below.

After 1.0:

- Tier 1 items follow normal semver. A breaking change needs a major version and at
  least one release with the old item marked `#[deprecated]`.
- Tier 2 items can still change in a minor release. An item moves up to Tier 1 when the
  `#[doc(hidden)]` attribute comes off, it gets listed here, and a release ships.

## Known consumers

- The main consumer is a text rendering application with an MSDF glyph generator.
  Before each release is tagged, it is built against the release candidate. It uses:
  - `sigilbuzz::{Blob, Buffer, Face, Font, Direction, Feature, shape}`
  - `sigilbuzz::tables::{Outline, PathOp, Fvar}`
  - `sigilbuzz_render::{Affine, DEFAULT_TOLERANCE, flatten}`

  These are all Tier 1 and have kept the same shape since 0.19. The MSDF generator
  flattens one `MoveTo` plus one drawing op at a time, so it relies on `flatten`
  leaving a contour without `Close` open: a closing edge would add a reversed duplicate
  of every edge it draws.
- A software rasterizer uses the outline and render APIs under the same Tier 1 terms.

If you build on sigilbuzz, open an issue or pull request that links to your usage so
future audits know what to protect.
