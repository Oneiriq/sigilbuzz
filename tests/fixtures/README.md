# Test fixtures

Fonts used by the integration tests.

- `opensans_regular.ttf` — Open Sans Regular by Steve Matteson, Apache-2.0.
  Mirrored here so integration tests have a predictable fixture without
  depending on a sibling repository layout.
- `amiri_regular.ttf` — Amiri Regular by Khaled Hosny, OFL 1.1. Used for
  Arabic cursive-joining parity tests against rustybuzz. Version 1.001
  (2024-11-19), mirrored from the upstream release.
- `rubik_vf.ttf` — Rubik Variable (wght axis only) by Philipp Hubert &
  Sebastian Fischer, OFL-1.1. Sourced from the `googlefonts/rubik`
  repository. Used by `tests/variable_fonts.rs` to exercise the
  fvar → avar → HVAR advance-delta pipeline and `gvar` outline deltas
  against rustybuzz's output at the same axis coordinate.
- `var_kern.ttf` — Synthetic 972-byte variable font with two glyphs
  ("A", "V"), one `wght` axis (400..900), and a GPOS kern pair whose
  x_advance delta is -100 at wght=900 and 0 at wght=400 via a
  VariationIndex / ItemVariationStore pair. Built deterministically
  from `tests/tools/build_var_kern_fixture.py` so the fixture is
  reproducible; used by `tests/variable_kern.rs` to exercise the GPOS
  feature-variation wiring added for issue #13.
- `cbdt_synthetic.ttf` — Synthetic 860-byte font with one CBDT/CBLC
  strike at 32 ppem, one PNG-tagged bitmap glyph, and a CBLC index
  sub-table in format 1 (variable-metric, u32 offsets) plus a CBDT
  record in format 17 (small metrics + PNG data). Built deterministically
  from `tests/tools/build_cbdt_fixture.py`; used by
  `tests/bitmap_fonts.rs` to drive the CBDT/CBLC parsers and the
  unified `Face::glyph_bitmap` accessor on a real `Face`. The PNG is
  a 67-byte 1×1 transparent image so the fixture stays well under
  5 KB. Public-domain / no third-party content.
- `sbix_synthetic.ttf` — Synthetic 784-byte font carrying an Apple
  `sbix` table at version 1 with one strike at 32 ppem, two glyph
  slots (gid 0 empty, gid 1 = PNG). Built deterministically from
  `tests/tools/build_sbix_fixture.py`; used by
  `tests/bitmap_fonts.rs` to drive the sbix parser and the
  `Face::glyph_bitmap` accessor on the sbix path. Public-domain /
  no third-party content.
- `phantom_anchor.ttf` — Synthetic 780-byte fixture with four glyphs
  (`.notdef`, `base`, `mark`, `combo`). `combo` is a composite with one
  XY-mode component and one *anchor-mode* component whose `arg1` lands
  in the parent's phantom-point range (pp2, the advance-width origin).
  Hand-crafted because no font in the existing OFL corpus exercises
  the phantom-anchor branch added in PR #80. Built deterministically
  from `tests/tools/build_phantom_anchor_fixture.py`; used by
  `tests/outline_parity.rs` to drive the phantom-resolution path on a
  real `Face`. Public-domain / no third-party content (entirely
  synthesised at build time).
