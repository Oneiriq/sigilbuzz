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
  outline deltas against rustybuzz's output at the same axis coordinate.
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
- `../fonts/NotoSansKR-HangulTone-Subset.ttf`: an 8 KB subset of Noto Sans KR (OFL 1.1,
  Copyright 2014-2021 Adobe, Reserved Font Name 'Source'), from
  <https://github.com/google/fonts/tree/main/ofl/notosanskr>, instanced at Regular and
  cut to the Hangul tone marks, the dotted circle, and a few jamo and syllables. It sits
  with the other Noto fonts in `tests/fonts/`, whose README has the exact fontTools
  commands. `tests/hangul_tone_harfbuzz_parity.rs` uses it, since no other fixture has
  the tone marks.
