# Complex-script test fixtures

Fonts used by the complex-script shaper integration tests. All
vendored under the SIL Open Font License, Version 1.1 —
redistribution is permitted so long as the license notice travels
with the file. The fonts are unmodified upstream builds.

All Noto Sans fonts below are from the Noto Project Authors and
carry the upstream OFL 1.1 license; each is reused under its
reserved-font-name exemption because sigilbuzz ships the files
unmodified. The OFL text lives in the upstream repository's `OFL.txt`.

- `NotoSansDevanagari-Regular.ttf` — Source:
  <https://github.com/notofonts/notofonts.github.io/tree/main/fonts/NotoSansDevanagari/hinted/ttf>.
- `NotoSansHebrew-Regular.ttf` — Source:
  <https://github.com/notofonts/notofonts.github.io/tree/main/fonts/NotoSansHebrew/hinted/ttf>.
  The font ships GDEF/GPOS tables covering Hebrew niqqud
  (mark-to-base) and cantillation (mark-to-mark) so the parity
  fixture exercises the full stack.
- `NotoSansKhmer-Regular.ttf` — Source:
  <https://github.com/notofonts/notofonts.github.io/tree/main/fonts/NotoSansKhmer/hinted/ttf>.
- `NotoSansBengali-Regular.ttf` — Source:
  <https://github.com/notofonts/NotoSansBengali/tree/main/fonts/ttf/hinted/instance_ttf>.
- `NotoSansGurmukhi-Regular.ttf` — Source:
  <https://github.com/notofonts/NotoSansGurmukhi/tree/main/fonts/ttf/hinted/instance_ttf>.
- `NotoSansGujarati-Regular.ttf` — Source:
  <https://github.com/notofonts/NotoSansGujarati/tree/main/fonts/ttf/hinted/instance_ttf>.
- `NotoSansOriya-Regular.ttf` — Source:
  <https://github.com/notofonts/NotoSansOriya/tree/main/fonts/ttf/unhinted/instance_ttf>.
- `NotoSansTamil-Regular.ttf` — Source:
  <https://github.com/notofonts/NotoSansTamil/tree/main/fonts/ttf/hinted/instance_ttf>.
- `NotoSansTelugu-Regular.ttf` — Source:
  <https://github.com/notofonts/NotoSansTelugu/tree/main/fonts/ttf/hinted/instance_ttf>.
- `NotoSansKannada-Regular.ttf` — Source:
  <https://github.com/notofonts/NotoSansKannada/tree/main/fonts/ttf/hinted/instance_ttf>.
- `NotoSansMalayalam-Regular.ttf` — Source:
  <https://github.com/notofonts/NotoSansMalayalam/tree/main/fonts/ttf/hinted/instance_ttf>.
- `NotoSansSinhala-Regular.ttf` — Source:
  <https://github.com/notofonts/NotoSansSinhala/tree/main/fonts/ttf/hinted/instance_ttf>.
- `NotoSansMyanmar-Regular.ttf` — Source:
  <https://github.com/notofonts/notofonts.github.io/tree/main/fonts/NotoSansMyanmar/hinted/ttf>.
- `NotoSansThai-Regular.ttf` — Source:
  <https://github.com/notofonts/notofonts.github.io/tree/main/fonts/NotoSansThai/hinted/ttf>.
- `NotoSansLao-Regular.ttf` — Source:
  <https://github.com/notofonts/notofonts.github.io/tree/main/fonts/NotoSansLao/hinted/ttf>.
- `NotoSansNKo-Regular.ttf` — Source:
  <https://github.com/notofonts/notofonts.github.io/tree/main/fonts/NotoSansNKo/hinted/ttf>.
- `NotoSansBuginese-Regular.ttf` — Source:
  <https://github.com/notofonts/notofonts.github.io/tree/main/fonts/NotoSansBuginese/hinted/ttf>.
- `NotoSansTaiTham-Regular.ttf` — Source:
  <https://github.com/notofonts/notofonts.github.io/tree/main/fonts/NotoSansTaiTham/hinted/ttf>.
- `NotoSansBalinese-Regular.ttf` — Source:
  <https://github.com/notofonts/notofonts.github.io/tree/main/fonts/NotoSansBalinese/hinted/ttf>.
- `NotoSansSundanese-Regular.ttf` — Source:
  <https://github.com/notofonts/notofonts.github.io/tree/main/fonts/NotoSansSundanese/hinted/ttf>.
- `NotoSansLepcha-Regular.ttf` — Source:
  <https://github.com/notofonts/notofonts.github.io/tree/main/fonts/NotoSansLepcha/hinted/ttf>.
- `NotoSansLimbu-Regular.ttf` — Source:
  <https://github.com/notofonts/notofonts.github.io/tree/main/fonts/NotoSansLimbu/hinted/ttf>.
- `NotoSansCham-Regular.ttf` — Source:
  <https://github.com/notofonts/notofonts.github.io/tree/main/fonts/NotoSansCham/hinted/ttf>.
- `NotoSansOldHangul-Subset.ttf` — Subset of Noto Sans Korean variable
  font (weight axis collapsed to Regular) restricted to the Hangul
  Jamo blocks (U+1100..U+11FF, U+A960..U+A97F, U+D7B0..U+D7FF) plus a
  handful of precomposed modern Hangul syllables so the parity tests
  can also smoke-test the "don't break modern Hangul" invariant.
  Upstream: <https://github.com/google/fonts/tree/main/ofl/notosanskr>
  (`NotoSansKR[wght].ttf`). Subset generated with `fonttools subset`
  keeping layout features `ljmo`, `vjmo`, `tjmo`, `ccmp`, `calt`,
  `liga`, `locl` and dropping the variable (`gvar`/`fvar`/`avar`/
  `HVAR`/`STAT`) and vertical (`vhea`/`vmtx`) tables — the shaper
  only consumes the static Regular master in this fixture.
