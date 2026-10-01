# Complex-script test fonts

Fonts used by the complex-script shaping integration tests. All of them are included
under the SIL Open Font License, Version 1.1. Redistribution is permitted as long as the
license notice travels with the file. Unless noted below, the fonts are unmodified
upstream builds.

All the Noto Sans fonts below are from the Noto Project Authors and carry the upstream
OFL 1.1 license. Each is reused under its reserved-font-name exemption because
sigilbuzz ships the files unmodified. The OFL text lives in the upstream repository's
`OFL.txt`.

- `NotoSansDevanagari-Regular.ttf`. Source:
  <https://github.com/notofonts/notofonts.github.io/tree/main/fonts/NotoSansDevanagari/hinted/ttf>.
- `NotoSansHebrew-Regular.ttf`. Source:
  <https://github.com/notofonts/notofonts.github.io/tree/main/fonts/NotoSansHebrew/hinted/ttf>.
  The font ships GDEF and GPOS tables covering Hebrew niqqud (mark-to-base) and
  cantillation (mark-to-mark), so the parity test exercises the full stack.
- `NotoSansKhmer-Regular.ttf`. Source:
  <https://github.com/notofonts/notofonts.github.io/tree/main/fonts/NotoSansKhmer/hinted/ttf>.
- `NotoSansBengali-Regular.ttf`. Source:
  <https://github.com/notofonts/NotoSansBengali/tree/main/fonts/ttf/hinted/instance_ttf>.
- `NotoSansGurmukhi-Regular.ttf`. Source:
  <https://github.com/notofonts/NotoSansGurmukhi/tree/main/fonts/ttf/hinted/instance_ttf>.
- `NotoSansGujarati-Regular.ttf`. Source:
  <https://github.com/notofonts/NotoSansGujarati/tree/main/fonts/ttf/hinted/instance_ttf>.
- `NotoSansOriya-Regular.ttf`. Source:
  <https://github.com/notofonts/NotoSansOriya/tree/main/fonts/ttf/unhinted/instance_ttf>.
- `NotoSansTamil-Regular.ttf`. Source:
  <https://github.com/notofonts/NotoSansTamil/tree/main/fonts/ttf/hinted/instance_ttf>.
- `NotoSansTelugu-Regular.ttf`. Source:
  <https://github.com/notofonts/NotoSansTelugu/tree/main/fonts/ttf/hinted/instance_ttf>.
- `NotoSansKannada-Regular.ttf`. Source:
  <https://github.com/notofonts/NotoSansKannada/tree/main/fonts/ttf/hinted/instance_ttf>.
- `NotoSansMalayalam-Regular.ttf`. Source:
  <https://github.com/notofonts/NotoSansMalayalam/tree/main/fonts/ttf/hinted/instance_ttf>.
- `NotoSansSinhala-Regular.ttf`. Source:
  <https://github.com/notofonts/NotoSansSinhala/tree/main/fonts/ttf/hinted/instance_ttf>.
- `NotoSansMyanmar-Regular.ttf`. Source:
  <https://github.com/notofonts/notofonts.github.io/tree/main/fonts/NotoSansMyanmar/hinted/ttf>.
- `NotoSansThai-Regular.ttf`. Source:
  <https://github.com/notofonts/notofonts.github.io/tree/main/fonts/NotoSansThai/hinted/ttf>.
- `NotoSansLao-Regular.ttf`. Source:
  <https://github.com/notofonts/notofonts.github.io/tree/main/fonts/NotoSansLao/hinted/ttf>.
- `NotoSansNKo-Regular.ttf`. Source:
  <https://github.com/notofonts/notofonts.github.io/tree/main/fonts/NotoSansNKo/hinted/ttf>.
- `NotoSansBuginese-Regular.ttf`. Source:
  <https://github.com/notofonts/notofonts.github.io/tree/main/fonts/NotoSansBuginese/hinted/ttf>.
- `NotoSansTaiTham-Regular.ttf`. Source:
  <https://github.com/notofonts/notofonts.github.io/tree/main/fonts/NotoSansTaiTham/hinted/ttf>.
- `NotoSansBalinese-Regular.ttf`. Source:
  <https://github.com/notofonts/notofonts.github.io/tree/main/fonts/NotoSansBalinese/hinted/ttf>.
- `NotoSansSundanese-Regular.ttf`. Source:
  <https://github.com/notofonts/notofonts.github.io/tree/main/fonts/NotoSansSundanese/hinted/ttf>.
- `NotoSansLepcha-Regular.ttf`. Source:
  <https://github.com/notofonts/notofonts.github.io/tree/main/fonts/NotoSansLepcha/hinted/ttf>.
- `NotoSansLimbu-Regular.ttf`. Source:
  <https://github.com/notofonts/notofonts.github.io/tree/main/fonts/NotoSansLimbu/hinted/ttf>.
- `NotoSansCham-Regular.ttf`. Source:
  <https://github.com/notofonts/notofonts.github.io/tree/main/fonts/NotoSansCham/hinted/ttf>.
- `NotoSansBrahmi-Regular.ttf`. Source:
  <https://github.com/notofonts/notofonts.github.io/tree/main/fonts/NotoSansBrahmi/hinted/ttf>.
  Brahmi historical script (U+11000..U+1107F). The font ships only a `ccmp` GSUB
  feature and no positional features, so the parity test exercises segmentation and
  cluster handling rather than heavy GSUB substitution.
- `NotoSansSharada-Regular.ttf`. Source:
  <https://github.com/notofonts/notofonts.github.io/tree/main/fonts/NotoSansSharada/hinted/ttf>.
  Sharada historical script (U+11180..U+111DF). Ships `akhn`, `abvs`, and `blws`
  lookups. Sign i (U+111B4) is a spacing pre-base glyph that the USE reorder pass moves
  before the base.
- `NotoSansKhojki-Regular.ttf`. Source:
  <https://github.com/notofonts/notofonts.github.io/tree/main/fonts/NotoSansKhojki/hinted/ttf>.
  Khojki historical script (U+11200..U+1124F). The font lists lookups under both `khoj`
  and `gujr`. sigilbuzz tries `khoj` first.
- `NotoSansTirhuta-Regular.ttf`. Source:
  <https://github.com/notofonts/notofonts.github.io/tree/main/fonts/NotoSansTirhuta/hinted/ttf>.
  Tirhuta historical script (U+11480..U+114DF). Ships `rphf`, `abvf`, `blwf`, `pstf`,
  and positional lookups. Sign e (U+114B9) and sign o (U+114BC) are pre-base vowel
  signs that trigger the USE pre-base reorder.
- `NotoSansModi-Regular.ttf`. Source:
  <https://github.com/notofonts/notofonts.github.io/tree/main/fonts/NotoSansModi/hinted/ttf>.
  Modi historical script (U+11600..U+1165F). Ships `rphf`, `half`, `pres`, and `calt`
  lookups.
- `NotoSansOldHangul-Subset.ttf`. A subset of the Noto Sans Korean variable font (weight
  axis collapsed to Regular), limited to the Hangul Jamo blocks (U+1100..U+11FF,
  U+A960..U+A97F, U+D7B0..U+D7FF) plus a handful of precomposed modern Hangul
  syllables, so the parity tests can also check that modern Hangul still shapes
  correctly. Upstream: <https://github.com/google/fonts/tree/main/ofl/notosanskr>
  (`NotoSansKR[wght].ttf`). Subset with `fonttools subset`, keeping the layout
  features `ljmo`, `vjmo`, `tjmo`, `ccmp`, `calt`, `liga`, and `locl`, and dropping the
  variable (`gvar`, `fvar`, `avar`, `HVAR`, `STAT`) and vertical (`vhea`, `vmtx`)
  tables. The shaper only uses the static Regular master in this fixture.

- `NotoSansKR-HangulTone-Subset.ttf`. An 8 KB subset of the Noto Sans Korean variable
  font, instanced at Regular (weight 400) and cut to the characters
  `tests/hangul_tone_harfbuzz_parity.rs` needs: the Hangul tone marks U+302E and
  U+302F, U+25CC DOTTED CIRCLE, a few modern and old jamo, the syllables U+AC00,
  U+AC01, and U+AC1C, U+0301, and the space. The other Hangul fixture has no tone
  marks. Upstream: <https://github.com/google/fonts/tree/main/ofl/notosanskr>
  (`NotoSansKR[wght].ttf`), OFL 1.1, Copyright 2014-2021 Adobe, with Reserved Font
  Name 'Source'. Built with fontTools:

      python3 -m fontTools.varLib.instancer 'NotoSansKR[wght].ttf' wght=400 --static \
          -o NotoSansKR-Regular.ttf
      python3 -m fontTools.subset NotoSansKR-Regular.ttf \
          --unicodes="U+0020,U+0301,U+25CC,U+302E-302F,U+1100-1103,U+1161-1163,U+11A8-11AA,U+11C3,U+1140,U+A960,U+D7B0,U+D7CB,U+AC00,U+AC01,U+AC1C" \
          --layout-features="ljmo,vjmo,tjmo,ccmp,calt,liga,locl,kern,mark,mkmk" \
          --no-hinting --drop-tables+=STAT,MVAR,DSIG,BASE,vhea,vmtx \
          --output-file=NotoSansKR-HangulTone-Subset.ttf

## CFF subsetting fixtures

Real CFF1 and CFF2 fonts used by the round-trip tests in
`crates/sigilbuzz-subset/tests/real_cff_round_trip.rs`. They back up the synthetic
fixtures from #120, #135, and #138 with real OFL font bytes.

- `SourceCodePro-Latin-Subset.otf`: CFF1 (non-CID). Source: Adobe's upstream
  `SourceCodePro-Regular.otf` from
  <https://github.com/adobe-fonts/source-code-pro/raw/release/OTF/SourceCodePro-Regular.otf>.
  License: SIL Open Font License 1.1 (Adobe, "Source" reserved font name). Subset with
  `fonttools subset` to printable ASCII to keep the fixture small enough to include:

      python3 -m fontTools.subset SourceCodePro-Regular.otf \
          --unicodes='U+0020-007E' \
          --output-file=SourceCodePro-Latin-Subset.otf \
          --no-hinting --desubroutinize \
          --drop-tables+=GSUB,GPOS,GDEF,FFTM,DSIG \
          --no-layout-closure
- `SourceSans3VF-Latin-Subset.otf`: CFF2 variable font with a single `wght` axis from
  200 to 900. Source: Adobe's `SourceSans3VF-Upright.otf` from
  <https://github.com/adobe-fonts/source-sans/raw/release/VF/SourceSans3VF-Upright.otf>.
  License: SIL Open Font License 1.1 (Adobe, "Source" reserved font name). Subset with
  `fonttools subset` to printable ASCII while keeping the `fvar`, `avar`, and `HVAR`
  axis data, so the round trip can check that variation behavior survives:

      python3 -m fontTools.subset SourceSans3VF-Upright.otf \
          --unicodes='U+0020-007E' \
          --output-file=SourceSans3VF-Latin-Subset.otf \
          --no-hinting \
          --drop-tables+=DSIG,STAT,MVAR,BASE
- `CidCff1Synthetic.otf`: a hand-built CID-keyed CFF1 font (FDArray and FDSelect) with
  two Font DICTs and at least one subroutine shared across them. Real OFL CID-keyed
  CFF1 fonts are CJK fonts, far over the 200 KB limit per fixture, so this one is
  generated by `tests/tools/build_cid_cff1_fixture.py`. That script is the reproducible
  build, so re-run it whenever the fixture changes. The font contains no third-party
  copyrighted bytes, so no license terms apply. It maps U+0041..U+0045 ('A' to 'E') to
  five CID glyphs spread across two FDs, which exercises both per-FD subroutine
  renumbering (#135) and subroutines shared across FDs (#138):

      python3 tests/tools/build_cid_cff1_fixture.py
