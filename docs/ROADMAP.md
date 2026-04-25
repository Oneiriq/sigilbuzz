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

## 0.2.0 (shipping)

Scripts that rustybuzz still covers that sigilbuzz 0.1.0 did not, plus AAT fallback, plus variable-font kerning.

### SE Asian scripts via USE (Universal Shaping Engine)

- [x] USE machinery — category classifier from Unicode data, syllable segmenter, reorder pass
- [x] Khmer (`abvs`, `blws`, `pres`, `psts`, `calt`, `ccmp`, `cjct`, `pref`, `rphf`)
- [x] Myanmar
- [x] Thai
- [x] Lao
- [x] Old Hangul (Jamo composition)

### Remaining Indic scripts

- [x] Bengali (`RephPosition::AfterSub`)
- [x] Gurmukhi (`RephPosition::BeforeSub` + `blwf` masking)
- [x] Gujarati (`RephPosition::BeforePost`)
- [x] Oriya (`RephPosition::AfterMain`)
- [x] Tamil (split-matra decomposition for U+0BCA/U+0BCB/U+0BCC)
- [x] Telugu (`RephPosition::AfterPost` + `RephMode::Explicit`)
- [x] Kannada (`RephPosition::AfterPost`)
- [x] Malayalam (`RephMode::LogRepha` via U+0D4E)
- [x] Sinhala (split-matra decomposition for U+0DDA/U+0DDC/U+0DDD/U+0DDE + Explicit reph)

### Hebrew

- [x] Hebrew shaping with `hebr` > DFLT script priority, niqqud via GPOS mark-to-base, cantillation via mark-to-mark

### Mixed-script runs

- [x] `Buffer::script_runs` iterator auto-segments the buffer; `shape()` dispatches GSUB/GPOS once per segment with the segment's own script priority
- [x] INHERITED combining-mark blocks attach to the previous real-script segment so clusters survive

### Legacy

- [x] AAT `morx` (Extended Glyph Metamorphosis) — types 0 (Rearrangement), 1 (Contextual), 2 (Ligature)
- [x] AAT `kerx` (Extended Kerning) — format 0 ordered pair list
- [x] OpenType-wins policy — morx/kerx only consulted when GSUB/GPOS absent

### Variable-font deltas in GPOS

- [x] GPOS feature-variations — `Device`/`VariationIndex` sub-offsets on every `ValueRecord` field, resolved against GDEF's shared `ItemVariationStore`

### Hardening

- [x] Wave 3 fuzz pass on the 0.2.0 surface (+1 fix — INHERITED segmenter attachment)

---

## 0.3.0 (shipping)

Renderer-facing companion crates land in the workspace; remaining script-completeness carry-overs from 0.1.0 / 0.2.0 close.

### Workspace conversion

- [x] `[workspace]` at the repo root with `members = [".", "crates/*"]`. Companion crates inherit `edition` / `rust-version` / `authors` / `license` / `repository` and pull `sigilbuzz` via `workspace.dependencies`.

### Glyph outline extraction (prerequisite for renderer crates)

- [x] Full `glyf` simple + composite contour parsing, with composite flattening at extraction time and gvar deltas applied when coords are passed
- [x] CFF1 Top DICT + Subr INDEX + CharStrings INDEX + Type 2 charstring interpreter (rmoveto/hmoveto/vmoveto/rlineto/hlineto/vlineto/rrcurveto family/endchar/callsubr/callgsubr/return/hstem/vstem/hintmask/cntrmask)
- [x] CFF2 trimmed CFF1 + `blend` operator against the shared `ItemVariationStore`
- [x] `Face::glyph_outline(gid)` and `Face::glyph_outline_at_coords(gid, coords)` over a `PathOp { MoveTo, LineTo, QuadTo, CubicTo, Close }` enum

### Companion crate `sigilbuzz-gpu`

- [x] Slug-algorithm GPU outline encoder
- [x] Iterative cubic-to-quadratic flattening (Sederberg third-difference, MAX_DEPTH=18)
- [x] Band decomposition with deterministic per-band scatter
- [x] `SlugOptions { band_count, cubic_tolerance }` with sane 1-design-unit default

### Companion crate `sigilbuzz-paint`

- [x] COLRv1 paint evaluator emitting a `DrawCmd { FillGlyph, PushLayer, PopLayer }` stream
- [x] Transform composition (Translate/Scale/Rotate/Skew + Var* siblings) over a 6-tuple affine
- [x] ColorLine stop resolution against `CPAL`, foreground sentinel `0xFFFF`, alpha multiplication
- [x] `PaintComposite` semantics matching Skia / SVG layer ordering
- [x] Cycle detection across `PaintColrGlyph` recursion

### Script-completeness follow-ups

- [x] Myanmar kinzi reorder (#44)
- [x] Thai sara-am / Lao lao-am U+0E33 / U+0EB3 PUA decomposition (#45)
- [x] Old Hangul per-run shaper selection for mixed buffers (#46)
- Deferred: Amiri Allah/bism-Allah `rlig` rule-selection (#21) — glyph count already parity, gid sequence diverges by ~200-line dispatcher refactor; rolled to 0.4.0.

### Hardening

- [x] Wave 4 fuzz pass on the new outline + Slug + paint surface (+3 fixes — CFF stack cap, Slug NaN/zero-tolerance, CFF `hflex1` `dy6` index)

---

## 0.4.0+ (next)

Polish + remaining gaps that surfaced during 0.3.0 development.

- [ ] Amiri `rlig` rule-selection parity (#21) — feature dispatcher rule-ordering refactor
- [ ] glyf composite anchor-point resolution (the 2/6710 Amiri misses noted by #52)
- [ ] sigilbuzz-paint coverage gaps — radial / sweep gradient integration tests, full ItemVariationStore delta application path
- [ ] AAT `kerx` format 2 (compound-class kerning)
- [ ] PDF / SVG output backends — likely as `sigilbuzz-pdf` / `sigilbuzz-svg` companion crates

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
