# Stability commitment

This document describes which sigilbuzz APIs are stable for downstream
consumers, which are explicitly experimental, and the rules sigilbuzz
follows when adding or changing public items.

## Status: pre-1.0, lockstep workspace

Every crate in the sigilbuzz workspace is below 1.0 and may break in a
minor version bump. The 12 crates ship together — the workspace cuts
one version per release, so consumers should pin compatible versions
across the family (e.g. `sigilbuzz = "0.19"`, `sigilbuzz-render =
"0.5"`, `sigilbuzz-paint = "0.1"`, all from the same workspace
release). See `RELEASING.md` for the per-release version-bump
checklist.

We aim to reach **1.0 in 2026** with a stable shaping API and a
documented bridge for downstream renderers (oniq, pixel-stroke, and
the broader Rust text ecosystem).

## What is "public" right now

Three categories, mirrored in the source via `#[doc(hidden)]`
annotations and `unstable_*` namespaces:

### Tier 1 — stable public API

These items are documented, exercised by external consumers, and will
follow the standard semver discipline once the workspace ships 1.0.
Deprecation will go through a normal `#[deprecated]` cycle; signature
changes will only happen on a major-version bump.

**`sigilbuzz` (root crate):**

- `Blob`, `Face`, `Font`, `Buffer`, `Glyph`, `Direction`, `Feature`
- `OwnedFace` — lifetime-free face for caching / cross-thread sharing
- `shape`
- `Face::glyph_outline`, `Face::glyph_outline_at_coords`,
  `Face::glyph_bounds`, `Face::name`, `Face::parse`, `Face::parse_bytes`
- `OwnedFace::parse`, `OwnedFace::as_face`, `OwnedFace::data`,
  `OwnedFace::data_arc`
- `Font::new`, `Font::with_coords`
- `tables::{Outline, OutlineSink, PathOp}` — outline iteration
- `tables::{Cmap, Glyf, Cff, Cff2, Head, Hhea, Hmtx, Maxp, Name,
  NameRecord}` — headline SFNT views
- `tables::{Colr, ColrPaint, Cpal, Color}` — colour-glyph surface
- `tables::{Fvar, VariationAxis, Avar, Hvar, Vvar, Mvar}` — variations
- `tables::tag::*` — table tag constants
- `Error`, `Result`, `VERSION`
- **Promoted in 0.20.0** (audit follow-up #235 — curated re-exports
  of items previously reachable only through the `#[doc(hidden)]`
  `ot::*` and `unicode::*` deep paths):
  - `feature` — OpenType feature-tag byte-literal constants
    (re-export of `ot::feature`).
  - `JoiningForm` — Arabic joining-form enum (re-export of
    `ot::arabic::JoiningForm`).
  - `UnicodeScript` — coarse script bucket (re-export of
    `unicode::Script`, renamed to leave room for a future
    `ot::Script` script-tag enum).
  - `script_of`, `is_hangul_jamo` — char-to-script classifiers
    (re-exports of `unicode::script_of` /
    `unicode::is_hangul_jamo`).
  - `BidiInfo` — UAX #9 result type (re-export of
    `unicode::bidi::BidiInfo`).
  - `BidiClass`, `bidi_class` — UCD `Bidi_Class` enum + lookup
    (re-exports of `unicode::bidi_class::*`).
  - `JoiningType` — Arabic / Mongolian joining-type enum
    (re-export of `unicode::joining::JoiningType`).
  The deep `ot::*` and `unicode::*` paths remain `#[doc(hidden)]`
  Tier-2; only the names listed above carry the Tier-1 stability
  commitment.

**`sigilbuzz-render`:**

- `Rasterizer`, `Pixmap`, `ColorPixmap`, `RenderError`
- `Affine`, `flatten`, `flatten_grouped`, `FlattenedCurve`, `Segment`,
  `DEFAULT_TOLERANCE`
- `encode_png`, `encode_png_alpha`
- `decode_ebdt_mono`, `decode_png`, `rasterize_bitmap_glyph`,
  `rescale_bilinear`

**`sigilbuzz-paint`:**

- `evaluate`, `evaluate_at_coords`, `DrawCmd`, `GlyphId`, `PaintSource`
- `Color`, `Gradient`, `GradientKind`, `ColorStop`, `Extend`,
  `Transform2D`, `CompositeMode`

**`sigilbuzz-gpu`:**

- `encode_glyph`, `encode_glyph_at_coords`, `SlugOptions`
- `SlugGlyph`, `Band`, `QuadSegment`, `Bbox`, `Vec2`

**`sigilbuzz-pdf`:**

- `emit_type3_font`, `emit_type1_font`, `emit_otf_embedded_font`
- `Type1Font`, `OtfEmbeddedFont`
- `emit_d1_prologue`, `emit_fill_epilogue`, `emit_path_ops`,
  `outline_bbox`

**`sigilbuzz-svg`:**

- The full crate surface: outline-to-SVG and (with `--features color`)
  COLRv1-to-SVG emission.

**`sigilbuzz-subset`:**

- `subset`, `SubsetInput`, `SubsetOutput`
- `instance`, `AxisPin`, `F2Dot14`, `InstanceInput`, `InstancedOutput`
- `compute_closure`, `subset_cff1_non_identity`,
  `subset_cff2_non_identity`, `emit_classdef`,
  `emit_coverage_from_glyphs`, `emit_coverage_from_pairs`

**`sigilbuzz-woff`:**

- `unwrap_woff1`, `wrap_woff1`, `wrap_woff1_with_options`,
  `WrapWoff1Options`
- `unwrap_woff2`, `wrap_woff2`, `wrap_woff2_with_options`,
  `WrapOptions`
- `WoffError`, `Result`

**`sigilbuzz-text-layout`:**

- `line_break_opportunities`, `LineBreakIter`, `BreakOpportunity`,
  `LineBreakClass`
- `wrap_lines`, `LineRange`, `WrapOptions`
- `word_breaks`

**`sigilbuzz-hyphen`:**

- `hyphenate`, `Patterns`, `ParseError`, `Language`
- `break_opportunities_with_hyphens`, `HyphenatedBreak`
  (under the `text-layout-integration` feature)

**`sigilbuzz-capi`:**

- The C ABI surface declared in `include/hb.h`. Symbol-compatible with
  HarfBuzz; tracked via the `tests/c_link.rs` link-time check.

### Tier 2 — public but experimental (`#[doc(hidden)]`)

These items are technically reachable from outside the crate (because
a sibling workspace crate needs them) but are not part of the
stability commitment. They are hidden from rustdoc to discourage
downstream pinning. Their shape may change in any minor release.

- `sigilbuzz::ot::*` — script-shaping internals: Arabic, Indic, USE,
  Mongolian, Tibetan state machines plus feature tag constants. The
  shaper API is `shape()`; the per-script machinery is not part of
  the public surface. As of 0.20.0 the consumer-facing subset
  (`feature` constants module, `arabic::JoiningForm`) is re-exported
  at crate root and *is* Tier-1 — see the `sigilbuzz` (root crate)
  list above.
- `sigilbuzz::unicode::*` — Unicode property tables. As of 0.20.0
  the consumer-facing subset is re-exported at crate root and *is*
  Tier-1 (`UnicodeScript`, `script_of`, `is_hangul_jamo`,
  `BidiInfo`, `BidiClass`, `bidi_class`, `JoiningType`); the
  remaining `unicode::*` deep paths (bidi-class table internals,
  indic / use category tables, normalization helpers, bidi-bracket
  pairs, joining-type lookup table) stay Tier-2 because the UCD
  data continues to evolve UCD-version by UCD-version.
- `sigilbuzz::tables::{ankr, avar, base, cbdt, cblc, cff, cff2, cmap,
  ebdt, eblc, fvar, gdef, glyf, gpos, gsub, gvar, head, hhea, hmtx,
  hvar, kern, kerx, layout, loca, math, maxp, morx, multi_var_store,
  mvar, name, parse, sbix, svg_table, varc, variation_store, vhea,
  vmtx, vorg, vvar}` — per-table parser submodules. The headline
  types are re-exported at `tables::` root and *are* stable. Helper
  types reachable only through the deep submodule path (e.g.
  `tables::layout::FeatureList`, `tables::gpos::ChainContextPos`) are
  not.

### Tier 3 — internal (private or `pub(crate)`)

Anything inside a `mod` declaration without `pub` is internal. Each
crate carries a flat set of private modules under `src/`; `pub use`
in `lib.rs` is the gate that determines what consumers see. Items
that should not have escaped the crate but did get demoted to
`pub(crate)` during this audit; sibling-crate-only items get
`#[doc(hidden)]` rather than demotion (demotion would break the
sibling).

## Doc-hidden policy

`#[doc(hidden)]` is the audit's preferred tool for marking items
"public for technical reasons, not part of the contract." The semantic
meaning here is:

> This item compiles and links from outside the crate. We make no
> promise that its name, signature, or behaviour will survive the next
> minor release. Pin against it at your own risk.

Concretely:

1. If a sibling workspace crate (`sigilbuzz-render`, `sigilbuzz-subset`,
   etc.) needs an item, the item stays `pub` but earns
   `#[doc(hidden)]`.
2. If no sibling needs the item and it leaked through over-broad `pub`,
   it gets demoted to `pub(crate)`.
3. If an item is a sibling-internal *and* deeply experimental
   (subject to redesign before 1.0), the parent module gets
   `#[doc(hidden)]` rather than annotating each leaf.

The visible rustdoc at <https://docs.rs/sigilbuzz> is therefore the
source of truth for the public API. Anything not visible there is
fair game to break.

## Lints

The workspace enforces `missing_docs = warn` (rustc) workspace-wide:
every `pub` item must carry a doc comment. New items without
documentation surface as build warnings, then trip CI. The `[lints]`
block is hoisted to `[workspace.lints]` in the root `Cargo.toml` and
inherited by each crate via `lints.workspace = true`.

`clippy::missing_docs_in_private_items` is intentionally **allowed**:
private helper documentation is best-effort, not part of the contract.

## Adding a new public item

1. Add the item with `pub` and a real doc comment (one-line summary
   minimum; rustdoc with example for non-trivial signatures). The
   workspace `missing_docs = warn` will catch a missing comment.
2. If only sibling crates need the item, put `#[doc(hidden)]` on it
   *or* on the enclosing module, and note which sibling consumes it.
3. If the item is part of the eventual 1.0 surface, add it to the
   matching tier in this document. Reviews will check the entry.
4. If the item is genuinely experimental and may change in the next
   release, place it under an `unstable_*` namespace or document the
   experimental status in its rustdoc. Prefer not to ship truly
   unstable items at all — use a feature gate or an internal testbed.

## Removing or renaming a public item

Pre-1.0 we may rename or remove without a deprecation cycle, but only
in a minor version bump and only with a CHANGELOG entry under the
release that ships the change. Tier-1 items in this document warrant
extra scrutiny: a Tier-1 break should be intentional, called out in
the PR description, and accompanied by a test in any internal
consumer (oniq) that exercises the new shape.

Post-1.0:

- Tier-1 items follow standard semver: breaking changes require a
  major version bump and at least one deprecation release.
- Tier-2 items (`#[doc(hidden)]`) may continue to break in minor
  releases. Promotions out of Tier-2 happen by deleting the
  `#[doc(hidden)]` attribute, adding the item to this document, and
  cutting a release.

## Consumers we coordinate with

- **oniq** (`/Users/shon/repos/oniq`) is the headline downstream
  consumer. Every release runs the oniq build against the candidate
  branch before tagging. The oniq imports are:
  - `sigilbuzz::{Blob, Buffer, Face, Font, Direction, Feature, shape}`
  - `sigilbuzz::tables::{Outline, PathOp, Fvar}`
  - `sigilbuzz_render::{Affine, DEFAULT_TOLERANCE, flatten}`
  These are all Tier-1 and frozen in shape from 0.19 onward.
- **pixel-stroke** (`/Users/shon/repos/pixel-stroke`) consumes the
  outline + render surface for software rasterization. Same Tier-1
  contract.

If you are adding a sigilbuzz consumer, file an issue or PR linking
your usage so future audits know what to protect.
