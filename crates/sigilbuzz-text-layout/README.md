# sigilbuzz-text-layout

Line-breaking, word-wrap, and word-segmentation for [sigilbuzz].

This companion crate implements a curated subset of
[UAX #14 *Unicode Line Breaking Algorithm*][uax14] sufficient to wrap
English, other European scripts, and CJK text correctly. It also offers
a simplified [UAX #29][uax29] word-segmentation iterator for callers
that want word boundaries (cursor movement, double-click selection)
without pulling in a full Unicode segmentation crate.

## Entry points

- `line_break_opportunities(text)` — UAX 14 break iterator.
- `wrap_lines(shaped, text, options)` — walk a `ShapedRun` and a
  width budget to produce `LineRange`s.
- `word_breaks(text)` — simplified UAX 29 word-segmentation iterator.

## Coverage

The line-break classifier covers the high-impact UAX 14 classes: BK,
CR, LF, NL, WJ, CL, CP, OP, QU, GL, NS, CM, SP, BA, BB, HY, AL, NU, PR,
PO, ID, EX, ZW, EB, EM. Brahmic combining marks, Korean Jamo
clustering, complex line-breaking for Southeast Asian scripts, and the
LB30a regional-indicator pair logic are deferred to a future release.

## License

Apache-2.0.

[sigilbuzz]: https://github.com/Oneiriq/sigilbuzz
[uax14]: https://www.unicode.org/reports/tr14/
[uax29]: https://www.unicode.org/reports/tr29/
