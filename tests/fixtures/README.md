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
- `var_kern.ttf`: a synthetic 1012-byte variable font with three glyphs ("A", "V", and an
  unkerned "B" that lets a subset drop a glyph), one `wght` axis (400 to 900), and a GPOS
  kern pair whose `x_advance` delta is -100 at wght=900 and 0 at wght=400, through a
  VariationIndex and ItemVariationStore. Built deterministically by
  `tests/tools/build_var_kern_fixture.py`. `tests/variable_kern.rs` uses it to exercise
  the GPOS feature-variation support added for issue #13.
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
