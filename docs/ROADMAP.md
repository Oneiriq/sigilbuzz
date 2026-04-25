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

## 0.4.0 (shipping)

Polish: every script-completeness carry-over from prior releases closes, the renderer-output companion crates land, and two real latent bugs surfaced during the polish work get fixed.

### Script-completeness

- [x] Amiri `rlig` rule-selection parity (#21) — refactored `apply_gsub_lookup` to mirror HarfBuzz's `apply_forward` (cursor-first walk, take first matching subtable per cursor) instead of running each subtable across the whole run independently. Net -200 lines. Allah / bism-Allah / Muhammad / lillah / qul / akbar all parity-clean.
- [x] glyf composite anchor-mode + TWO_BY_TWO column-major fix — the 2/6710 Amiri parity miss was actually a TWO_BY_TWO matrix-field-order bug latent since 0.3.0; symmetric scales hid it. Anchor-mode resolution implemented at the same time. Amiri now 6710/6710.

### AAT compound-class kerning

- [x] AAT `kerx` format 2 — compound-class kerning subtable. AAT lookup-table format 0 + 6 (format 2 already shipped in 0.3.0). 904-byte synthetic fixture.

### Companion crate `sigilbuzz-svg`

- [x] SVG outline emission (default feature) — `glyph_to_svg` / `glyph_to_svg_at_coords` over `PathOp` → SVG path-data.
- [x] COLRv1 emission gated on the `color` feature — depends on `sigilbuzz-paint` only when enabled.
- [x] Sweep-gradient gracefully degrades to a linear gradient with a `<!-- sweep-fallback -->` marker so consumers needing true sweep can detect and route through `sigilbuzz-gpu`.

### Companion crate `sigilbuzz-pdf`

- [x] PDF Type 3 font emitter — `emit_type3_font(face, gids) -> Type3Font` with per-glyph `CharProc` + Encoding + Widths + FontMatrix.
- [x] Exact quadratic-to-cubic degree elevation for `QuadTo` ops (PDF has no quadratic operator).

### sigilbuzz-paint coverage + bug fixes

- [x] PaintRadialGradient + PaintSweepGradient integration coverage.
- [x] Full ItemVariationStore delta-application path with hand-rolled IVS fixture (1 axis, 2 regions, 5 delta-set rows).
- [x] DeltaSetIndexMap indirection — `var_index_base` now resolves through GDEF's `DeltaSetIndexMap` instead of bit-slicing the base directly.
- [x] **F2DOT14 unit fix** — `var_delta` returned raw IVS ints as `f32` and added them to fields already divided by 16384; every PaintVar* alpha and stop-offset was scaled wrong.
- [x] **GDEF-parked IVS fix** — fonts that store the shared IVS in GDEF (per OpenType v1.3+) silently emitted static output even with non-empty coords.

### Hardening

- [x] Wave 5 fuzz pass on the 0.4.0 surface (+2 fixes — `sigilbuzz-svg` leaf-list misalignment when an interior glyph has no outline; `kerx` per-subtable error handling so one malformed subtable doesn't poison the rest of the table).

---

## 0.5.0 (shipping)

Operational gaps closed. Subsetting, the remaining PDF font types, performance baselines, crates.io publish posture, and the phantom-point glyf fallback all land.

### Companion crate `sigilbuzz-subset`

- [x] HarfBuzz `hb-subset` equivalent — font subsetting for distribution + serving. Closure walker pulls composite components into the kept set; cmap / glyf / loca / hmtx / hhea / maxp / head / post all rewritten with new gid order. SFNT directory rebuilt with checksums per spec. Open Sans → {A,B,C} produces a 2 284-byte font (~1% of the 217 KiB source).
- Pass-through: `name`, `OS/2`. Dropped (or strict-error) by default: GDEF, GSUB, GPOS, kern, vhea, vmtx, VORG, HVAR, gvar, COLR, CPAL, morx, kerx, fvar, avar. CFF/CFF2 raise `SubsetError::Unsupported`. Layout-table subsetting follows in 0.6.0+.

### `sigilbuzz-pdf` Type 1 + OTF-embedded

- [x] Type 1 (PostScript) emitter — cleartext charstrings (eexec encryption deliberately skipped), Type 1 number encoding across all four byte-form tiers, full PathOp coverage with degree-elevated cubics for QuadTo.
- [x] OTF / TrueType embedded emitter — emits the PDF font dictionary + descriptor + Identity-H CIDToGIDMap + widths around a raw font program. Subsetting the embedded program is `sigilbuzz-subset`'s job.

### Performance baselines

- [x] Criterion benches across Latin / Arabic / Devanagari / Khmer / Hebrew shaping plus Slug encoding and COLRv1 evaluation. `docs/PERFORMANCE.md` records the baseline.
- [x] Filed `performance` issues for the > 2× rustybuzz gaps: Arabic 32× (#74), Devanagari 173× (#75), Khmer 17× (#76). Tuning is 0.6.0+ work.
- Hebrew at 0.88× rustybuzz (faster). Latin at 1.17×.

### crates.io publish posture

- [x] Every workspace member has `publish = true`, finalised metadata (description / keywords / categories / per-crate README), and verifies under `cargo publish --dry-run --no-verify --allow-dirty`. Bootstrap caveat documented in `docs/RELEASING.md`: companion crates' `--dry-run` cannot resolve `{ workspace = true }` until `sigilbuzz` has at least one real publish on crates.io.
- [x] Full Apache 2.0 LICENSE + standard NOTICE present at the repo root.

### Glyf phantom-point anchors

- [x] Phantom-point references in glyf composite anchor-mode now resolve against `hmtx` (and `vmtx` if present) instead of degrading to zero translation. Currently dead-but-correct on the bundled corpus — neither Open Sans nor Amiri uses phantom-anchor mode — and exercised by a synthetic unit test until a fixture font that anchors to phantoms is added.

### Hardening

- [x] Wave 6 fuzz pass on the 0.5.0 surface (+2 fixes — `sigilbuzz-subset` Cargo metadata gap from publish-prep / subset-crate landing order; `sigilbuzz-pdf` Type 3 `font_matrix(0)` returning `[inf 0 0 inf 0 0]` for malformed faces, now clamped to match OTF-embedded behavior).

---

## 0.6.0 (shipping)

Operational gaps from 0.5.0 closed. `sigilbuzz-subset` covers every gid-keyed table in scope; complex-script shaping comes within 5× of rustybuzz; `sigilbuzz-woff` lands as the seventh workspace member; the workspace gains its first runtime dependency (`brotli-decompressor`, gated and confined to `sigilbuzz-woff`).

### Subset extension

- [x] Layout-table policy in `sigilbuzz-subset` — Coverage / ClassDef auto-format-pick emitters; closure walker pulls in ligature components and mark-base anchor partners; Preserve-or-Drop policy keeps GSUB / GPOS / GDEF intact under identity gid maps. Full byte-level rewriter for non-identity maps tracked as #87 for 0.7.0+.
- [x] CFF / CFF2 analysis primitives — Type 2 charstring scanner, subr-bias helper, transitive subr keep-set, operand encoder/decoder. Byte-level emitter tracked as #92 for 0.7.0+.
- [x] Variable-font subsetting — gvar by gid, HVAR with `ItemVariationStore` row dedup + `DeltaSetIndexMap` rewrite, fvar / avar pass-through. `SubsetInput.retain_variations` defaults true.

### Performance

- [x] Three algorithmic fixes in `src/shape.rs` close the > 2× rustybuzz gaps from #74 / #75 / #76: pre-parsed `ParsedGsubSubtable` enum (was re-parsing every cursor × every subtable × every lookup), `GlyphIds` shadow buffer (was rebuilding via `iter().collect()` every match), run-level `would_apply` digest (skip-iterator-style cursor gate). Devanagari 173× → 4.98×, Arabic 32× → 2.72×, Khmer 17× → 1.88×. Latin (1.17× → 0.74×) and Hebrew (0.88× → 0.62×) collateral wins — both faster than rustybuzz.

### Companion crate `sigilbuzz-woff`

- [x] WOFF1 unwrap (uncompressed pass-through, zlib deferred) + uncompressed wrap.
- [x] WOFF2 unwrap with Brotli decompression and full `glyf` + `loca` inverse transform — 8-stream reconstruction (nContour / nPoints / flags / triplets / composite / bbox bitmap+stream / instructions / overlap-simple). `wrap_woff2` deferred to 0.7.0.
- [x] First runtime dep introduced — `brotli-decompressor`, gated behind the `woff2` feature, confined to `sigilbuzz-woff`. Justified in `docs/deps.md`. Shaping core stays zero-dep.

### Glyf phantom-anchor real-font integration

- [x] Hand-crafted 780-byte phantom-anchor fixture exercises the code path that 0.5.0 added but couldn't reach with the bundled corpus.

### Hardening

- [x] Wave 7 fuzz pass on the 0.6.0 surface (+3 fixes — Coverage emitter byte-waste on non-identity inputs (#94/#95); WOFF2 empty-glyph + bbox-bitmap mis-alignment (#96/#97); WOFF2 transformVersion validation on non-glyf/loca tags (#98/#99)).

---

## 0.7.0 (shipping)

**Headline:** the C-API shim. Eight other PRs ride alongside it covering renderer-output legacy formats, layout subset rewriting, complex-script breadth, and the WOFF2 wrap direction.

### Companion crate `sigilbuzz-capi` (HarfBuzz-symbol-compatible C shim)

- [x] cdylib + staticlib + rlib crate-types. `hb_*` symbol names match HarfBuzz exactly so a downstream binary swaps `-lharfbuzz` for `-lsigilbuzz` and recompiles with no source changes.
- [x] Required surface: `hb_blob_*`, `hb_face_*`, `hb_font_*` (with `set_scale` / `get_scale` / `set_ppem` / `set_variations`), `hb_buffer_*` (create / destroy / reference / reset / clear_contents / add_utf8 / set_direction / set_script / set_language / guess_segment_properties / get_glyph_infos / get_glyph_positions / get_length), `hb_shape`, `hb_shape_full`, `hb_tag_from_string` / `to_string`, `hb_direction_from_string`, `hb_script_from_iso15924_tag`, `hb_language_from_string`, `hb_version` (advertises 8.0.0 for ABI compat) / `hb_version_string`.
- [x] Lifetime erasure via Arc-pinned bytes + contained `transmute` to `Face<'static>`. No core-sigilbuzz API change required.
- [x] Hand-written `include/hb.h` subset, pkg-config template, CMake find-module.
- [x] Stretch (`hb_subset_*`, `hb_paint_*`, `hb_face_collect_unicodes`, `hb_ot_layout_collect_features`) deferred to small follow-up issues.

### Companion crate extensions

- [x] CFF / CFF2 byte-level emitter primitives + identity-passthrough — INDEX builder, DICT integer + 5-byte deferred-offset placeholder + patcher, charset format 0/2 with auto-pick, Encoding format 0/1 with auto-pick, callsubr/callgsubr in-place renumber. Non-identity orchestration tracked as #108 for 0.8.0+.
- [x] GSUB / GPOS / GDEF byte-level subset rewriter scaffold + GSUB type 1 + GSUB type 4 ligature + GDEF ClassDef. Closure walker pulls ligature result gids forward (Open Sans → {f, i} retains the `fi` ligature). Remaining lookup types tracked as #107 for 0.8.0+.
- [x] `wrap_woff2` — forward `glyf` + `loca` transform with triplet encoder + Brotli encode (52% compression on Open Sans Latin). Closes the wrap deferral from 0.6.0. Dep switched from `brotli-decompressor` to `brotli` (same maintainers, both directions).

### New tables

- [x] CBDT / CBLC bitmap font format (Google color-emoji) — formats 17 / 18 / 19, IndexSubTable formats 1-5, raw PNG bytes exposed via `Face::glyph_bitmap`.
- [x] sbix bitmap font format (Apple color-emoji) — strikes, graphicType tags (`'png '`, `'jpg '`, `'tiff'`, `'jp2 '`, `'dupe'`).
- [x] `SVG ` table — pre-COLRv1 color-emoji format. `Face::svg_document(gid)` returns the inline SVG bytes plus a gzip-magic flag. Bytes-only — decompression and SVG parsing are the consumer's job.

### Scripts

- [x] Tibetan shaper — feature-loop only (no reordering needed). Parity-clean on a 9-string Noto Serif Tibetan corpus.
- [x] Mongolian shaper — cursive joining state machine reusing Arabic's, plus Free Variation Selector handling and auto-vertical default.
- [x] Eight USE-eligible scripts on the existing state machine: N'Ko, Buginese, Tai Tham, Balinese, Sundanese, Lepcha, Limbu, Cham. Only ISC/IPC tables and `Script` enum routing change.

### Generic shaping correctness

- [x] HarfBuzz-style "each lookup once per pass" dedup added to `run_default_gsub` (surfaced by Mongolian double-apply; verified neutral on Arabic / USE corpora).

### Hardening

- [x] Wave 8 fuzz pass on the 0.7.0 surface — clean. Probed all 11 priority areas across the nine PRs (capi C ABI, CFF emitter primitives, layout rewriter scaffold, bitmap parsers, SVG-in-OT, wrap_woff2, lookup-dedup, Tibetan/Mongolian shapers, USE block boundaries) — no genuine bugs filed.

---

## 0.8.0 (shipping)

The biggest 0.8.0 surface across any release: the C-API stretch bridges, full byte-level subset rewriting for **every** GSUB and GPOS lookup type, complete CFF subsetting (CID + CFF2), the Brahmi USE family, generic shaping fixes, and three rounds of fuzz hardening.

### `sigilbuzz-capi` stretch surface (closes #102/#103/#104)

- [x] `hb_set_t` opaque set type (Arc-pinned `BTreeSet<u32>`) with create/destroy/reference/add/del/has/get_population/next.
- [x] `hb_subset_*` bridge to sigilbuzz-subset (`hb_subset_input_create/_destroy/_unicode_set/_glyph_set` + `hb_subset_or_fail`).
- [x] `hb_paint_funcs_t` with all 10 callback setters + `hb_font_paint_glyph` walking sigilbuzz-paint's DrawCmd stream.
- [x] `hb_face_collect_unicodes` + `hb_ot_layout_collect_features` introspection.

### Subsetting completes

- [x] **Full GSUB byte-level rewriter**: types 1 (#109), 2+3 (#121), 4 (#112), 5+6+8 (#127). Closure walker pulls forward through every substitution variant. Two-phase build_gsub driver with renumber-stable lookup-index propagation.
- [x] **Full GPOS byte-level rewriter**: types 1 / 2 fmt 1+2 / 3 / 4 / 5 / 6 / 7 / 8 / 9 (#128 + #131). PairPos fmt 2 uses adaptive class-collapse (small kept-set → fmt-1 fallback for correctness; large kept-set → fmt-2 pass-through for compactness).
- [x] **CFF subsetting end-to-end**: non-CID CFF1 (#120), CID-keyed CFF1 (#135), CFF2 non-identity (#135). FDArray + FDSelect rewrite + per-FD subr renumber.
- [x] Closes #87 (full layout rewriter), #92 (CFF emitter), #107 (per-lookup-type follow-ups), #108 (CFF orchestration), #122 (CID + CFF2), #126 (GPOS context).

### Brahmi USE family (#123)

- [x] Brahmi (U+11000), Sharada (U+11180), Khojki (U+11200), Tirhuta (U+11480), Modi (U+11600). All five with vendored OFL Noto fixtures and parity vs rustybuzz.
- [x] **`USE_BASIC_FEATURES` correction**: added `nukt` + `akhn` (missing previously). Generic improvement that benefits every Indic-style USE script.

### Generic shaping correctness fixes

- [x] **Mongolian apply_forward bug** (#118 / PR #134): `apply_parsed_lookup_at`'s Ligature arm advanced cursor by INPUT span, not OUTPUT span, after in-place buffer shrinkage. One-line fix in `src/shape.rs`. Affects any script using ligatures with marker glyphs in the run.
- [x] **Limbu mark-zero passes** (#115 / PR #124): dominant-script-gated EARLY/LATE/NONE timing for clearing mark advances. USE/Myanmar=EARLY, default/Arabic/Hebrew/Thai/Lao=LATE, Indic/Khmer/Hangul=NONE. General correctness — fixed via Limbu, benefits everywhere.
- [x] **Cham `pref` dispatch** (#116 / PR #124): split USE basic-feature dispatch so `pref` runs first, with per-syllable post-`pref` reorder. General USE improvement.
- [x] **N'Ko cursive joining** (#114 / PR #124): added N'Ko block to joining-type table, routed through Arabic's state machine via `nko ` script tag.

### Hardening — three regression waves

- [x] Wave 9 fuzz on 0.8.0 surface: 2 fixes — CFF charset format-2 emitter overflow on SID 0xFFFF (#129/#130); CFF Encoding emitters silent truncation past u8 count limit (#132/#133).

---

## 0.9.0+ (next)

- [ ] WOFF1 zlib compression (currently uncompressed pass-through; most producers use WOFF2, so low priority).
- [ ] Real-font CFF integration fixtures (Source Han Sans CN subset for CID, an OFL CFF2 VF) — synthetic fixtures already cover the orchestration; vendored fixtures would tighten the integration coverage.
- [ ] Cross-FD CFF subr sharing (currently raises Unsupported; rare in real-world fonts).
- [ ] AAT kerx state-machine formats 1 / 4 / 6 (currently only format 0 is recognised).

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
