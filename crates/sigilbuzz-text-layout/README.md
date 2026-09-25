# sigilbuzz-text-layout

Line breaking, word wrap, and word boundaries for [sigilbuzz].

It implements the parts of [UAX 14, the Unicode Line Breaking Algorithm][uax14], that
you need to wrap English, other European languages, and CJK text correctly. It also has
a simplified [UAX 29][uax29] word iterator for cursor movement and double-click
selection, so you don't need a full Unicode segmentation crate for that.

## Entry points

- `line_break_opportunities(text)`: the UAX 14 break iterator.
- `wrap_lines(glyphs, text, options)`: takes shaped glyphs (`&[sigilbuzz::Glyph]`) and
  a width and returns `LineRange`s.
- `word_breaks(text)`: the simplified UAX 29 word iterator.

## Coverage

The line-break classifier covers the UAX 14 classes that matter most in practice: BK,
CR, LF, NL, WJ, CL, CP, OP, QU, GL, NS, CM, SP, BA, BB, HY, AL, NU, PR, PO, ID, EX, ZW,
EB, and EM. Brahmic combining marks, Korean Jamo clusters, dictionary-based breaking
for Southeast Asian scripts, and the LB30a regional-indicator rule are not implemented
yet.

## License

Apache-2.0. See the workspace root `LICENSE`.

[sigilbuzz]: https://github.com/Oneiriq/sigilbuzz
[uax14]: https://www.unicode.org/reports/tr14/
[uax29]: https://www.unicode.org/reports/tr29/
