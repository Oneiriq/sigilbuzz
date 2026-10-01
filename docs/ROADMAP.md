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
- The Universal Shaping Engine finds syllables with a simpler grammar than HarfBuzz's,
  and its category table misses some characters (Tirhuta sign i, for one). So `rphf`
  is not matched one syllable at a time, and some glyph flags differ from HarfBuzz's.
  On 1,992 USE test strings, 1,519 shape as HarfBuzz 14.5.0 does.
- Sinhala runs the earlier Indic pass. HarfBuzz shapes it with the Universal Shaping
  Engine.
- Khudawadi and Takri have no shaper. HarfBuzz shapes them with the Universal Shaping
  Engine, vowel constraints included.
- A buffer of several scripts shapes one script run at a time, where HarfBuzz shapes the
  whole buffer with the shaper of its script. So contextual lookups do not reach across
  runs, and with `PRODUCE_UNSAFE_TO_CONCAT` some flags at run boundaries differ.

## Known bugs

Found during the 0.22.0 hardening review. Each fix changes output for some valid
fonts, so they are left for a release that can call that out.

- Sinhala: a syllable with two or more pre-base vowel signs moves the wrong glyphs
  during reordering. HarfBuzz uses a stable partition here. The nine scripts the
  Indic shaper port covers reorder them as HarfBuzz does.
- CFF2 FDSelect format 4 truncates font DICT indexes to 8 bits, so a font with more
  than 256 font DICTs uses the wrong local subroutines.
- Progressive JPEG images in `sbix` decode their AC coefficients through the zigzag
  table twice.
- VARC: child components get empty axis coordinates where they should inherit the
  parent's, and `RESET_UNSPECIFIED_AXES` is ignored.
- `morx`: the substitution table layout differs from the spec in one place, and a
  ligature action that pushes the same component twice removes the ligature glyph.
- CFF subsetting counts stems per subroutine, so hint masks inside subroutines get the
  wrong size.

## How work gets picked

sigilbuzz is tested inside a real text rendering application. That application has a
sigilbuzz text backend next to its HarfBuzz backend. Its integration tests run against
both, and any difference in glyph IDs or advances counts as a sigilbuzz bug. Demos move
to sigilbuzz one at a time, and none of them may look worse after the switch. The end
goal is to drop rustybuzz from that application entirely.

When I'm deciding what to build next, the first question is whether real text rendering
workloads need it. Gaps that HarfBuzz covers and sigilbuzz doesn't come after that.

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
