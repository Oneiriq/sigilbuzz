# Complex-script test fixtures

Fonts used by the complex-script shaper integration tests. All
vendored under the SIL Open Font License, Version 1.1 —
redistribution is permitted so long as the license notice travels
with the file. The fonts are unmodified upstream builds.

- `NotoSansDevanagari-Regular.ttf` — Noto Sans Devanagari Regular by
  the Noto Project Authors. SIL OFL 1.1. Source:
  <https://github.com/notofonts/notofonts.github.io/tree/main/fonts/NotoSansDevanagari/hinted/ttf>.
  The full license text lives in the upstream repository's
  `OFL.txt`; sigilbuzz re-uses the font under its reserved-font-name
  exemption since this copy is unmodified.

- `NotoSansHebrew-Regular.ttf` — Noto Sans Hebrew Regular by the
  Noto Project Authors. SIL OFL 1.1. Source:
  <https://github.com/notofonts/notofonts.github.io/tree/main/fonts/NotoSansHebrew/hinted/ttf>.
  Same re-use terms as above. The font ships GDEF/GPOS tables
  covering Hebrew niqqud (mark-to-base) and cantillation
  (mark-to-mark) so the parity fixture exercises the full stack.

- `NotoSansKhmer-Regular.ttf` — Noto Sans Khmer Regular by the Noto
  Project Authors. SIL OFL 1.1. Source:
  <https://github.com/notofonts/notofonts.github.io/tree/main/fonts/NotoSansKhmer/hinted/ttf>.
  Same re-use terms as above.
