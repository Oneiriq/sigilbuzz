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
- `NotoSansBrahmi-Regular.ttf` — Source:
  <https://github.com/notofonts/notofonts.github.io/tree/main/fonts/NotoSansBrahmi/hinted/ttf>.
  Brahmi historical script (U+11000..U+1107F). The font ships only
  a `ccmp` GSUB feature — no positional / topographical features —
  so the parity corpus exercises segmentation + cluster handling
  rather than rich GSUB substitution.
- `NotoSansSharada-Regular.ttf` — Source:
  <https://github.com/notofonts/notofonts.github.io/tree/main/fonts/NotoSansSharada/hinted/ttf>.
  Sharada historical script (U+11180..U+111DF). Ships `akhn`,
  `abvs`, and `blws` lookups; sign-i (U+111B4) is a spacing pre-
  base glyph that the USE reorder pass moves before the base.
- `NotoSansKhojki-Regular.ttf` — Source:
  <https://github.com/notofonts/notofonts.github.io/tree/main/fonts/NotoSansKhojki/hinted/ttf>.
  Khojki historical script (U+11200..U+1124F). Advertises lookups
  under both `khoj` and `gujr`; sigilbuzz routes via `khoj` first.
- `NotoSansTirhuta-Regular.ttf` — Source:
  <https://github.com/notofonts/notofonts.github.io/tree/main/fonts/NotoSansTirhuta/hinted/ttf>.
  Tirhuta historical script (U+11480..U+114DF). Ships `rphf`,
  `abvf`, `blwf`, `pstf` and topographical lookups; sign-e
  (U+114B9) and sign-o (U+114BC) are pre-base vowel signs that
  the USE pre-base reorder fires for.
- `NotoSansModi-Regular.ttf` — Source:
  <https://github.com/notofonts/notofonts.github.io/tree/main/fonts/NotoSansModi/hinted/ttf>.
  Modi historical script (U+11600..U+1165F). Ships `rphf`, `half`,
  `pres`, and `calt` lookups.
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

## CFF subsetting fixtures

Real-font CFF1 + CFF2 fixtures used by the round-trip integration
tests in `crates/sigilbuzz-subset/tests/real_cff_round_trip.rs`. They
back the synthetic-only caveat in #120, #135, and #138 with real OFL
font bytes.

- `SourceCodePro-Latin-Subset.otf` — CFF1 (non-CID). Source: Adobe's
  upstream `SourceCodePro-Regular.otf` from
  <https://github.com/adobe-fonts/source-code-pro/raw/release/OTF/SourceCodePro-Regular.otf>.
  License: SIL Open Font License 1.1 (Adobe, "Source" reserved font
  name). Subset generated with `fonttools subset` to printable ASCII
  to keep the fixture small enough to vendor:

      python3 -m fontTools.subset SourceCodePro-Regular.otf \
          --unicodes='U+0020-007E' \
          --output-file=SourceCodePro-Latin-Subset.otf \
          --no-hinting --desubroutinize \
          --drop-tables+=GSUB,GPOS,GDEF,FFTM,DSIG \
          --no-layout-closure
- `SourceSans3VF-Latin-Subset.otf` — CFF2 + variable font (single
  `wght` axis spanning 200..900). Source: Adobe's
  `SourceSans3VF-Upright.otf` from
  <https://github.com/adobe-fonts/source-sans/raw/release/VF/SourceSans3VF-Upright.otf>.
  License: SIL Open Font License 1.1 (Adobe, "Source" reserved font
  name). Subset generated with `fonttools subset` to printable ASCII
  while preserving the `fvar` / `avar` / `HVAR` axis machinery so the
  round-trip can verify variation behaviour survives:

      python3 -m fontTools.subset SourceSans3VF-Upright.otf \
          --unicodes='U+0020-007E' \
          --output-file=SourceSans3VF-Latin-Subset.otf \
          --no-hinting \
          --drop-tables+=DSIG,STAT,MVAR,BASE
