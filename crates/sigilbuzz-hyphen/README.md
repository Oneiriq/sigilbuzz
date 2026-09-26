# sigilbuzz-hyphen

Pattern-based hyphenation for [sigilbuzz], using the algorithm Frank Liang developed
for TeX ([thesis, Stanford 1983][liang]). TeX, OpenOffice, and web browsers all use
the same approach.

The algorithm finds every pattern that matches inside a word, adds up their priority
numbers at each position, and allows a break wherever the total is odd.

## Entry points

- `hyphenate(word, patterns)` returns the byte offsets in `word` where a soft hyphen
  may go.
- `Patterns::for_language(Language::EnglishUs)` loads a bundled pattern set. It is
  parsed once, on first use.
- `Patterns::parse(text)` parses your own newline-separated pattern list, for a
  language that isn't bundled.

## Quick start

```rust
use sigilbuzz_hyphen::{hyphenate, Language, Patterns};

let patterns = Patterns::for_language(Language::EnglishUs).unwrap();
let breaks = hyphenate("hyphenation", patterns);
assert_eq!(breaks, vec![2, 6]); // "hy-phen-ation"
```

## Cargo features

| Feature | Default | What it does |
|---|---|---|
| `std` | yes | Enables `std`. Every bundled pattern set needs it. |
| `patterns-en-us` | yes | Bundles the US English patterns (`hyph-en-us.tex`). |
| `patterns-de` | no | Reserved for German. No patterns yet. |
| `patterns-fr` | no | Reserved for French. No patterns yet. |
| `patterns-es` | no | Reserved for Spanish. No patterns yet. |
| `text-layout-integration` | no | Adds hyphenation points to `sigilbuzz-text-layout`'s line-break stream. |

`cargo build --no-default-features` gives you the parser alone. You then load patterns
at runtime with `Patterns::parse`.

## Pattern license

The bundled US English patterns come from Gerard D. C. Kuiken's `hyph-en-us.tex` in
the TeX hyph-utf8 package. Its notice ("copying and distribution permitted, copyright
notice preserved") is compatible with this crate's Apache-2.0 license. The full notice
is in `patterns/LICENSE-en-us`.

## License

Apache-2.0 for the crate code. The bundled pattern data keeps its upstream notice in
`patterns/LICENSE-en-us`.

[sigilbuzz]: https://github.com/Oneiriq/sigilbuzz
[liang]: https://www.tug.org/docs/liang/
