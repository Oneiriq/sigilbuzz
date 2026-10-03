# sigilbuzz-text-layout

Line breaking, word wrap, and word boundaries for [sigilbuzz].

It implements [UAX 14, the Unicode Line Breaking Algorithm][uax14] (revision 55), and
the word boundary rules of [UAX 29, Unicode Text Segmentation][uax29] (revision 47),
from tables generated out of the Unicode 17.0.0 Character Database. Both pass every
case of the Unicode conformance files `LineBreakTest.txt` and `WordBreakTest.txt`.

## Entry points

- `line_break_opportunities(text)`: the UAX 14 break iterator.
- `line_break_opportunities_with(text, word_break)`: the same, tailored by a
  `WordBreak` the way CSS `word-break` tailors line breaking.
- `wrap_lines(glyphs, text, options)`: takes shaped glyphs (`&[sigilbuzz::Glyph]`) and
  a width and returns `LineRange`s. `WrapOptions::word_break` picks the tailoring.
- `word_breaks(text)`: the UAX 29 word boundary iterator, for cursor movement and
  double-click selection.
- `line_break_class(c)`: the `Line_Break` property of a character.

## Korean and `word-break`

By default (`WordBreak::Normal`) Korean breaks between syllables, like Chinese and
Japanese. Korean is usually set with spaces between words, so most Korean text wants
`WordBreak::KeepAll`, CSS `word-break: keep-all`: no break between two letters or
numbers, so Korean breaks at spaces instead of between syllables.

`KeepAll` changes nothing else. Punctuation, symbols, and emoji keep their default
breaks, so a word (eojeol) is not always kept whole: a particle that follows a closing
bracket, a closing quotation mark, or `%` can still wrap onto the next line by itself.
`(한국어)를` may break before `를`, and so may `50%를`. CSS defines it that way, and
Blink breaks there too. Like Blink, `KeepAll` also leaves the Southeast Asian scripts
of class SA to their own breaks.

```rust
use sigilbuzz_text_layout::{line_break_opportunities_with, WordBreak};

// "한국어를 공부해요." ("I study Korean.")
let text = "\u{D55C}\u{AD6D}\u{C5B4}\u{B97C} \u{ACF5}\u{BD80}\u{D574}\u{C694}.";
let breaks: Vec<usize> = line_break_opportunities_with(text, WordBreak::KeepAll)
    .map(|(offset, _)| offset)
    .collect();
assert_eq!(breaks, [13, 26]);
```

`WordBreak::BreakAll`, CSS `word-break: break-all`, goes the other way and lets words
in any script break between letters.

## Coverage

Every UAX 14 rule is implemented, including Korean syllable blocks of conjoining jamo
(LB26, LB27), Brahmic orthographic syllables (LB28a), and regional indicator pairs
(LB30a). The Southeast Asian scripts of class SA (Thai, Lao, Khmer, Myanmar, and
others) need a dictionary to find the boundaries between words, which this crate does
not have. Line breaking treats a run of them as one word that breaks at spaces and
punctuation.

## Regenerating the tables

The tables in `src/line_break_table.rs` and `src/word_break_table.rs` come from the
snapshots in `tests/tools/ucd/`:

```text
cargo test -p sigilbuzz-text-layout --test table_gen -- --ignored
```

`tests/table_gen.rs` explains how to refresh the snapshots for a new Unicode version,
and `tests/conformance.rs` how to run the conformance files.

## License

Apache-2.0. See the workspace root `LICENSE`.

[sigilbuzz]: https://github.com/Oneiriq/sigilbuzz
[uax14]: https://www.unicode.org/reports/tr14/
[uax29]: https://www.unicode.org/reports/tr29/
