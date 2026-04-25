# Performance baseline

Criterion numbers from the 0.6.0 release-prep tip on the maintainer's
macOS arm64 workstation. Re-run after any change that touches a hot
path and update this file together with the change so the historical
trend stays in one place.

## How to reproduce

```sh
# All shape benches (Latin / Arabic / Devanagari / Khmer / Hebrew):
cargo bench --workspace

# A single bench:
cargo bench --bench shape_arabic
cargo bench -p sigilbuzz-gpu --bench encode
cargo bench -p sigilbuzz-paint --bench evaluate

# Quick sanity-check pass (Criterion --quick mode, ~1s per measurement):
cargo bench --bench shape_latin -- --quick
```

Each shape bench runs the same 200-codepoint corpus through both
sigilbuzz and rustybuzz back-to-back so the ratio column below is
the load-bearing number — absolute timings shift with the host
machine, but the ratio against rustybuzz is stable.

## Shape benches (sigilbuzz vs rustybuzz)

| bench               | sigilbuzz   | rustybuzz   | ratio   | notes                                                  |
|---------------------|-------------|-------------|---------|--------------------------------------------------------|
| shape_latin         | 11.39 µs    | 15.03 µs    | 0.76x   | Open Sans, ASCII pangrams + liga/kern.                 |
| shape_arabic        | 290 µs      | 108 µs      | 2.70x   | Amiri, Quranic-grade rlig + IgnoreMarks.               |
| shape_devanagari    | 419 µs      | 85.8 µs     | 4.89x   | Noto Sans Devanagari, reph + conjuncts.                |
| shape_khmer         | 91.6 µs     | 49.0 µs     | 1.87x   | Noto Sans Khmer, USE state machine.                    |
| shape_hebrew        | 10.99 µs    | 17.28 µs    | 0.64x   | Noto Sans Hebrew, GPOS mark-to-base/mark-to-mark.      |

### 0.6.0 perf pass (PR closing #74 / #75 / #76)

The 0.5.0 numbers above had Devanagari at 173x, Arabic at 32x, Khmer
at 17x — every complex-script bench was an algorithmic outlier. Three
fixes in `src/shape.rs` and `src/tables/gsub/*.rs` collapsed each
into the < 5x rustybuzz envelope while leaving Latin and Hebrew
faster than rustybuzz:

1. **Hoist GSUB-id snapshot.** Context / chained-context / reverse-
   chain matchers used to rebuild a `Vec<u16>` of every glyph id on
   every cursor step. Now built once per `apply_gsub_lookup` call
   (`GlyphIds`) and updated incrementally — Single/Alternate touch
   one slot, Ligature/Multiple resync. Devanagari -17%.

2. **Pre-parse subtables once per lookup.** ChainContextAny/
   GsubContext format-3 parsing allocates four `Vec`s per call;
   the cursor walk was paying that cost at every `at`. The parsed
   form (`ParsedGsubSubtable`) is now constructed once at the
   `apply_gsub_lookup` entry and reused across the whole cursor
   walk. Devanagari -94%, Arabic -86%, Khmer -75%.

3. **Run-level "would_apply" + per-cursor digest.** Each lookup
   asks "any glyph in the run in any subtable's primary coverage?"
   before walking. Within the walk, only cursors whose glyph is in
   the digest get full subtable dispatch; the rest skip with a
   single `cov.contains` per parsed subtable. Mirrors HarfBuzz's
   skip-iterator gate. Devanagari -23%, Arabic -37%, Khmer -47%.

The chain-context matchers were also reordered to check input[0]
before walking backtrack (HarfBuzz semantics), and the per-syllable
`compute_half_mask` dry-run memoises `feature_would_substitute`
results so repeated `(halant, c2)` pairs don't re-walk GSUB.

Latin and Hebrew benefit indirectly: the cursor walk savings apply
to default `liga`/`calt`/`clig` and `mark`/`mkmk` GPOS as well —
both ratios now sit *below* rustybuzz on this corpus.

## sigilbuzz-gpu encode bench

No rustybuzz comparison — rustybuzz does not ship a Slug-style
outline encoder. Tracked against historical sigilbuzz-gpu runs.

| glyph         | mean    | notes                                  |
|---------------|---------|----------------------------------------|
| opensans_A    | 852 ns  | Simple base glyph, mostly straight.    |
| opensans_g    | 1.78 µs | Descender + closed two-storey curve.   |
| opensans_O    | 979 ns  | Pure oval, all four cubic quadrants.   |
| amiri_alef    | 896 ns  | Long vertical with thin tail.          |

## sigilbuzz-paint evaluate bench

No rustybuzz comparison — COLRv1 evaluation is not part of
rustybuzz. Tracked against historical sigilbuzz-paint runs.

| fixture                | mean   |
|------------------------|--------|
| solid                  | 58 ns  |
| linear_gradient        | 92 ns  |
| translate_scale        | 92 ns  |
| composite              | 86 ns  |
| radial_gradient        | 86 ns  |
| sweep_gradient         | 97 ns  |
| var_solid_at_coords    | 69 ns  |

## Comparing against rustybuzz directly

Each shape bench produces two Criterion entries side-by-side
(`shape_<script>/sigilbuzz` and `shape_<script>/rustybuzz`) so a
single `cargo bench` run reports both numbers without needing a
second toolchain or a different binary. The ratios above were
computed from the median timings of the matched groups.
