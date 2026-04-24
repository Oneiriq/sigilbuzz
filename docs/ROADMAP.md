# sigilbuzz Roadmap

**0.1.0 is not "MVP."** It is the point at which sigilbuzz is the *only* text shaper inside oniq, handles every real text case oniq throws at it, and beats rustybuzz on any benchmark we care about.

## 0.1.0 (shipping)

### M1 — Latin shaping that matches rustybuzz

- [x] `Blob`, `Face`, `Font`, `Buffer`, `Glyph`, `ShapedRun` public surface
- [x] SFNT directory parse + per-table byte slicing
- [x] Big-endian `Reader` primitives
- [x] `head` parser (unitsPerEm, indexToLocFormat, flags)
- [x] `maxp` parser (numGlyphs)
- [x] `hhea` parser (numberOfHMetrics, ascent/descent/lineGap)
- [x] `hmtx` parser (per-glyph advanceWidth + LSB)
- [x] `cmap` parser — encoding records, format 4 (BMP), format 12 (full Unicode)
- [x] `shape()` wiring: codepoint → cmap → glyph id → hmtx → advance → `Glyph`
- [x] Golden-file tests against Open Sans Regular matching rustybuzz output byte-for-byte

### M2 — OpenType basics

- [x] `GDEF` parser (glyph class definitions, mark attachment classes, MarkGlyphSetsDef)
- [x] Full GSUB lookup coverage: types 1, 2, 3, 4, 5 (fmt 1/2/3), 6 (fmt 1/2/3), 7 (extension), 8 (reverse chained)
- [x] Full GPOS lookup coverage: types 1, 2, 4, 5, 6, 7 (fmt 1/2/3), 8 (fmt 1/2/3), 9 (extension)
- [x] `LookupFlag` skip-iterators (IgnoreBaseGlyphs/IgnoreLigatures/IgnoreMarks/MarkAttachmentType/UseMarkFilteringSet)
- [x] `MAX_NESTED_DEPTH=16` guard on recursive lookup dispatch
- [x] Feature tag override plumbing end-to-end
- [x] Script / language-system selection with per-script priority lists (dev2 > deva > DFLT)

### M3 — CJK + legacy kerning

- [x] Vertical writing (`vert`, `vrt2`) substitutions — `vrt2` preferred if present
- [x] `vhea` / `vmtx` / `VORG` parsers + auto-enable on vertical `Buffer` direction
- [x] Legacy `kern` table (Apple format 0) with HarfBuzz-matching 64KB-length quirk + split delta
- [x] `loca` + `glyf` parsers sufficient for bounding-box queries

### M4 — Complex scripts

- [x] Arabic cursive joining (`init`, `medi`, `fina`, `isol`, `rlig`) with joining-type classifier from ArabicShaping.txt
- [x] Devanagari reordering (`nukt`, `akhn`, `rphf`, `rkrf`, `blwf`, `half`, `pstf`, `vatu`, `cjct`, `init`, `pres`, `abvs`, `blws`, `psts`, `haln`, `dist`)
- [x] Indic per-glyph info masks (`unicode_props`, `indic_position`) + final reph reorder to display slot
- [x] UAX 9 paragraph-direction resolution (full ordering remains a consumer concern)

### M5 — Variable fonts

- [x] `fvar` parser (axes + named instances)
- [x] `avar` parser (axis variation maps, `normalize_to_coords`)
- [x] `gvar` parser (tuple variation headers, packed point numbers, packed deltas, all-points shortcut)
- [x] `HVAR` advance-width delta evaluation with `ItemVariationStore`
- [x] `Font::with_coords` — per-shape variation coords threaded through the pipeline
- [x] `Face::glyph_bounds_at_coords` for coord-aware bbox queries

### M6 — 2026 HarfBuzz parity (color + paint)

- [x] `COLRv0` layered-glyph subtable
- [x] `COLRv1` paint tree — all 32 paint variants, zero-copy traversal via `Colr::paint_at`
- [x] `CPAL` v0/v1 palette tables with BGRA→RGBA unpack and palette-type flags

### Hardening

- [x] Malformed-font fuzz pass on every parser (two waves, five bugs fixed and shipped)
- [x] Deterministic output — no `HashMap`/`BTreeMap` iteration in shaping paths
- [x] `cargo build --no-default-features` clean
- [x] `cargo clippy -D warnings` clean under default and `--no-default-features`
- [x] Zero runtime deps in the core crate

---

## 0.2.0 (next)

Scripts that rustybuzz still covers that sigilbuzz 0.1.0 does not.

### SE Asian scripts via USE (Universal Shaping Engine)

- [ ] USE machinery — category classifier from Unicode data, syllable segmenter, reorder pass
- [ ] Khmer (`abvs`, `blws`, `pres`, `psts`, `calt`, `ccmp`, `cjct`, `pref`, `rphf`)
- [ ] Myanmar
- [ ] Thai (with mark reordering)
- [ ] Lao
- [ ] Old Hangul (Jamo composition)

### Remaining Indic scripts

- [ ] Bengali (`RephPosition::AfterMain`)
- [ ] Gurmukhi
- [ ] Gujarati
- [ ] Oriya
- [ ] Tamil
- [ ] Telugu (`RephPosition::AfterPost`)
- [ ] Kannada (`RephPosition::AfterPost`)
- [ ] Malayalam
- [ ] Sinhala

### Hebrew

- [ ] Hebrew shaping (cantillation, vowel positioning, `dlig` / `hlig` / `calt`)

### Legacy

- [ ] AAT `morx` (extended glyph metamorphosis, Apple fallback for non-GSUB fonts)
- [ ] AAT `kerx` (extended kerning for macOS legacy fonts)

### Variable-font deltas in GPOS

- [ ] GPOS feature-variations (HVAR-style deltas for pair-kerning `ValueRecord`s)

---

## 0.3.0+ (aspirational)

- [ ] `hb_gpu`-equivalent Slug-algorithm outline encoder for GPU rasterization
- [ ] PDF / SVG output backends (may live in a separate crate)
- [ ] COLRv1 paint *evaluation* helpers (stays a consumer concern by default)

---

## Dogfood protocol (driven by oniq)

sigilbuzz is not allowed to sit in isolation. Every milestone is validated by integrating it into oniq and running oniq's tests and demos.

1. **Wire the crate as a path dependency** once M1 is complete. Add a `text-sigilbuzz` feature on oniq that is mutually additive with `text-harfbuzz` (they're different backends). Create `engine/src/text/shaping/sigilbuzz.rs` exposing a `SigilbuzzShaper: TextShaper`.
2. **Run the existing `harfbuzz_ttf` integration tests against both backends.** For every assertion, both backends should produce identical glyph IDs and advances; divergence is a sigilbuzz bug.
3. **Cut over demos one at a time.** Each demo flips from `text-harfbuzz` to `text-sigilbuzz` when sigilbuzz covers its script needs. A demo is not allowed to regress visually.
4. **Remove rustybuzz from oniq** once `cargo test --features text-sigilbuzz` passes the full suite and no demo still requires `text-harfbuzz`.

## Non-negotiables

- No dependencies added to the core crate. Every new external requires an entry in `docs/deps.md` with a paragraph-long justification.
- Every parser ships with hand-crafted byte fixtures exercising the happy path, the boundaries, and at least one truncated / malformed case.
- Shaping output is deterministic — same font, same buffer, same feature list, same bytes out. Golden files enforce this.
- Works with `cargo build --no-default-features` throughout. Any new feature gate that breaks no_std blocks merging.

## Tracking

Each release line gets a GitHub milestone. Follow-ups surface as issues referenced from the tracking issue for the milestone.
