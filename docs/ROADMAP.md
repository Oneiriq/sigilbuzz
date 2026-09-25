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
