# Roadmap

sigilbuzz is heading for 1.0 in 2026. For 1.0 I want a stable shaping API and a
documented path for renderers built on top of it. What already shipped is in
[CHANGELOG.md](../CHANGELOG.md).

## Next

- Publish the workspace to crates.io.
- JPEG AC refinement scans (progressive JPEGs with `Ah > 0` in AC bands).
- SVG `<textPath>` tangent rotation, `side="right"`, and path cycling.
- sbix `jp2 ` (JPEG 2000) images.
- TIFF LZW and CCITT compression, multiple IFDs, and non-RGB color models.
- Nested SVG masks.

## Known gaps

These are smaller pieces that are not scheduled yet.

- SVG filters: `feTurbulence`, `feImage`, `feMorphology`, `feConvolveMatrix`,
  `feSpecularLighting`, `feDiffuseLighting`, and `feComponentTransfer`.
- Line breaking in `sigilbuzz-text-layout`: Brahmic combining marks, Korean Jamo
  clusters, dictionary-based breaking for Thai, Lao, and Khmer, and the LB30a
  regional-indicator rule.
- Hyphenation patterns for German, French, and Spanish. The cargo features exist but
  ship no patterns yet.
- Language tags from version 1 `name` tables are read but not exposed.
- Subsetting a CFF or CFF2 font down to fewer glyphs drops its layout and variation
  tables. TrueType fonts keep them.
- GSUB ligature and multiple substitution edit the glyph buffer in place, which is
  quadratic on very long runs (about 1.8 seconds for 40,000 Arabic characters).

## Known bugs

Found during the 0.22.0 hardening review. Each fix changes output for some valid
fonts, so they are left for a release that can call that out.

- Indic: a syllable with two or more pre-base matras moves the wrong glyphs during
  reordering. HarfBuzz uses a stable partition here.
- CFF2 FDSelect format 4 truncates font DICT indexes to 8 bits, so a font with more
  than 256 font DICTs uses the wrong local subroutines.
- Progressive JPEG images in `sbix` decode their AC coefficients through the zigzag
  table twice.
- Nested GPOS mark lookups apply to the whole run instead of the matched glyphs.
- Bidi: an RLI inside a directional override loses its direction.
- VARC: child components get empty axis coordinates where they should inherit the
  parent's, and `RESET_UNSPECIFIED_AXES` is ignored.
- SVG: `style="stop-color:x;"` drops the whole gradient stop.
- `morx`: the substitution table layout differs from the spec in one place, and a
  ligature action that pushes the same component twice removes the ligature glyph.
- CFF subsetting counts stems per subroutine, so hint masks inside subroutines get the
  wrong size.

## How work gets picked

sigilbuzz is tested by using it inside oniq. oniq has a sigilbuzz text backend next to
its HarfBuzz backend. Its integration tests run against both, and any difference in
glyph IDs or advances counts as a sigilbuzz bug. Demos move to sigilbuzz one at a time,
and none of them may look worse after the switch. The end goal is to drop rustybuzz
from oniq entirely.

When I'm deciding what to build next, the first question is whether oniq needs it.
Gaps that HarfBuzz covers and sigilbuzz doesn't come after that.

Each release has a GitHub milestone. Follow-up work is filed as issues against it.

## Ground rules

- The core crate takes no dependencies. Any new external dependency, in any crate,
  needs an entry in [deps.md](deps.md).
- Every parser ships with hand-built byte fixtures that cover the normal case, the
  boundaries, and at least one truncated or malformed input.
- Shaping output is deterministic. The same font, buffer, and features give the same
  bytes out.
- `cargo build --no-default-features` keeps working. A feature gate that breaks
  `no_std` does not merge.
