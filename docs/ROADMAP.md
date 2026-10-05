# Roadmap

sigilbuzz is heading for 1.0 in 2026. For 1.0 I want a stable shaping API and a
documented path for renderers built on top of it. What already shipped is in
[CHANGELOG.md](../CHANGELOG.md).

## Next

Roughly in this order: what the consumers need first, then output differences that
real text hits, then parity on crafted fonts.

- Consumer adoption of 0.24.0:
  - Keep one `Font` per font, size and instance. The shaping caches start at a
    font's second call, so a backend that builds a `Font` for every run (oniq's does)
    gets only the first-call savings.
  - Move oneiric's per-run `Cff2` workaround to `Face::glyph_outlines`.
  - Have the C API's `hb_font_set_variations` call `Font::with_variations`. It still
    normalizes by hand and sets only the first axis that a repeated tag names.
  - Shape SVG `<text>` in oniq with sigilbuzz, so usvg's `text` feature, the last
    path that links rustybuzz into a consumer, can go.
  - Run the 554-case differential built from the consumers' fonts and strings in CI.
- Per-glyph cost of long runs. A 100-character run still takes 2 to 5 times as long as
  in HarfBuzz (normalization, Hangul composition, PairPos walks), and a font's first
  top-to-bottom call is slower than in 0.23.0 for fonts without `VORG`.
- A Devanagari fuzz input that GSUB grows to 3,079 glyphs takes about 24 s in a fuzz
  build (#302). Find the stage that grows or walks the buffer faster than linearly and
  bound it the way HarfBuzz's `max_len` and `max_ops` do.
- Feature semantics:
  - Automatic fractions around U+2044 and `rand`, which HarfBuzz's default GSUB
    stages hold. `rand` needs HarfBuzz's seeded generator so output stays
    deterministic.
  - Feature ranges. `Feature` has no start and end, and the C API ignores
    `hb_feature_t.start` and `end`, so a ranged feature applies to the whole buffer.
    Hangul `calt=2` also differs from HarfBuzz.
  - Required features in HarfBuzz's stages: a required `kern`, `frac` or vertical
    Hangul `calt`, the Arabic required `init`, `medi`, `fina`, `isol`, `stch` and
    `mset`, and `rtla`, `ltra`, `rtlm` and `ltrm` in the syllabic and Hangul shapers.
- Syllabic shapers on a glyph buffer. When a GSUB stage 0 lookup (`rvrn` or a required
  feature) changes a segment's glyph count, the Indic, Khmer, Myanmar and Universal
  Shaping Engine shapers skip that segment, because their code points no longer line
  up with the glyphs. Then the Khojki ZWJ difference.
- Variation core:
  - Per-shared-tuple scalars in `gvar`. Each glyph works out every tuple's scalar
    over every axis, so a crafted 2,000-axis font takes about twice as long as in
    HarfBuzz to instance partially.
  - `avar` version 2, which is rejected today.
  - An `OS/2` reader and a font metrics API, for the `USE_TYPO_METRICS` vertical
    fallback and for the line metrics consumers need before they can drop ttf-parser.
- Read the VARC MultiItemVariationStore layout HarfBuzz adopted after 14.5.0, once
  fontTools and a HarfBuzz release write and read it (#299). Until then such a `VARC`
  counts as absent, as in HarfBuzz 14.5.0.

## Known gaps

These are smaller pieces that are not scheduled yet.

Shaping and Unicode:

- Scripts without a `UnicodeScript` bucket (Armenian, Georgian, Coptic, Ethiopic,
  Thaana and others) shape as `Other` under `DFLT`, where HarfBuzz tries their own tag
  first. `Other` segments would need to carry their ISO 15924 code.
- A buffer whose script is set or guessed as Han because its first letter is kana takes
  the `hani` tag; HarfBuzz uses `kana`.
- `stch` tells Lm and Lo letters from cased letters through Rust's case properties,
  so a titlecase letter, or a modifier letter with Other_Lowercase, is misjudged.
- The General_Category table is at Unicode 17.0.0 and the Script table at 18.0.0.
  HarfBuzz 14.5.0 reads 18.0.0 for both.
- Vertical CJK with `vert=0`, and `vert=1` in horizontal text, differ from HarfBuzz in
  66 cases of the sweeps run against 0.23.1.
- `morx` ligatures: HarfBuzz merges the clusters of the glyphs between a ligature's
  first and last component; sigilbuzz gives only the ligature the merged cluster.
- AAT lookup formats 4, 8 and 10 are not read, so a `morx` table that uses them does
  nothing there.

Performance:

- No caller-owned plan object like `hb_shape_plan`. The caches live in `Font`.
- Lookups of tens of thousands of format 1 or format 2 context subtables are 4% to 11%
  slower than in 0.23.1, and a crafted table of 8,000 overlapping lookups that runs
  past the cache budget about 30% slower.
- Source Code Pro (static CFF) shapes 100 Latin characters left to right in about
  8.0 us against 7.0 us in 0.23.0.

VARC:

- The 2,048-component, 2^20-op and 64-level caps fail the glyph, where HarfBuzz stops
  expanding at 64 levels and 16,384 edges and draws what it has. A cycle of more than
  32 glyphs still reaches the 64-level cap.
- Glyph extents of VARC glyphs come from the `glyf` box for a `glyf` font and from
  the control box of the VARC outline for a CFF font. HarfBuzz unions each leaf's
  extents, mapped by its transform.
- `GlyphOutlines` draws VARC leaves from tables it reads once per glyph, and
  `Face::glyph_outline_at_coords` parses `VARC` once per call.
- HarfBuzz after 14.5.0 applies VARC deltas in a font without `fvar`; 14.5.0 and
  sigilbuzz do not.

Subsetting and instancing (`sigilbuzz-subset`):

- A partial CFF2 instance keeps the fractional values its store projection gives,
  where HarfBuzz rounds each folded default and projected delta. Source Serif 4 VF at
  `wght=650,opsz=keep` comes out at 2.59 MB against HarfBuzz's 0.96 MB.
- CFF2 instances keep the source's `vmtx` top side bearings, `vhea` extremes and `VORG`
  values, which HarfBuzz recomputes from the glyph boxes.
- Private DICT values (`BlueScale`, blended `BlueValues`) are written slightly
  differently from HarfBuzz, and `GPOS` is not repacked, so it is about twice
  HarfBuzz's size.
- `AxisLimit::Range` cannot narrow an axis or move its default (HarfBuzz's L4
  instancing). It returns `SubsetError::Unsupported`.
- The `subset` fuzz target does not call `instance_user` yet.

Rendering, paint and text layout:

- SVG `<textPath>` tangent rotation, `side="right"`, and path cycling.
- sbix `jp2 ` (JPEG 2000) images.
- TIFF LZW and CCITT compression, multiple IFDs, and non-RGB color models.
- Nested SVG masks.
- SVG filters: `feTurbulence`, `feImage`, `feMorphology`, `feConvolveMatrix`,
  `feSpecularLighting`, `feDiffuseLighting`, `feComponentTransfer`, `feComposite`,
  `feBlend`, `feTile`, `feDropShadow` and `feDisplacementMap`.
- JPEG chroma upsampling is nearest-neighbor. libjpeg's fancy upsampling differs by up
  to 95 levels at sharp chroma edges.
- `sigilbuzz-paint` walks the paint tree in HarfBuzz 11's callback order. HarfBuzz 14
  pushes an `...AroundCenter` transform as one composed transform and orders the root
  transform and clip differently; the values match once the transforms are composed.
- Dictionary-based breaking in `sigilbuzz-text-layout` for the Southeast Asian
  scripts in UAX 14 class SA (Thai, Lao, Khmer, Myanmar, Tai Tham, and others), which
  write words without spaces between them. Without a dictionary, line breaking treats
  a run of them as one word that breaks only at spaces and punctuation (LB1 resolves
  SA to AL), and `word_breaks` puts a boundary between every character of such a run
  (UAX 29 leaves SA letters out of ALetter). The rest of UAX 14 and UAX 29 is done,
  including Korean jamo, Brahmic orthographic syllables (LB28a), and regional
  indicators (LB30a).
- Hyphenation patterns for German, French, and Spanish. The cargo features exist but
  ship no patterns yet.
- A UAX 50 vertical-orientation helper. oneiric patches sideways spaces by hand.
- Language tags from version 1 `name` tables are read but not exposed.

## How work gets picked

sigilbuzz is the text shaper of a real text rendering application and the tools
around it. None of them runs a HarfBuzz backend anymore, so parity is measured against
HarfBuzz 14.5.0 directly: each change is compared with HarfBuzz's output, through
uharfbuzz, on the repository's fonts, system fonts and crafted fonts. The consumers'
own fonts and strings feed a differential of 554 cases, which 0.24.0 matches in glyph
IDs, clusters and positions. A difference from HarfBuzz counts as a sigilbuzz bug. The
end goal is to drop rustybuzz from the consumers entirely.

When I'm deciding what to build next, the first question is whether real text rendering
workloads need it. Gaps that HarfBuzz covers and sigilbuzz doesn't come after that.

Each release has a GitHub milestone. Follow-up work is filed as issues against it.

## Ground rules

- The core crate takes no dependencies. Any new external dependency, in any crate,
  needs an entry in [deps.md](deps.md).
- Every parser ships with hand-built byte fixtures that cover the normal case, the
  boundaries, and at least one truncated or malformed input.
- Shaping output is deterministic. The same font, buffer, and features give the same
  bytes out.
- `cargo build --no-default-features` keeps working. A feature gate that breaks
  `no_std` does not merge.
