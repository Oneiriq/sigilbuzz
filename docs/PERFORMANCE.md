# Performance

Criterion numbers from the 0.6.0 release, measured on the maintainer's macOS arm64
workstation. If you change a hot path, re-run the benchmarks and update this file in
the same change, so the history stays in one place.

## Running the benchmarks

```sh
# Every benchmark in the workspace (Latin, Arabic, Devanagari, Khmer, Hebrew, plus
# the GPU encoder and COLRv1 evaluator):
cargo bench --workspace

# A single benchmark:
cargo bench --bench shape_arabic
cargo bench -p sigilbuzz-gpu --bench encode
cargo bench -p sigilbuzz-paint --bench evaluate

# A quick pass (Criterion's --quick mode, about one second per measurement):
cargo bench --bench shape_latin -- --quick
```

Each shaping benchmark runs the same 200-codepoint text through sigilbuzz and
rustybuzz back to back. Absolute times depend on the machine. The ratio against
rustybuzz is the number to watch, because it stays stable across machines.

## Shaping: sigilbuzz vs rustybuzz

| Benchmark | sigilbuzz | rustybuzz | Ratio | Notes |
|---|---|---|---|---|
| shape_latin | 11.39 µs | 15.03 µs | 0.76x | Open Sans, ASCII pangrams with liga and kern. |
| shape_arabic | 290 µs | 108 µs | 2.70x | Amiri, heavy rlig use with IgnoreMarks. |
| shape_devanagari | 419 µs | 85.8 µs | 4.89x | Noto Sans Devanagari, reph and conjuncts. |
| shape_khmer | 91.6 µs | 49.0 µs | 1.87x | Noto Sans Khmer, USE state machine. |
| shape_hebrew | 10.99 µs | 17.28 µs | 0.64x | Noto Sans Hebrew, GPOS mark-to-base and mark-to-mark. |

A ratio below 1 means sigilbuzz is faster.

### What changed in 0.6.0 (#74, #75, #76)

In 0.5.0, Devanagari took 173x as long as rustybuzz, Arabic 32x, and Khmer 17x. Every
complex-script benchmark was an outlier for algorithmic reasons. Three fixes in
`src/shape.rs` and `src/tables/gsub/*.rs` brought each of them under 5x and left Latin
and Hebrew faster than rustybuzz:

1. Build the glyph ID snapshot once. The context, chained-context, and reverse-chain
   matchers used to rebuild a `Vec<u16>` of every glyph ID at every cursor step. Now
   `apply_gsub_lookup` builds it once (`GlyphIds`) and updates it as it goes. Single and
   Alternate substitutions touch one slot. Ligature and Multiple resync. Devanagari
   -17%.

2. Parse each subtable once per lookup. Parsing a format 3 context subtable allocates
   four `Vec`s, and the cursor walk used to pay that at every position. The parsed form
   (`ParsedGsubSubtable`) is now built once when `apply_gsub_lookup` starts and reused
   for the whole walk. Devanagari -94%, Arabic -86%, Khmer -75%.

3. Skip lookups that can't apply. Each lookup first checks whether any glyph in the
   run is in any subtable's coverage. During the walk, only positions whose glyph is in
   that set get full subtable dispatch. The rest skip after one `cov.contains` check
   per subtable. This mirrors HarfBuzz's skip-iterator check. Devanagari -23%, Arabic
   -37%, Khmer -47%.

The chained-context matchers also check `input[0]` before walking the backtrack
sequence, as HarfBuzz does. The per-syllable `compute_half_mask` dry run now caches
`feature_would_substitute` results, so repeated `(halant, c2)` pairs don't walk GSUB
again.

Latin and Hebrew got faster as a side effect. The cursor walk savings also apply to
the default `liga`, `calt`, and `clig` features and to `mark` and `mkmk` in GPOS.

## GPU encoder (`sigilbuzz-gpu`)

rustybuzz has no Slug-style outline encoder, so these numbers are only compared with
earlier sigilbuzz-gpu runs.

| Glyph | Mean | Notes |
|---|---|---|
| opensans_A | 852 ns | Simple glyph, mostly straight lines. |
| opensans_g | 1.78 µs | Descender and a closed two-story bowl. |
| opensans_O | 979 ns | An oval, all four curved quadrants. |
| amiri_alef | 896 ns | Long vertical stroke with a thin tail. |

## COLRv1 evaluator (`sigilbuzz-paint`)

rustybuzz does not evaluate COLRv1, so these numbers are only compared with earlier
sigilbuzz-paint runs.

| Fixture | Mean |
|---|---|
| solid | 58 ns |
| linear_gradient | 92 ns |
| translate_scale | 92 ns |
| composite | 86 ns |
| radial_gradient | 86 ns |
| sweep_gradient | 97 ns |
| var_solid_at_coords | 69 ns |

## Reading the Criterion output

Each shaping benchmark defines two entries side by side, `shape_<script>/sigilbuzz`
and `shape_<script>/rustybuzz`, so one `cargo bench` run reports both without a second
toolchain or binary. The ratios above come from the median times of each pair.
