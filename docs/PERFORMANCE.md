# Performance

Numbers for 0.24.0. The per-call shaping tables come from the measurements in the
0.24.0 pull requests (#295 and #301), which compare sigilbuzz with 0.23.0 and with
HarfBuzz 14.5.0. The Criterion numbers further down were rerun for this release. If you
change a hot path, measure again and update this file in the same change, so the history
stays in one place. Numbers from earlier releases are in this file's git history.

## How 0.24.0 shapes faster

A `Font` keeps what HarfBuzz keeps per face and font, from its second shaping call on:

- a glyph digest per lookup and subtable, so a lookup that cannot apply to any glyph of
  the run is skipped before it is parsed, and subtables are parsed only when a glyph
  their digest admits reaches them;
- the language system each script, language and FeatureVariations record resolves to;
- the merged lookups of each shaping stage;
- per-glyph vertical origins and varied advances (`HVAR`, `VVAR` or phantom points).

Clones made after a font's first call share these caches. A font's first call builds
none of them, so code that builds a `Font` for every run gets only part of the gain.
Keep one `Font` per font, size and instance. The Indic, Khmer, Myanmar and USE syllable
grammars are compiled once per process. Output never depends on the caches.

## Per-call shaping

### Method

- One Windows 11 laptop (Intel i9-11900H), shared with other jobs at about 25%
  background load, so differences under about 4% are noise.
- Release builds with `lto = "thin"` and `codegen-units = 1`, one sigilbuzz version per
  binary, as a user links it.
- Each row shapes 20 texts of 1, 8 or 100 characters (Hangul syllables, Latin letters
  and spaces, Arabic letters, Devanagari words) with no features, either after
  `guess_segment_properties` (LTR: horizontal, right to left for Arabic) or top to
  bottom (TTB).
- A run reports the median thread-cycle time of 11 batches of about 10 ms. The versions
  of a row run in alternation, and a cell is the median over 5 runs (3 for first
  calls). Times are microseconds per call.
- The 0.23.0 and 0.24.0 columns, and the hostile-table numbers below, were measured
  for #301 on its branch. Its last two commits came after: one changes only first
  calls of lookups with more than 1,024 subtables, which were measured again and were
  within noise, and one caps how much room a lookup's parsed subtables reserve, which
  was not timed again.
- HarfBuzz is 14.5.0 through uharfbuzz 0.56.2, measured for #295 on the same laptop:
  the median of 15 wall-clock batches from Python, best of 2. Each HarfBuzz number
  includes about 0.5 to 1 us of Python per call, so HarfBuzz's own time is that much
  lower.

### Repeated calls on one `Font`

This is the case the caches are for, and what an application that keeps its fonts
sees.

| Font | Axes | Dir | Chars | 0.23.0 | 0.24.0 | HarfBuzz 14.5.0 |
|---|---|---|---|---|---|---|
| Noto Sans KR VF otf (CFF2, VORG) | default | LTR | 1 | 56.51 | 3.38 | 1.13 |
| Noto Sans KR VF otf (CFF2, VORG) | default | LTR | 8 | 70.60 | 7.73 | 1.82 |
| Noto Sans KR VF otf (CFF2, VORG) | default | LTR | 100 | 228.42 | 42.49 | 20.51 |
| Noto Sans KR VF otf (CFF2, VORG) | default | TTB | 1 | 59.31 | 3.42 | 1.85 |
| Noto Sans KR VF otf (CFF2, VORG) | default | TTB | 8 | 66.63 | 6.38 | 3.08 |
| Noto Sans KR VF otf (CFF2, VORG) | default | TTB | 100 | 222.00 | 36.70 | 25.30 |
| Noto Sans KR VF otf (CFF2, VORG) | wght=700 | LTR | 1 | 58.89 | 3.47 | 1.90 |
| Noto Sans KR VF otf (CFF2, VORG) | wght=700 | LTR | 8 | 71.06 | 8.86 | 2.29 |
| Noto Sans KR VF otf (CFF2, VORG) | wght=700 | LTR | 100 | 246.20 | 47.66 | 19.61 |
| Noto Sans KR VF otf (CFF2, VORG) | wght=700 | TTB | 1 | 57.70 | 3.61 | 1.63 |
| Noto Sans KR VF otf (CFF2, VORG) | wght=700 | TTB | 8 | 71.24 | 7.47 | 2.61 |
| Noto Sans KR VF otf (CFF2, VORG) | wght=700 | TTB | 100 | 266.18 | 54.90 | 30.32 |
| Noto Sans KR VF ttf (glyf, vmtx) | default | LTR | 1 | 57.47 | 3.21 | 1.27 |
| Noto Sans KR VF ttf (glyf, vmtx) | default | LTR | 8 | 92.63 | 8.12 | 1.85 |
| Noto Sans KR VF ttf (glyf, vmtx) | default | LTR | 100 | 220.00 | 44.09 | 16.01 |
| Noto Sans KR VF ttf (glyf, vmtx) | default | TTB | 1 | 58.17 | 3.30 | 1.93 |
| Noto Sans KR VF ttf (glyf, vmtx) | default | TTB | 8 | 66.09 | 6.32 | 2.44 |
| Noto Sans KR VF ttf (glyf, vmtx) | default | TTB | 100 | 231.34 | 37.37 | 27.33 |
| Noto Sans KR VF ttf (glyf, vmtx) | wght=700 | LTR | 1 | 57.77 | 3.46 | 1.24 |
| Noto Sans KR VF ttf (glyf, vmtx) | wght=700 | LTR | 8 | 66.77 | 7.77 | 1.88 |
| Noto Sans KR VF ttf (glyf, vmtx) | wght=700 | LTR | 100 | 253.46 | 49.81 | 22.09 |
| Noto Sans KR VF ttf (glyf, vmtx) | wght=700 | TTB | 1 | 56.65 | 3.67 | 2.08 |
| Noto Sans KR VF ttf (glyf, vmtx) | wght=700 | TTB | 8 | 85.79 | 7.68 | 3.62 |
| Noto Sans KR VF ttf (glyf, vmtx) | wght=700 | TTB | 100 | 315.74 | 50.65 | 62.87 |
| Hahmlet (glyf, no vmtx) | wght=700 | LTR | 1 | 13.19 | 2.97 | 1.63 |
| Hahmlet (glyf, no vmtx) | wght=700 | LTR | 8 | 16.96 | 4.59 | 1.75 |
| Hahmlet (glyf, no vmtx) | wght=700 | LTR | 100 | 90.39 | 22.29 | 15.19 |
| Hahmlet (glyf, no vmtx) | wght=700 | TTB | 1 | 7.07 | 2.99 | 1.45 |
| Hahmlet (glyf, no vmtx) | wght=700 | TTB | 8 | 10.12 | 4.39 | 7.87 |
| Hahmlet (glyf, no vmtx) | wght=700 | TTB | 100 | 66.91 | 20.13 | 227.45 |
| Source Code Pro (CFF, no VORG) | default | LTR | 1 | 1.02 | 1.11 | 1.08 |
| Source Code Pro (CFF, no VORG) | default | LTR | 8 | 1.61 | 1.71 | 2.23 |
| Source Code Pro (CFF, no VORG) | default | LTR | 100 | 7.05 | 8.06 | 5.15 |
| Source Code Pro (CFF, no VORG) | default | TTB | 1 | 1.12 | 1.76 | 1.78 |
| Source Code Pro (CFF, no VORG) | default | TTB | 8 | 1.87 | 1.88 | 1.41 |
| Source Code Pro (CFF, no VORG) | default | TTB | 100 | 9.54 | 8.57 | 5.22 |
| Source Serif 4 VF (CFF2, no VORG) | default | LTR | 1 | 9.76 | 3.64 | 1.67 |
| Source Serif 4 VF (CFF2, no VORG) | default | LTR | 8 | 14.24 | 6.90 | 2.41 |
| Source Serif 4 VF (CFF2, no VORG) | default | LTR | 100 | 72.64 | 43.68 | 10.85 |
| Source Serif 4 VF (CFF2, no VORG) | default | TTB | 1 | 7.14 | 2.81 | 1.47 |
| Source Serif 4 VF (CFF2, no VORG) | default | TTB | 8 | 10.58 | 4.68 | 1.65 |
| Source Serif 4 VF (CFF2, no VORG) | default | TTB | 100 | 49.00 | 22.83 | 4.78 |
| Source Serif 4 VF (CFF2, no VORG) | wght=700 | LTR | 1 | 9.84 | 3.95 | 1.32 |
| Source Serif 4 VF (CFF2, no VORG) | wght=700 | LTR | 8 | 18.96 | 8.15 | 2.21 |
| Source Serif 4 VF (CFF2, no VORG) | wght=700 | LTR | 100 | 118.53 | 62.23 | 13.48 |
| Source Serif 4 VF (CFF2, no VORG) | wght=700 | TTB | 1 | 7.64 | 3.11 | 1.51 |
| Source Serif 4 VF (CFF2, no VORG) | wght=700 | TTB | 8 | 13.03 | 4.71 | 1.77 |
| Source Serif 4 VF (CFF2, no VORG) | wght=700 | TTB | 100 | 77.40 | 23.66 | 6.33 |
| Segoe UI | default | LTR | 1 | 7.08 | 2.29 | 1.47 |
| Segoe UI | default | LTR | 8 | 10.12 | 3.78 | 2.32 |
| Segoe UI | default | LTR | 100 | 53.78 | 21.86 | 5.63 |
| Segoe UI | default | TTB | 1 | 6.33 | 2.15 | 2.12 |
| Segoe UI | default | TTB | 8 | 9.75 | 3.24 | 1.86 |
| Segoe UI | default | TTB | 100 | 50.46 | 16.35 | 4.80 |
| Roboto | default | LTR | 1 | 5.41 | 2.68 | 1.29 |
| Roboto | default | LTR | 8 | 8.17 | 5.72 | 2.16 |
| Roboto | default | LTR | 100 | 42.36 | 36.23 | 9.67 |
| Roboto | default | TTB | 1 | 4.19 | 2.14 | 1.63 |
| Roboto | default | TTB | 8 | 6.08 | 4.01 | 1.58 |
| Roboto | default | TTB | 100 | 28.50 | 20.18 | 3.71 |
| Amiri (Arabic) | default | LTR | 1 | 58.51 | 9.63 | 1.63 |
| Amiri (Arabic) | default | LTR | 8 | 90.63 | 30.44 | 8.14 |
| Amiri (Arabic) | default | LTR | 100 | 413.22 | 208.64 | 78.07 |
| Amiri (Arabic) | default | TTB | 1 | 46.15 | 4.17 | 1.60 |
| Amiri (Arabic) | default | TTB | 8 | 56.94 | 8.53 | 4.09 |
| Amiri (Arabic) | default | TTB | 100 | 216.28 | 49.65 | 18.13 |
| Noto Sans Devanagari | default | LTR | 1 | 370.87 | 10.12 | 1.98 |
| Noto Sans Devanagari | default | LTR | 8 | 438.20 | 29.12 | 5.38 |
| Noto Sans Devanagari | default | LTR | 100 | 806.56 | 204.08 | 52.49 |
| Noto Sans Devanagari | default | TTB | 1 | 360.25 | 9.98 | 2.59 |
| Noto Sans Devanagari | default | TTB | 8 | 422.78 | 37.47 | 6.88 |
| Noto Sans Devanagari | default | TTB | 100 | 799.29 | 206.75 | 55.33 |

Long runs still take 2 to 5 times as long per glyph as in HarfBuzz: normalization,
Hangul composition and PairPos walks did not change in 0.24.0.

### A fresh `Face` and `Font` for every call

The first-call path, for code that builds a `Font` per run. A font's first
top-to-bottom call is slower than in 0.23.0 for fonts without `VORG` (Hahmlet, Source
Serif 4 VF, Source Code Pro), because their vertical origins come from glyph extents
since 0.23.1, and a first call works those out for every new glyph.

<details><summary>Full table</summary>

| Font | Axes | Dir | Chars | 0.23.0 | 0.24.0 |
|---|---|---|---|---|---|
| Noto Sans KR VF otf (CFF2, VORG) | default | LTR | 1 | 56.45 | 12.74 |
| Noto Sans KR VF otf (CFF2, VORG) | default | LTR | 8 | 63.65 | 25.76 |
| Noto Sans KR VF otf (CFF2, VORG) | default | LTR | 100 | 218.88 | 163.43 |
| Noto Sans KR VF otf (CFF2, VORG) | default | TTB | 1 | 56.43 | 12.54 |
| Noto Sans KR VF otf (CFF2, VORG) | default | TTB | 8 | 65.47 | 23.08 |
| Noto Sans KR VF otf (CFF2, VORG) | default | TTB | 100 | 216.12 | 170.51 |
| Noto Sans KR VF otf (CFF2, VORG) | wght=700 | LTR | 1 | 59.94 | 13.55 |
| Noto Sans KR VF otf (CFF2, VORG) | wght=700 | LTR | 8 | 67.88 | 26.94 |
| Noto Sans KR VF otf (CFF2, VORG) | wght=700 | LTR | 100 | 244.63 | 175.47 |
| Noto Sans KR VF otf (CFF2, VORG) | wght=700 | TTB | 1 | 53.85 | 13.17 |
| Noto Sans KR VF otf (CFF2, VORG) | wght=700 | TTB | 8 | 68.31 | 25.68 |
| Noto Sans KR VF otf (CFF2, VORG) | wght=700 | TTB | 100 | 240.42 | 190.12 |
| Noto Sans KR VF ttf (glyf, vmtx) | default | LTR | 1 | 56.16 | 13.46 |
| Noto Sans KR VF ttf (glyf, vmtx) | default | LTR | 8 | 68.36 | 24.07 |
| Noto Sans KR VF ttf (glyf, vmtx) | default | LTR | 100 | 211.36 | 159.16 |
| Noto Sans KR VF ttf (glyf, vmtx) | default | TTB | 1 | 54.41 | 12.87 |
| Noto Sans KR VF ttf (glyf, vmtx) | default | TTB | 8 | 65.31 | 23.67 |
| Noto Sans KR VF ttf (glyf, vmtx) | default | TTB | 100 | 212.30 | 172.75 |
| Noto Sans KR VF ttf (glyf, vmtx) | wght=700 | LTR | 1 | 54.40 | 13.41 |
| Noto Sans KR VF ttf (glyf, vmtx) | wght=700 | LTR | 8 | 68.45 | 26.02 |
| Noto Sans KR VF ttf (glyf, vmtx) | wght=700 | LTR | 100 | 244.49 | 184.43 |
| Noto Sans KR VF ttf (glyf, vmtx) | wght=700 | TTB | 1 | 54.41 | 13.89 |
| Noto Sans KR VF ttf (glyf, vmtx) | wght=700 | TTB | 8 | 81.58 | 30.00 |
| Noto Sans KR VF ttf (glyf, vmtx) | wght=700 | TTB | 100 | 347.54 | 254.19 |
| Hahmlet (glyf, no vmtx) | wght=700 | LTR | 1 | 13.33 | 9.58 |
| Hahmlet (glyf, no vmtx) | wght=700 | LTR | 8 | 17.70 | 11.82 |
| Hahmlet (glyf, no vmtx) | wght=700 | LTR | 100 | 91.97 | 74.23 |
| Hahmlet (glyf, no vmtx) | wght=700 | TTB | 1 | 7.09 | 13.33 |
| Hahmlet (glyf, no vmtx) | wght=700 | TTB | 8 | 10.53 | 41.80 |
| Hahmlet (glyf, no vmtx) | wght=700 | TTB | 100 | 69.16 | 510.91 |
| Source Code Pro (CFF, no VORG) | default | LTR | 1 | 1.12 | 1.20 |
| Source Code Pro (CFF, no VORG) | default | LTR | 8 | 1.70 | 1.83 |
| Source Code Pro (CFF, no VORG) | default | LTR | 100 | 7.21 | 8.01 |
| Source Code Pro (CFF, no VORG) | default | TTB | 1 | 1.23 | 1.97 |
| Source Code Pro (CFF, no VORG) | default | TTB | 8 | 1.94 | 4.35 |
| Source Code Pro (CFF, no VORG) | default | TTB | 100 | 9.67 | 24.41 |
| Source Serif 4 VF (CFF2, no VORG) | default | LTR | 1 | 9.07 | 6.47 |
| Source Serif 4 VF (CFF2, no VORG) | default | LTR | 8 | 13.92 | 11.60 |
| Source Serif 4 VF (CFF2, no VORG) | default | LTR | 100 | 69.69 | 54.30 |
| Source Serif 4 VF (CFF2, no VORG) | default | TTB | 1 | 7.69 | 9.61 |
| Source Serif 4 VF (CFF2, no VORG) | default | TTB | 8 | 11.31 | 27.84 |
| Source Serif 4 VF (CFF2, no VORG) | default | TTB | 100 | 50.00 | 160.56 |
| Source Serif 4 VF (CFF2, no VORG) | wght=700 | LTR | 1 | 9.56 | 7.03 |
| Source Serif 4 VF (CFF2, no VORG) | wght=700 | LTR | 8 | 19.12 | 16.12 |
| Source Serif 4 VF (CFF2, no VORG) | wght=700 | LTR | 100 | 124.17 | 104.79 |
| Source Serif 4 VF (CFF2, no VORG) | wght=700 | TTB | 1 | 8.78 | 10.55 |
| Source Serif 4 VF (CFF2, no VORG) | wght=700 | TTB | 8 | 14.39 | 34.31 |
| Source Serif 4 VF (CFF2, no VORG) | wght=700 | TTB | 100 | 81.98 | 189.34 |
| Segoe UI | default | LTR | 1 | 7.08 | 6.35 |
| Segoe UI | default | LTR | 8 | 10.76 | 9.35 |
| Segoe UI | default | LTR | 100 | 58.14 | 44.59 |
| Segoe UI | default | TTB | 1 | 6.58 | 5.55 |
| Segoe UI | default | TTB | 8 | 9.95 | 8.15 |
| Segoe UI | default | TTB | 100 | 52.04 | 36.11 |
| Roboto | default | LTR | 1 | 5.84 | 5.62 |
| Roboto | default | LTR | 8 | 8.24 | 8.89 |
| Roboto | default | LTR | 100 | 42.39 | 47.02 |
| Roboto | default | TTB | 1 | 4.58 | 3.90 |
| Roboto | default | TTB | 8 | 7.23 | 6.03 |
| Roboto | default | TTB | 100 | 29.56 | 31.59 |
| Amiri (Arabic) | default | LTR | 1 | 63.74 | 17.53 |
| Amiri (Arabic) | default | LTR | 8 | 88.06 | 50.56 |
| Amiri (Arabic) | default | LTR | 100 | 455.94 | 388.00 |
| Amiri (Arabic) | default | TTB | 1 | 46.51 | 11.32 |
| Amiri (Arabic) | default | TTB | 8 | 58.75 | 22.69 |
| Amiri (Arabic) | default | TTB | 100 | 213.42 | 164.33 |
| Noto Sans Devanagari | default | LTR | 1 | 370.78 | 45.64 |
| Noto Sans Devanagari | default | LTR | 8 | 464.15 | 70.46 |
| Noto Sans Devanagari | default | LTR | 100 | 847.40 | 499.24 |
| Noto Sans Devanagari | default | TTB | 1 | 380.26 | 40.97 |
| Noto Sans Devanagari | default | TTB | 8 | 421.72 | 68.84 |
| Noto Sans Devanagari | default | TTB | 100 | 776.64 | 498.88 |

</details>

### Hostile layout tables

Open Sans with its GSUB or GPOS replaced by a crafted table, measured for #301. Time per
call on one `Font` (a first call in parentheses): the median of 9 alternating runs for
the rows in milliseconds, and the range of four single calls for the rows in seconds.
Output is the same in both versions.

| Table | Text | 0.23.1 | 0.24.0 |
|---|---|---|---|
| GSUB, 32,000 offsets to one ChainContextFormat2 (64 KB) | aaaa | 5.19 ms (4.93) | 5.49 ms (5.47) |
| GSUB, 8,000 of them (16 KB) | aaaa | 1.31 ms (1.36) | 1.37 ms (1.44) |
| GSUB, 32,000 offsets to one ChainContextFormat3 (64 KB) | a | 6.80 ms (6.56) | 6.10 ms (6.49) |
| GSUB, 32,000 offsets to one ChainContextFormat1 (64 KB) | aaaa | 3.99 ms (4.02) | 4.32 ms (4.37) |
| GPOS, 32,000 offsets to one ChainContextPos format 2 (64 KB) | aaaa | 4.85 ms (5.06) | 5.30 ms (5.61) |
| GSUB, 500 indices of one lookup of 32,000 format 3 subtables (66 KB) | a | 5.6 to 7.0 s | 4.5 to 5.9 s |
| GPOS, the same with ChainContextPos (66 KB) | a | 6.1 to 6.7 s | 4.1 to 4.7 s |
| GSUB, 8,000 overlapping lookups of 8,000 subtables (96 KB) | a | 1.6 to 2.0 s | 2.2 to 2.5 s |

Heap over three calls on one `Font`, measured with a counting allocator: the peak
during the calls, and what the `Font` keeps after them.

| Table | 0.23.1 peak / kept | 0.24.0 peak / kept |
|---|---|---|
| GSUB, 32,000 x ChainContextFormat2 | 5.4 MB / 0 | 5.5 MB / 2 KB |
| GPOS, 32,000 x format 2 | 5.6 MB / 0 | 5.8 MB / 3 KB |
| GSUB, 500 indices x 32,000 format 3 | 7.3 MB / 0 | 7.5 MB / 15 KB |
| GPOS, 500 indices x 32,000 format 3 | 7.6 MB / 0 | 7.7 MB / 15 KB |
| GSUB, 8,000 overlapping lookups | 1.4 MB / 0 | 7.8 MB / 6.3 MB |
| GSUB, 16 lookups x 2,000 distinct digests | 0.3 MB / 0 | 0.7 MB / 0.7 MB |

What a `Font` keeps per GSUB or GPOS table is one slot pointer per lookup, a 40-byte
header per lookup applied, and 24 bytes per kept digest, for at most twice the table's
length plus 64 Ki digests. On top of that come at most 16 resolved language systems and
64 stage plans per table, and three per-glyph caches of at most 4,096 four-byte entries
each. The overlapping-lookups table is the one case only that budget bounds: 6.3 MB
kept for a 96 KB table.

### Outlines

`Face::glyph_outlines` against `Face::glyph_outline_at_coords`, per glyph over 100
consecutive glyphs, measured for #295:

| Font | `glyph_outline_at_coords` | `GlyphOutlines` |
|---|---|---|
| Source Serif 4 VF (CFF2), default | 5.12 us | 3.74 us |
| Source Serif 4 VF (CFF2), wght 700 | 5.29 us | 3.68 us |
| Noto Sans KR VF otf (CFF2) | 2.98 us | 2.09 us |
| Source Code Pro (CFF) | 0.75 us | 0.44 us |

## Criterion benchmarks

### Running them

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

Each shaping benchmark shapes the same 200-codepoint text through sigilbuzz and
rustybuzz back to back. sigilbuzz shapes on one `Font`, so its caches are warm after
the first iteration. Absolute times depend on the machine. The ratio against rustybuzz
is the number to watch, because it moves less across machines.

### Results for 0.24.0

Measured on 2026-10-04 at the 0.24.0 release commit with `cargo bench --workspace
--bench shape_latin --bench shape_arabic --bench shape_devanagari --bench shape_khmer
--bench shape_hebrew --bench encode --bench evaluate`, Criterion's default settings
(3 s warm-up, 100 samples), on the laptop above. Each time is the middle of Criterion's
estimate. Other jobs ran on the laptop at the same time (12% to 25% CPU load at the
start and end of the run). A first run under full load took up to twice as long, and
its ratios differed by up to a factor of two (Khmer 5.5x instead of 2.45x), so read the
ratios as approximate and rerun on a quiet machine before comparing releases. The
0.6.0 numbers in this file's history came from a macOS arm64 workstation, so their
absolute times do not compare with these.

| Benchmark | sigilbuzz | rustybuzz | Ratio | Notes |
|---|---|---|---|---|
| shape_latin | 50.9 µs | 37.5 µs | 1.36x | Open Sans, ASCII pangrams with liga and kern. |
| shape_arabic | 455 µs | 263 µs | 1.73x | Amiri, heavy rlig use with IgnoreMarks. |
| shape_devanagari | 772 µs | 250 µs | 3.09x | Noto Sans Devanagari, reph and conjuncts. |
| shape_khmer | 342 µs | 140 µs | 2.45x | Noto Sans Khmer, Khmer shaper. |
| shape_hebrew | 62.1 µs | 41.4 µs | 1.50x | Noto Sans Hebrew, GPOS mark-to-base and mark-to-mark. |

A ratio below 1 means sigilbuzz is faster.

GPU encoder (`sigilbuzz-gpu`). rustybuzz has no Slug-style outline encoder, so these
numbers are only compared with earlier sigilbuzz-gpu runs.

| Glyph | Time | Notes |
|---|---|---|
| opensans_A | 3.57 µs | Simple glyph, mostly straight lines. |
| opensans_g | 6.09 µs | Descender and a closed two-story bowl. |
| opensans_O | 3.39 µs | An oval, all four curved quadrants. |
| amiri_alef | 3.09 µs | Long vertical stroke with a thin tail. |

COLRv1 evaluator (`sigilbuzz-paint`). rustybuzz does not evaluate COLRv1, so these
numbers are only compared with earlier sigilbuzz-paint runs.

| Fixture | Time |
|---|---|
| solid | 488 ns |
| linear_gradient | 696 ns |
| translate_scale | 548 ns |
| composite | 692 ns |
| radial_gradient | 647 ns |
| sweep_gradient | 656 ns |
| var_solid_at_coords | 410 ns |

### Reading the Criterion output

Each shaping benchmark defines two entries side by side, `shape_<script>/sigilbuzz`
and `shape_<script>/rustybuzz`, so one `cargo bench` run reports both without a second
toolchain or binary. The ratios above come from the middle estimates of each pair.
