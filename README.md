# sigilbuzz

sigilbuzz is a text shaping engine written in pure Rust. You give it a font and a
string. It gives you back glyph IDs and positions, ready to draw. The API follows the
HarfBuzz model (blob, face, font, buffer, shape), so it will feel familiar if you have
used HarfBuzz or rustybuzz.

The core crate has no runtime dependencies, builds under `no_std`, and returns the
same output for the same input every time.

## Status

The current release is 0.22.0. sigilbuzz is still pre-1.0, so a minor release can
change the API. The names exported from the crate root are the ones I intend to keep
stable. [docs/STABILITY.md](docs/STABILITY.md) lists them.

## What it supports

- OpenType layout: every GSUB and GPOS lookup type, GDEF, feature variations, and the
  legacy `kern` table.
- AAT `morx` and `kerx`, used when a font has no GSUB or GPOS.
- Complex scripts: Arabic, Hebrew, Devanagari and the rest of the Indic family,
  Khmer, Myanmar, Thai, Lao, Tibetan, Mongolian, N'Ko, Old Hangul, plus the scripts
  handled by the Universal Shaping Engine (Balinese, Brahmi, Buginese, Cham, Khojki,
  Lepcha, Limbu, Modi, Sharada, Sundanese, Tai Tham, Tirhuta).
- Mixed-script runs, bidi (UAX 9 with paired brackets), and vertical text.
- Variable fonts: `fvar`, `avar`, `gvar`, `HVAR`, `VVAR`, `MVAR`, and VARC composite
  glyphs.
- Glyph outlines from TrueType `glyf`, CFF, and CFF2.
- Color fonts: COLRv0, COLRv1, CPAL, SVG-in-OT, CBDT/CBLC, sbix, and EBDT/EBLC.
- TrueType Collections (`.ttc`), plus the `name`, `BASE`, and `MATH` tables.

Script shaping is checked against rustybuzz on real fonts (Open Sans, Amiri, and the
Noto families under `tests/fonts/`).

## Crates

The repository is a Cargo workspace. The shaping engine is the root crate. Everything
else is optional and lives under `crates/`.

| Crate | What it does |
|---|---|
| `sigilbuzz` | Parses fonts, shapes text, and exposes outlines and font tables. |
| `sigilbuzz-render` | CPU rasterizer for outlines, color glyphs, SVG-in-OT, and embedded bitmaps. Includes a PNG encoder. |
| `sigilbuzz-paint` | Walks a COLRv1 paint tree and emits a flat list of draw commands. |
| `sigilbuzz-gpu` | Encodes outlines for GPU rendering with the Slug algorithm. |
| `sigilbuzz-subset` | Font subsetter and variable-font instancer, similar to `hb-subset`. |
| `sigilbuzz-woff` | WOFF1 and WOFF2 wrap and unwrap. |
| `sigilbuzz-svg` | Writes glyph outlines and COLRv1 glyphs as SVG. |
| `sigilbuzz-pdf` | Emits Type 3, Type 1, and embedded OpenType fonts for PDF. |
| `sigilbuzz-text-layout` | Line breaking (UAX 14), word wrap, and word boundaries. |
| `sigilbuzz-hyphen` | Liang hyphenation with bundled US English patterns. |
| `sigilbuzz-capi` | A C library that exports HarfBuzz's `hb_*` symbols, so C code can link it in place of HarfBuzz. |
| `sigilbuzz-cli` | The `sigilbuzz` command-line tool, similar to `hb-shape` and `hb-subset`. |

Each crate has its own README with an example.

## Quick start

```toml
[dependencies]
sigilbuzz = "0.22"
```

```rust
use sigilbuzz::{feature, shape, Blob, Buffer, Face, Feature, Font};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let blob = Blob::from_path("OpenSans-Regular.ttf")?;
    let face = Face::parse(&blob, 0)?;
    let font = Font::new(face, 16.0);

    let mut buffer = Buffer::new();
    buffer.push_str("Hello, world");

    let features = [Feature { tag: feature::LIGA, value: 1 }];
    let run = shape(&font, &buffer, &features)?;
    for glyph in &run.glyphs {
        println!("gid {} advance {} cluster {}", glyph.glyph_id, glyph.x_advance, glyph.cluster);
    }
    Ok(())
}
```

A few things you will likely need next:

- Right-to-left text: call `buffer.set_direction(Direction::Rtl)`. As in HarfBuzz, the
  glyphs come back in visual order (leftmost glyph first), ready to draw left to right.
- Mixed-direction text: use `buffer.set_text_bidi(text)` in place of `push_str`. It runs
  the Unicode bidi algorithm, reorders the text into visual order, and shapes it left to
  right. `buffer.bidi_map()` maps between logical and visual byte offsets afterward.
- Variable fonts: `font.with_coords(&coords)` shapes at a given set of normalized axis
  coordinates.
- Font collections: pass the member index to `Face::parse`. `fonts_in_collection` tells
  you how many members a `.ttc` file has.
- A face you can cache or share across threads: `OwnedFace` owns its bytes and has no
  lifetime parameter.

## Goals

- Pure Rust. No C, no FFI, no C toolchain. It builds anywhere stable Rust builds,
  including `wasm32-unknown-unknown`.
- No runtime dependencies in the core crate. A few companion crates take one where
  writing our own made no sense (Brotli for WOFF2, zlib for WOFF1 and PNG, clap for the
  CLI). [docs/deps.md](docs/deps.md) explains each one.
- `no_std` support. The default build uses `std`, but the shaping path runs under
  `no_std` with `alloc`.
- Deterministic output. The same font, text, features, and direction produce the same
  glyphs, byte for byte. That matters for replays, lockstep networking, and golden-file
  tests.
- Keep up with current HarfBuzz, including the newer pieces like GPU outline encoding,
  color paint, and PDF and SVG output.

## Non-goals

- The Rust API follows HarfBuzz's structure with Rust types and ownership. It does not
  mirror the C API. C callers can use `sigilbuzz-capi`, which exports the `hb_*`
  symbols.
- sigilbuzz does not aim for bug-for-bug compatibility with older HarfBuzz releases.
  Where HarfBuzz keeps a behavior for historical reasons, sigilbuzz picks the simpler
  rule.

## Why I built it

I needed a text shaper for a Rust rendering project, and the Rust options had stalled.
When I started in early 2026, rustybuzz had not published a release since November 2024.
harfbuzz-rs had barely changed since 2021 and still targeted HarfBuzz 2.x. The 2026
HarfBuzz release added a GPU rasterizer, COLR paint, and PDF and SVG output, and none of
it was reachable from Rust. So I wrote a shaper that covers it. I test sigilbuzz against
real text rendering workloads, and those workloads decide what gets built next.

## Documentation

- [CHANGELOG.md](CHANGELOG.md): what shipped in each release.
- [docs/ROADMAP.md](docs/ROADMAP.md): what comes next.
- [docs/STABILITY.md](docs/STABILITY.md): which APIs are stable before 1.0.
- [docs/PERFORMANCE.md](docs/PERFORMANCE.md): benchmark numbers against rustybuzz.
- [docs/deps.md](docs/deps.md): every external dependency and why it is there.
- [docs/RELEASING.md](docs/RELEASING.md): how a release is cut and published.
- [fuzz/README.md](fuzz/README.md): the fuzz targets and how to run them.
- [agent.md](agent.md): contribution rules.

## Development

After cloning, install the pre-push hook. It runs the same checks as CI:

```bash
scripts/install-hooks.sh
```

The hook runs `cargo fmt --all --check`, clippy with and without default features,
`cargo test --workspace --all-features`, and the `no_std` build. CI runs the same checks on pushes
and pull requests to `main` and `release/**` branches, but the hook catches problems
first. Don't bypass it with `--no-verify`.

Where things live:

- `src/`: the shaping core. `tables/` holds the font table parsers, `ot/` the OpenType
  layout engine and script shapers, `unicode/` the Unicode property data.
- `crates/`: the companion crates.
- `tests/`: integration and parity tests. Fonts live in `tests/fixtures/` and
  `tests/fonts/`.
- `benches/`: Criterion benchmarks that run sigilbuzz and rustybuzz side by side.

The minimum supported Rust version is 1.81.

## License

Apache-2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE).
