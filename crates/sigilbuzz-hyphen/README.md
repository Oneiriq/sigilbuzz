# sigilbuzz-hyphen

Liang/Knuth pattern-driven hyphenation for [sigilbuzz].

This companion crate implements [Liang's hyphenation algorithm][liang]
(PhD thesis, Stanford 1983; widely deployed in TeX, OpenOffice, web
browsers). The algorithm walks every contiguous *pattern* that matches
in a word, sums priority numbers position-by-position, and treats odd
numbers as valid break points.

## Entry points

- `hyphenate(word, patterns)` — returns the byte offsets within `word`
  where soft-hyphen breaks are valid.
- `Patterns::for_language(Language::EnglishUs)` — fetch a pre-bundled
  pattern set (parsed and cached on first use).
- `Patterns::parse(text)` — parse a custom newline-separated Liang
  pattern list (e.g. for a language not bundled here).

## Quick start

```rust
use sigilbuzz_hyphen::{hyphenate, Language, Patterns};

let patterns = Patterns::for_language(Language::EnglishUs).unwrap();
let breaks = hyphenate("hyphenation", patterns);
assert_eq!(breaks, vec![2, 6]); // "hy-phen-ation"
```

## Cargo features

| Feature                    | Default | Effect                                          |
|----------------------------|---------|-------------------------------------------------|
| `std`                      | yes     | Enables `std`. Required by every pattern bundle. |
| `patterns-en-us`           | yes     | Bundles the en-us pattern set (`hyph-en-us.tex`). |
| `patterns-de`              | no      | Reserved — German bundle (no patterns yet).      |
| `patterns-fr`              | no      | Reserved — French bundle.                        |
| `patterns-es`              | no      | Reserved — Spanish bundle.                       |
| `text-layout-integration`  | no      | Bridge to `sigilbuzz-text-layout` UAX 14 stream. |

`cargo build --no-default-features` produces a parser-only crate that
takes patterns at runtime via `Patterns::parse`.

## Bundled pattern licensing

The bundled `en-us` pattern set is derived from Gerard D.C. Kuiken's
`hyph-en-us.tex` (TeX hyph-utf8 package). Its upstream notice
("copying and distribution permitted, copyright notice preserved") is
compatible with this crate's Apache-2.0 grant. See
`patterns/LICENSE-en-us` for the verbatim notice.

## License

Apache-2.0 (crate code). Bundled pattern data carries the upstream
notice in `patterns/LICENSE-en-us`.

[sigilbuzz]: https://github.com/Oneiriq/sigilbuzz
[liang]: https://www.tug.org/docs/liang/
