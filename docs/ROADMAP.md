# sigilbuzz Roadmap to 0.1.0

**0.1.0 is not "MVP."** It is the point at which sigilbuzz is the *only* text shaper inside oniq, handles every real text case oniq throws at it, and beats rustybuzz on any benchmark we care about. Everything below is scaffolding toward that goal.

## Milestones

### M1 — Latin shaping that matches rustybuzz

The minimum thing that makes sigilbuzz load-bearing. Shape a run of Latin text against a real font, produce glyph IDs and advances that agree with rustybuzz to the integer.

- [x] `Blob`, `Face`, `Font`, `Buffer`, `Glyph`, `ShapedRun` public surface
- [x] SFNT directory parse + per-table byte slicing
- [x] Big-endian `Reader` primitives
- [ ] `head` parser (unitsPerEm, indexToLocFormat, flags)
- [ ] `maxp` parser (numGlyphs)
- [ ] `hhea` parser (numberOfHMetrics, ascent/descent/lineGap)
- [ ] `hmtx` parser (per-glyph advanceWidth + LSB)
- [ ] `cmap` parser — encoding records, format 4 (BMP), format 12 (full Unicode)
- [ ] `shape()` wiring: codepoint → cmap → glyph id → hmtx → advance → `Glyph`
- [ ] Golden-file tests against Open Sans Regular matching rustybuzz output byte-for-byte for a fixed corpus

### M2 — OpenType basics

What people actually expect from "a shaping engine."

- [ ] `GDEF` parser (glyph class definitions, mark attachment classes)
- [ ] `GSUB` lookup framework + lookup type 4 (ligature substitution) for `liga`
- [ ] `GSUB` lookup type 1 (single substitution) for `smcp`
- [ ] `GPOS` lookup framework + lookup type 2 (pair adjustment) for `kern`
- [ ] Feature tag override plumbing end-to-end (override defaults, enable alternates)
- [ ] Script / language-system selection (Latin default is "dflt/latn", but the infrastructure must be there for more)

### M3 — CJK + legacy kerning

So oniq's CJK fallback fonts render properly.

- [ ] Vertical writing (`vert`, `vrt2`) substitutions
- [ ] Legacy `kern` table (Apple format 0) for fonts without GPOS
- [ ] `loca` + `glyf` parsers sufficient for bounding-box queries (the renderer needs this even though MSDF generation lives outside sigilbuzz)

### M4 — Complex scripts

The payoff for being called a shaping engine at all.

- [x] Arabic cursive joining (`init`, `medi`, `fina`, `isol`, `rlig`)
- [ ] Indic reordering (`akhn`, `rphf`, `blwf`, `half`, `pstf`, `vatu`)
- [ ] Bidi-aware buffer preparation (UAX 9) — either in sigilbuzz or via a thin consumer-provided hook

### M5 — Variable fonts

Because 2024+ fonts ship variations and nobody should have to fall back to 2019-era shaping for them.

- [ ] `fvar` parser (axes)
- [ ] `avar` parser (axis variation maps)
- [ ] `gvar` parser (glyph outline deltas) — needed for glyph bbox queries even if sigilbuzz never rasterizes
- [ ] Per-shape variation coord plumbing on `Font`

### M6 — 2026 HarfBuzz parity (aspirational)

Optional modules that land on top of the shaping core. Any of these can become its own crate if it pulls sigilbuzz in directions the shaping core shouldn't go.

- [ ] `hb_gpu`-equivalent Slug-algorithm outline encoder for GPU rasterization
- [ ] `hb_gpu_paint_t`-equivalent COLRv0/v1 paint encoder
- [ ] PDF / SVG output backends

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

Milestones get their own issues under `github.com/Oneiriq/sigilbuzz` once there is something to discuss. Until then, this file is the source of truth for "what we are doing next."
