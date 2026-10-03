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

- `NotoSansKR-Calt-Subset.ttf`. A 4 KB subset of the same Regular instance of Noto Sans
  Korean, with one lookup added, for `tests/hangul_mixed_parity.rs`: a single
  substitution under every script's `calt` that turns `a` into `c` and U+1100 into
  U+1101, so the test sees which glyphs `calt` reaches. It keeps the space, `a` to `c`,
  U+00E1, U+0301, U+25CC, U+302E, U+1100, U+1101, U+1161, U+11A8, U+AC00, U+AC01, and
  U+4E2D. Same upstream and license as `NotoSansKR-HangulTone-Subset.ttf`. Built with
  fontTools:

      python3 -m fontTools.subset NotoSansKR-Regular.ttf \
          --unicodes="U+0020,U+0061-0063,U+00E1,U+0301,U+25CC,U+302E,U+1100-1101,U+1161,U+11A8,U+AC00,U+AC01,U+4E2D" \
          --layout-features="ljmo,vjmo,tjmo,ccmp,calt,locl" \
          --no-hinting --drop-tables+=STAT,MVAR,DSIG,BASE,vhea,vmtx,GPOS \
          --output-file=subset.ttf

  then, in Python, append `buildLookup([buildSingleSubstSubtable({"a": "c", "uni1100":
  "uni1101"})])` from `fontTools.otlLib.builder` to the GSUB lookup list and add its
  index to every `calt` feature.

- `NotoSansKR-Palt-Subset.ttf`. A 24 KB subset of the Noto Sans Korean variable font,
  axes and all, for `tests/feature_variations_gpos_parity.rs`. Its GPOS is version 1.1,
  with one FeatureVariations record that gives `palt` and `vpal` an extra lookup from
  `wght` 0.77899 (normalized) on. It keeps the space, U+3001, U+3002, U+300C, U+300D,
  five hiragana, seven katakana, and U+FF01, U+FF08, U+FF09, and U+FF1F. Same upstream
  and license as `NotoSansKR-HangulTone-Subset.ttf`. Built with this repository's
  subsetter, which keeps and remaps the FeatureVariations:

      cargo run -p sigilbuzz-cli --release -- subset 'NotoSansKR[wght].ttf'           NotoSansKR-Palt-Subset.ttf           --unicodes="U+0020,U+3001,U+3002,U+300C,U+300D,U+3042,U+3044,U+3046,U+3048,U+304A,U+30AB,U+30BF,U+30CA,U+30C6,U+30B9,U+30C8,U+FF08,U+FF09,U+FF01,U+FF1F"

- `NotoSansDevanagari-NoGDEF-Subset.ttf`. A 2 KB subset of
  `NotoSansDevanagari-Regular.ttf` above without its GDEF table, for
  `tests/vowel_constraints_parity.rs`: with no GDEF glyph classes, HarfBuzz synthesizes
  them from each character's General_Category. It keeps the space, U+0905, U+0915,
  U+0945, and U+25CC. Same upstream and license as the full font. Built with fontTools:

      python3 -m fontTools.subset NotoSansDevanagari-Regular.ttf \
          --unicodes="U+0020,U+0905,U+0915,U+0945,U+25CC" --layout-features='*' \
          --no-hinting --drop-tables+=GDEF,DSIG,STAT,MVAR,BASE \
          --output-file=NotoSansDevanagari-NoGDEF-Subset.ttf

- Subsets of six Noto fonts for `tests/script_coverage_parity.rs`, one for each of
  Javanese, Chakma, Khudawadi, Takri, Syriac, and Adlam, which HarfBuzz shapes with the
  Universal Shaping Engine or, for Syriac, the Arabic shaper. Each keeps the characters
  the tests shape, the space, U+200C, U+200D, and U+25CC, and all of its layout features.
  The sources are the hinted builds at
  <https://github.com/notofonts/notofonts.github.io/tree/main/fonts>
  (`NotoSans<Script>/hinted/ttf/NotoSans<Script>-Regular.ttf`), from the Noto Project
  Authors under the OFL 1.1. Built with fontTools 4.66.1 (`fontTools.subset` with
  `layout_features=['*']`, no hinting, all name IDs, the `.notdef` outline, and
  `DSIG`, `STAT`, `MVAR`, and `BASE` dropped):
  - `NotoSansJavanese-Subset.ttf`: U+A980..U+A983, U+A986, U+A98F, U+A9A0, U+A9A1,
    U+A9A4, U+A9AB, U+A9AD, U+A9B2, U+A9B4, U+A9B6, U+A9B8, U+A9BA..U+A9C0.
  - `NotoSansChakma-Subset.ttf`: U+11100..U+11103, U+11107, U+11108, U+11116, U+1111A,
    U+11122, U+11123, U+11127, U+11128, U+1112C, U+1112D, U+11131, U+11133, U+11134.
  - `NotoSansKhudawadi-Subset.ttf`: U+112B0, U+112BA, U+112C0, U+112C9, U+112D8,
    U+112DF, U+112E0, U+112E1, U+112E3, U+112E5, U+112E9, U+112EA.
  - `NotoSansTakri-Subset.ttf`: U+11680, U+11686, U+1168A, U+11694, U+116A2, U+116A4,
    U+116AB, U+116AD, U+116AE, U+116B2, U+116B4, U+116B6, U+116B7.
  - `NotoSansSyriac-Subset.ttf`: U+0640, U+070F, U+0710, U+0712, U+0713, U+0715,
    U+0718, U+071D, U+0720, U+0721, U+072A, U+0730, U+0732. It keeps the font's `stch`
    feature, which stretches U+070F SYRIAC ABBREVIATION MARK.
  - `NotoSansAdlam-Subset.ttf`: U+1E900, U+1E902, U+1E904, U+1E922, U+1E924, U+1E926,
    U+1E944, U+1E946, U+1E94A, U+1E94B.

- `NotoSansDevanagari-Dev3-Subset.ttf`. A 41 KB subset of
  `NotoSansDevanagari-Regular.ttf` above whose `dev2` script records in GSUB and GPOS are
  renamed `dev3`, for `tests/indic3_parity.rs`. No released font uses the Indic3 tags
  yet, and HarfBuzz gives a font that has them the Universal Shaping Engine. It keeps
  the space, U+200C, U+200D, U+25CC, U+0901..U+0903, U+0905, U+0915, U+0916, U+0924,
  U+0928, U+092A, U+092E, U+092F, U+0930, U+0937, U+0938, U+093C, U+093E..U+0941,
  U+0947, U+094B, and U+094D. Same upstream and license as the full font. Built with
  fontTools 4.66.1, subset as above, then in Python:

      for tag in ("GSUB", "GPOS"):
          records = font[tag].table.ScriptList.ScriptRecord
          for r in records:
              if r.ScriptTag == "dev2":
                  r.ScriptTag = "dev3"
          records.sort(key=lambda r: r.ScriptTag)

- `NotoSansChakma-Dist-Subset.ttf` and `NotoSansChakma-Dist9-Subset.ttf`. 6 KB subsets
  of Noto Sans Chakma for `tests/concat_flags_parity.rs`. They keep the class-based
  chained context of the `dist` feature, whose rules for a letter all start with a
  vowel sign or a post-base form. Same upstream, license and fontTools options as
  `NotoSansChakma-Subset.ttf` above, with the space, U+200C, U+200D, U+25CC, U+11103,
  U+11120, U+11122, U+11133, U+11134, U+11145, and U+11146.
  `NotoSansChakma-Dist9-Subset.ttf` then gets eight chained context subtables in front
  of the `dist` lookup's subtable, so it is the ninth. Each matches only the space, in
  Python:

      from fontTools.ttLib.tables import otTables as ot

      gpos = font["GPOS"].table
      (dist,) = [r.Feature.LookupListIndex for r in gpos.FeatureList.FeatureRecord
                 if r.FeatureTag == "dist"]
      lookup = gpos.LookupList.Lookup[dist[0]]

      def space_only():
          st = ot.ChainContextPos()
          st.Format = 3
          cov = ot.Coverage()
          cov.glyphs = ["space"]
          st.BacktrackGlyphCount = 0
          st.BacktrackCoverage = []
          st.InputGlyphCount = 1
          st.InputCoverage = [cov]
          st.LookAheadGlyphCount = 0
          st.LookAheadCoverage = []
          st.PosCount = 0
          st.PosLookupRecord = []
          return st

      lookup.SubTable = [space_only() for _ in range(8)] + lookup.SubTable
      lookup.SubTableCount = len(lookup.SubTable)

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
