# Test fixtures

Fonts used by the integration tests.

- `opensans_regular.ttf`: Open Sans Regular by Steve Matteson, Apache-2.0. Copied here
  so the integration tests have a predictable fixture without depending on the layout
  of a sibling repository.
- `amiri_regular.ttf`: Amiri Regular by Khaled Hosny, OFL 1.1. Used for the Arabic
  cursive-joining parity tests against rustybuzz. Version 1.001 (2024-11-19), copied
  from the upstream release.
- `rubik_vf.ttf`: Rubik Variable (`wght` axis only) by Philipp Hubert and Sebastian
  Fischer, OFL 1.1, from the `googlefonts/rubik` repository. `tests/variable_fonts.rs`
  uses it to exercise the `fvar`, `avar`, and `HVAR` advance-delta pipeline and `gvar`
  outline deltas against HarfBuzz's output at the same axis coordinate, and
  `tests/anchor_variations.rs` its variable mark anchors. Both read HarfBuzz 14.5.0's
  output from `rubik_variable_shaping.expected`; regenerate that file with `uv run
  --no-project --with uharfbuzz==0.56.2 python tests/tools/variable_shaping_expected.py`.
  `tests/feature_variations_gsub_parity.rs` uses its GSUB 1.1 FeatureVariations, which
  give `rvrn` heavier currency signs from `wght` 500 on.
- `hahmlet_gvar_subset.ttf`: a 7,908-byte subset of Hahmlet Variable (`wght` 100 to
  900, default 400), version 1.002, Copyright 2020 The Hahmlet Project Authors, SIL Open
  Font License 1.1. Source: <https://github.com/google/fonts/raw/main/ofl/hahmlet/Hahmlet%5Bwght%5D.ttf>
  (SHA-256 `892bffe530255770a7435226154a02f519055ff6bedf64254f37f21d15a59279`), license at
  <https://github.com/google/fonts/blob/main/ofl/hahmlet/OFL.txt>. It keeps the space,
  `O`, `Á`, `Å`, U+3143 and the syllables U+BE60, U+BE75 and U+BED0, plus the components
  `Á` and `Å` need. Its `gvar` has simple glyphs whose tuples list only some points,
  composites whose components move (one through a sparse tuple) and take their metrics
  from a `USE_MY_METRICS` component, and a space whose advance moves through its phantom
  points, which no other fixture has together. Built with this repo's `sigilbuzz subset`
  in two passes, since the closure of the first pass still follows the GPOS mark
  attachments of the accents into every base:

      cargo run --release -p sigilbuzz-cli -- subset 'Hahmlet[wght].ttf' nolayout.ttf \
          --unicodes "U+0020,O,U+00C1,U+00C5,U+3143,U+BE60,U+BE75,U+BED0" --drop-layout
      cargo run --release -p sigilbuzz-cli -- subset nolayout.ttf hahmlet_gvar_subset.ttf \
          --unicodes "U+0020,O,U+00C1,U+00C5,U+3143,U+BE60,U+BE75,U+BED0" --drop-layout

  `tests/gvar_iup_parity.rs` checks outlines, extents and advances against
  `hahmlet_gvar_subset.expected`, HarfBuzz 14.5.0's output at five weights. Regenerate
  that file with `uv run --no-project --with uharfbuzz==0.56.2 python
  tests/tools/gvar_iup_expected.py`.
- `var_kern.ttf`: a synthetic 768-byte variable font with three glyphs ("A", "V", and an
  unkerned "B" that lets a subset drop a glyph), one `wght` axis (400 to 900), and a GPOS
  kern pair whose `x_advance` delta is -100 at wght=900 and 0 at wght=400, through a
  VariationIndex and ItemVariationStore. The PairValueRecord's device offset is measured
  from its PairSet, as the OpenType spec requires. Built deterministically by the
  `fixture` module in `tests/variable_kern.rs`. Regenerate it with `cargo test --test
  variable_kern -- --ignored regenerate_var_kern_fixture`. A test fails when the
  committed file drifts from the builder. `tests/variable_kern.rs` uses it to exercise the
  GPOS feature-variation support added for issue #13, and
  `crates/sigilbuzz-subset/tests/variable_round_trip.rs` uses it to check that a subset
  that drops a glyph keeps the kern variation.
- `cbdt_synthetic.ttf`: a synthetic 860-byte font with one CBDT/CBLC strike at 32 ppem,
  one PNG bitmap glyph, a CBLC index subtable in format 1 (variable metrics, u32
  offsets), and a CBDT record in format 17 (small metrics plus PNG data). Built
  deterministically by `tests/tools/build_cbdt_fixture.py`. `tests/bitmap_fonts.rs` uses
  it to drive the CBDT/CBLC parsers and the `Face::glyph_bitmap` accessor on a real
  `Face`. The PNG is a 67-byte 1x1 transparent image, so the fixture stays well under
  5 KB. Public domain, no third-party content.
- `sbix_synthetic.ttf`: a synthetic 784-byte font with an Apple `sbix` table (version
  1), one strike at 32 ppem, and two glyph slots (gid 0 empty, gid 1 a PNG). Built
  deterministically by `tests/tools/build_sbix_fixture.py`. `tests/bitmap_fonts.rs` uses
  it to drive the sbix parser and the `Face::glyph_bitmap` accessor on the sbix path.
  Public domain, no third-party content.
- `math_synthetic.ttf`: a synthetic 876-byte font with four glyphs and a hand-built
  OpenType `MATH` table that exercises every subtable parser: `MathConstants` (51
  MathValueRecords with realistic values), `MathGlyphInfo` (italic correction, top
  accent, extended-shape coverage, and per-corner kern info), and `MathVariants` (one
  vertical glyph construction with two progressive variants and a 3-part assembly).
  Real math fonts (STIX 2 Math, Latin Modern Math, Asana Math) are 150 KB to 700 KB
  before subsetting, too heavy to include for one integration test. Built
  deterministically by `tests/tools/build_math_fixture.py`. Used by
  `tests/math_fixture.rs`. Public domain, no third-party content.
- `phantom_anchor.ttf`: a synthetic 780-byte font with four glyphs (`.notdef`, `base`,
  `mark`, `combo`). `combo` is a composite with one XY-mode component and one
  anchor-mode component whose `arg1` points into the parent's phantom-point range (pp2,
  the advance-width origin). Built by hand because no font in the existing OFL set
  exercises the phantom-anchor code added in PR #80. Built deterministically by
  `tests/tools/build_phantom_anchor_fixture.py`. `tests/outline_parity.rs` uses it to
  drive phantom-point resolution on a real `Face`. Public domain, no third-party
  content (generated entirely at build time).
- `attach_chain.ttf`: a synthetic 1,628-byte font with five bases, the ligature `f_i`,
  and the combining marks U+0300 to U+0303. Its GPOS stacks a mark on a mark before
  that mark attaches, joins `b` cursively with the RightToLeft flag, attaches marks to
  bases, ligatures and marks, and then moves the bases with a `blwm` lookup. No other
  fixture has a lookup that moves a base after its mark attached. Built
  deterministically by `tests/tools/build_attach_chain_fixture.py` with fontTools.
  `tests/attach_offsets_parity.rs` uses it. SIL Open Font License 1.1, no third-party
  content.
- `stage_order.ttf`: a synthetic 2,092-byte font with the Latin letters a, b, c, e and
  f, the Arabic letters beh, lam and alef, and alternate glyphs. Each GSUB lookup makes
  one substitution, and the lookups of `ccmp`, `ltra`, `liga`, `calt`, `smcp`, `vert`,
  the joining features and `mset` interleave by lookup index, so the result shows the
  order the features ran in. No other fixture has lookups that interleave across
  features. Built deterministically by `tests/tools/build_stage_order_fixture.py` with
  fontTools. `tests/stage_order_parity.rs` uses it. SIL Open Font License 1.1, no
  third-party content.
- `noto_sans_cjk_jp_uvs_subset.otf`: an 11 KB subset of Noto Sans CJK JP Regular
  (version 2.004) by Adobe and the Noto Project Authors, SIL Open Font License 1.1
  (reserved font name "Source", which the subset does not use). Source:
  <https://github.com/notofonts/noto-cjk/raw/main/Sans/OTF/Japanese/NotoSansCJKjp-Regular.otf>,
  license at <https://github.com/notofonts/noto-cjk/blob/main/Sans/LICENSE>. It keeps
  the font's cmap format 14 subtable (Unicode Variation Sequences) for a few base
  characters, with both default and non-default sequences for VS1, VS2, and VS17 to
  VS19, so the tests can exercise variation sequence lookups and shaping. No other
  fixture has a format 14 subtable. Built with fontTools 4.66.0 (`pyftsubset`):

      pyftsubset NotoSansCJKjp-Regular.otf \
          --unicodes="U+0020,U+0061,U+3001,U+3002,U+FF01,U+FF0C,U+845B,U+9089,U+6F22,U+4E08,U+8FBB,U+FE00,U+FE01,U+E0100,U+E0101,U+E0102" \
          --layout-features='*' --desubroutinize --no-hinting --name-IDs='*' \
          --notdef-outline --output-file=noto_sans_cjk_jp_uvs_subset.otf

  The selectors must be in `--unicodes`, or `pyftsubset` drops the format 14 records.
  `tests/cmap14_parity.rs` and the `sigilbuzz-capi` glyph lookup tests use it.
- `noto_sans_kr_vf_vertical_subset.otf`: a 10 KB, 16-glyph subset of Noto Sans KR
  Variable (version 2.004) by Adobe and the Noto Project Authors, SIL Open Font License
  1.1 (Copyright 2014-2021 Adobe, reserved font name "Source", which the subset does not
  use). Source:
  <https://github.com/notofonts/noto-cjk/raw/main/Sans/Variable/OTF/Subset/NotoSansKR-VF.otf>
  (git blob `1c59da9a18539f40f117a32e54f205a447f2815d`), license at
  <https://github.com/notofonts/noto-cjk/blob/main/Sans/LICENSE>. It is a CFF2 variable
  font (`wght` 100 to 900, default 100) that keeps `vhea`, `vmtx`, `VORG` and `VVAR`,
  including the `VVAR` vertical origin map, plus `BASE` and `STAT`, for the space, the
  corner brackets U+300C and U+300D, the ideographic comma and full stop, U+AC00, and
  U+2030 and U+2170, whose vertical origins vary with the weight. No other fixture has
  vertical metrics or their variations.
  `crates/sigilbuzz-subset/tests/vertical_round_trip.rs` and the CLI tests use it. Cut
  with the sigilbuzz subsetter itself (no fontTools needed), from the repository root:

      cargo run --release -p sigilbuzz-cli -- subset NotoSansKR-VF.otf \
          tests/fixtures/noto_sans_kr_vf_vertical_subset.otf \
          --unicodes 'U+0020,U+300C,U+300D,U+3001,U+3002,U+AC00,U+2030,U+2170'

  `tests/variable_vertical_parity.rs` checks its top-to-bottom runs against
  `vertical_shaping.expected`, HarfBuzz 14.5.0's output at six weights. That file also
  holds HarfBuzz's top-to-bottom runs of `hahmlet_gvar_subset.ttf` and `rubik_vf.ttf`
  (varied glyph boxes, no `vmtx`), of `hahmlet_gvar_subset.ttf` with a hand-built
  `vhea`, `vmtx` and `gvar` (varied top phantom points), and of the static fonts in
  `tests/vertical_shaping.rs`. Regenerate it with `uv run --no-project --with
  uharfbuzz==0.56.2 python tests/tools/vertical_shaping_expected.py`.

- `../fonts/NotoSansKR-HangulTone-Subset.ttf`: an 8 KB subset of Noto Sans KR (OFL 1.1,
  Copyright 2014-2021 Adobe, Reserved Font Name 'Source'), from
  <https://github.com/google/fonts/tree/main/ofl/notosanskr>, instanced at Regular and
  cut to the Hangul tone marks, the dotted circle, and a few jamo and syllables. It sits
  with the other Noto fonts in `tests/fonts/`, whose README has the exact fontTools
  commands. `tests/hangul_tone_harfbuzz_parity.rs` uses it, since no other fixture has
  the tone marks.
- `../fonts/NotoSansDevanagari-Dev3-Subset.ttf`: a 41 KB subset of Noto Sans Devanagari
  (OFL 1.1, the Noto Project Authors) whose `dev2` script records are renamed `dev3`, so
  `tests/indic3_parity.rs` can check that a font with the Indic3 tags gets the Universal
  Shaping Engine, as in HarfBuzz. No released font uses those tags yet. It sits with the
  other Noto fonts in `tests/fonts/`, whose README has the exact fontTools steps. The
  subsets of Noto Sans Javanese, Chakma, Khudawadi, Takri, Syriac, and Adlam that
  `tests/script_coverage_parity.rs` uses are listed there too.
- `noto_sans_kr_vf_cff2_subset.otf`: a 4,480-byte subset of Noto Sans KR VF (version
  2.004) by Adobe, SIL Open Font License 1.1 (reserved font name "Source", which the
  subset does not use). Source:
  <https://github.com/notofonts/noto-cjk/raw/main/Sans/Variable/OTF/Subset/NotoSansKR-VF.otf>
  (SHA-256 `e647f53b18a4823647a51bcbdac866617701a4dd8ce495bac2db5ecea88d8f21`), license
  at <https://github.com/notofonts/noto-cjk/blob/main/Sans/LICENSE>. It keeps the
  `CFF2` outlines, the `wght` axis and the variation tables for `.notdef` and the
  syllables 귁 (U+ADC1) and 빛 (U+BE5B), whose last contours end away from their start
  points. CFF2 charstrings have no `endchar`, so the last contour of every glyph used
  to stay open, and a fill drew streaks across those two syllables. Built with this
  repository's CLI:

      sigilbuzz subset NotoSansKR-VF.otf noto_sans_kr_vf_cff2_subset.otf \
          --unicodes "U+ADC1,U+BE5B" --drop-layout

  `tests/cff2_closed_contours.rs` uses it.
