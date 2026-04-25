# Performance baseline

First-run Criterion numbers captured against the 0.5.0 release tip on
the maintainer's macOS arm64 workstation. Re-run after any change that
touches a hot path and update this file together with the change so
the historical trend stays in one place.

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
| shape_latin         | 18.09 µs    | 15.51 µs    | 1.17x   | Open Sans, ASCII pangrams + liga/kern.                 |
| shape_arabic        | 3.63 ms     | 112 µs      | 32.4x   | Amiri, Quranic-grade rlig + IgnoreMarks. See #74.      |
| shape_devanagari    | 15.27 ms    | 88 µs       | 173x    | Noto Sans Devanagari, reph + conjuncts. See #75.       |
| shape_khmer         | 867 µs      | 50 µs       | 17.3x   | Noto Sans Khmer, USE state machine. See #76.           |
| shape_hebrew        | 15.91 µs    | 18.0 µs     | 0.88x   | Noto Sans Hebrew, GPOS mark-to-base/mark-to-mark.      |

### Follow-ups (>2x rustybuzz)

The complex-script benches expose three regressions that need
dedicated optimisation passes. Each has a tracking issue with the
candidate root causes:

- **Arabic 32x slower** — `https://github.com/Oneiriq/sigilbuzz/issues/74`
  GSUB cursor walker + LookupFlag skip iterators were already flagged
  as the heaviest path in 0.4.0; the bench confirms it.
- **Devanagari 173x slower** — `https://github.com/Oneiriq/sigilbuzz/issues/75`
  Syllable classifier + Indic feature dispatch chain.
- **Khmer 17x slower** — `https://github.com/Oneiriq/sigilbuzz/issues/76`
  USE category classifier + state machine.

Hebrew and Latin sit within ~1.2x of rustybuzz and are not blocking;
revisit if a future change knocks either above the 2x line.

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
