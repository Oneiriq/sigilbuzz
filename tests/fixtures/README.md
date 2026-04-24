# Test fixtures

Fonts used by the integration tests.

- `opensans_regular.ttf` — Open Sans Regular by Steve Matteson, Apache-2.0.
  Mirrored here so integration tests have a predictable fixture without
  depending on a sibling repository layout.
- `rubik_vf.ttf` — Rubik Variable (wght axis only) by Philipp Hubert &
  Sebastian Fischer, OFL-1.1. Sourced from the `googlefonts/rubik`
  repository. Used by `tests/variable_fonts.rs` to exercise the
  fvar → avar → HVAR advance-delta pipeline and `gvar` outline deltas
  against rustybuzz's output at the same axis coordinate.
