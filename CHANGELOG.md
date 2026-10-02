# Changelog

Every release of the sigilbuzz workspace, newest first. Numbers in parentheses are
GitHub issues and pull requests. sigilbuzz is pre-1.0, so a minor release may change
the API. See [docs/STABILITY.md](docs/STABILITY.md) for what is covered by the
stability commitment.

## 0.22.0 (2026-10-02)

Added:

- `Buffer::guess_segment_properties`, HarfBuzz's `hb_buffer_guess_segment_properties`: an
  unset script becomes that of the first character that is not Common or Inherited, and
  an unset direction becomes that script's horizontal direction. The language stays
  unset, so the output does not depend on the process locale. With the script set,
  `shape` uses one shaper for the whole buffer, as HarfBuzz does, so code that calls
  `hb_buffer_guess_segment_properties` before `hb_shape` gets HarfBuzz's output for text
  that mixes scripts too. Without the call, `shape` still shapes each script run with
  its own shaper. On 1,793 strings that put the vowel constraint sequences after Latin or
  another script, the output with the call matches HarfBuzz 14.5.0 in glyphs, clusters,
  glyph flags and positions at every cluster level (without it: 1,746, and 1,716 at
  `MonotoneCharacters`).
- `OwnedFace`, a face that owns its font bytes behind an `Arc<[u8]>` and has no
  lifetime parameter. It parses the table directory once and hands out `Face` views
  through `as_face()`. It is `Send + Sync` and cheap to clone, so you can keep parsed
  faces in a cache or share them across worker threads.
- TrueType Collection (`.ttc`) support. `Face::parse` and `OwnedFace::parse` now take a
  member index into a collection. They used to reject collections as unsupported. The
  new `fonts_in_collection` returns the member count, or `None` for a plain TTF or OTF.
- `BidiParagraph`, which runs the Unicode bidi algorithm over a paragraph and shapes it
  the way HarfBuzz callers do: each run of one embedding level in logical order and in
  its own direction, with the text around it as context, and the runs in visual order.
  `line_runs`, `visual_runs`, `shape_line` and `reorder_visual` work one line at a time,
  and glyph clusters stay byte offsets into the paragraph text. `BidiRun` and
  `ShapedBidiRun` describe the runs. The CLI's `shape --bidi` shapes through it.
- `BufferFlags` with `Buffer::{set_flags, flags}`, HarfBuzz's buffer flags (`BOT`,
  `EOT`, `PRESERVE_DEFAULT_IGNORABLES`, `REMOVE_DEFAULT_IGNORABLES`,
  `DO_NOT_INSERT_DOTTED_CIRCLE`) with HarfBuzz's values and behavior.
- `ClusterLevel` with `Buffer::{set_cluster_level, cluster_level}`, HarfBuzz's four
  cluster levels. Clusters form and merge where HarfBuzz forms and merges them
  (graphemes, ligatures, reordered syllables, deleted default ignorables), as the level
  allows. A Rust `Buffer` defaults to `MonotoneCharacters`, the level closest to the old
  output.
- `unicode::normalize::{decompose, compose, combining_class, modified_combining_class}`,
  generated from the Unicode 17.0 data, with Hangul handled algorithmically.
- Buffer script, language and context. `Buffer::set_script`, `set_language`,
  `set_pre_context` and `set_post_context` (with getters and `Buffer::CONTEXT_LENGTH`)
  now reach shaping. A set script shapes the whole buffer as that script. The language
  picks the OpenType language system in GSUB and GPOS, and the context text lets
  Arabic, N'Ko and Mongolian letters at the buffer edges join across it.
- `Language`, a BCP 47 tag that maps to OpenType language system tags (at most three,
  as in HarfBuzz). The table is generated from the OpenType language tag registry and
  SIL's ISO 639 data. `cargo test --test language_table_gen -- --ignored` regenerates
  it.
- `Buffer::unset_direction` and `Buffer::has_explicit_direction`. Broken Indic, Khmer,
  Myanmar and USE syllables now get a U+25CC dotted circle, as in HarfBuzz (turn it off
  with `BufferFlags::DO_NOT_INSERT_DOTTED_CIRCLE`).
- `UnicodeScript::{iso15924_tag, from_iso15924_tag, horizontal_direction}` and
  `Direction::horizontal_for_script`.
- `ShapedRun` is re-exported from the crate root.
- `UnicodeScript` buckets for Syriac and the 75 other scripts HarfBuzz 14.5.0 gives the
  Universal Shaping Engine that sigilbuzz had none for (see Changed), and the hidden
  `unicode::script_code`, the Unicode Script property of a character as an ISO 15924
  code (`Zyyy` for Common, `Zinh` for Inherited, `Zzzz` for unassigned code points).
- GPOS cursive attachment (lookup type 3). `curs` runs by default on horizontal runs.
  It never ran before.
- `PairPos::lookup_with_device_base`, which also returns the bytes the records' Device
  and VariationIndex offsets are measured from (the PairSet in format 1, the subtable in
  format 2), and `PairPos::value_format2`.
- `sigilbuzz-capi`: `hb_buffer_set_flags`, `hb_buffer_get_flags`,
  `hb_buffer_set_cluster_level` and `hb_buffer_get_cluster_level`, with HarfBuzz's
  constants and defaults (`HB_BUFFER_CLUSTER_LEVEL_MONOTONE_GRAPHEMES` for a new
  buffer).
- `sigilbuzz-capi`: `hb_buffer_add_utf32`, `hb_buffer_add_codepoints`,
  `hb_buffer_add_latin1`, `hb_subset_input_reference`, `hb_subset_input_create_or_fail`,
  `hb_paint_funcs_reference`, `hb_paint_funcs_make_immutable` and `_is_immutable`, the
  `push_clip_rectangle`, `image` and `custom_palette_color` paint callbacks,
  `hb_color_line_get_color_stops`, `hb_color_line_get_extend`, and `hb_color_get_*`.
- `sigilbuzz-paint`: `evaluate_with` and `EvalOptions` choose the variation coordinates,
  the CPAL palette and the foreground color in one call.
- `sigilbuzz-render`: `Rasterizer::with_foreground` picks the COLR foreground color.
- COLR `ClipList` parsing: `tables::colr::{ClipList, ClipBox}` and
  `Colr::{clip_list, clip_box, clip_list_offset, var_index_map_offset}`.
- `sigilbuzz-capi`: `hb_paint_funcs_set_color_glyph_func`. `hb_version` reports 8.2.0,
  the HarfBuzz release that added that callback.
- `sigilbuzz-paint`: `Transform2D::inverse`.
- `sigilbuzz-subset` 0.12.0: `SubsetWarning`, returned in `SubsetOutput::warnings` and
  `InstancedOutput::warnings`. Every malformed layout or variation structure the
  subsetter leaves out, instead of failing the run, is reported with its table, byte
  offset and reason, up to 65,536 warnings. The new fields break struct literals.
- `ClassDef::empty` and `ClassDef::parse_at`.
- `fuzz/`: cargo-fuzz targets for every part of the workspace that reads untrusted
  input. See [fuzz/README.md](fuzz/README.md).
- Per-syllable matching, HarfBuzz's `F_PER_SYLLABLE`: the Indic, Khmer, Myanmar and USE
  shapers number their syllables in the new `Glyph::syllable` field (HarfBuzz's
  `syllable()` byte), and the features HarfBuzz registers per syllable (`locl`, `ccmp`
  and the syllable-forming features of each shaper) only match glyphs of the cursor's
  syllable, so a conjunct or ligature no longer forms across a syllable boundary.
  `MatchContext::with_per_syllable` and `MatchContext::per_syllable` expose the setting
  to the lookup matchers. `Glyph` gains a public field, so code that builds a `Glyph`
  with a struct literal must add `syllable: 0`.
- Glyph flags, HarfBuzz's `hb_glyph_flags_t`: `GlyphFlags` (`UNSAFE_TO_BREAK`,
  `UNSAFE_TO_CONCAT`, `SAFE_TO_INSERT_TATWEEL`) in the new `Glyph::flags` field, set where
  HarfBuzz 14.5.0 sets them: the skipping iterator's matches and failed matches in
  every GSUB and GPOS lookup type, cluster merges a cluster level skips, ligatures and
  deleted glyphs, kerning (GPOS, `kern`, `kerx` pairs), cursive and mark attachment,
  fallback mark positioning, Arabic, Mongolian and N'Ko joining, the syllables of the
  Indic, Khmer, Myanmar and USE shapers, a left matra the Indic shaper gives no `init`,
  and the Hangul shaper's jamo and tone marks. Every glyph of a cluster carries the same
  flags. On the Khmer, Indic and Hangul test strings the flags match HarfBuzz 14.5.0
  wherever the glyphs do, at every cluster level. `BufferFlags::PRODUCE_UNSAFE_TO_CONCAT` and
  `BufferFlags::PRODUCE_SAFE_TO_INSERT_TATWEEL` (HarfBuzz's values) turn on the two
  optional kinds. Code that builds a `Glyph` with a struct literal must add
  `flags: GlyphFlags::empty()`.
- `sigilbuzz-capi`: `hb_glyph_info_get_glyph_flags`, `hb_glyph_flags_t` with the
  `HB_GLYPH_FLAG_*` constants, and `HB_BUFFER_FLAG_PRODUCE_UNSAFE_TO_CONCAT` and
  `HB_BUFFER_FLAG_PRODUCE_SAFE_TO_INSERT_TATWEEL`. `hb_glyph_info_t::mask` carries the
  glyph flags, as in HarfBuzz. It used to be zero.
- cmap format 14, Unicode Variation Sequences: `Cmap::variation_glyph`,
  `Cmap::variation_selectors`, `Cmap::variation_unicodes`, and `Face::variation_glyph`,
  read from the subtable under `(0, 5)` as HarfBuzz reads it. Shaping uses it as
  HarfBuzz's normalizer does: a character followed by a variation selector (U+FE00 to
  U+FE0F, U+E0100 to U+E01EF) takes the glyph the font gives the pair, or the
  character's usual glyph for a default sequence, and the selector is dropped with its
  cluster merged into the character's. Pairs the font does not list shape as before.
  Output changes only for fonts with a format 14 subtable. A format 14 subtable that
  does not fit is ignored.
- `sigilbuzz-capi`: `hb_font_get_nominal_glyph`, `hb_font_get_variation_glyph`,
  `hb_font_get_glyph`, `hb_face_collect_variation_selectors`, and
  `hb_face_collect_variation_unicodes`.
- `Buffer::{set_not_found_variation_selector_glyph, not_found_variation_selector_glyph}`,
  HarfBuzz's not-found variation selector glyph: a variation selector the font has no
  glyph for after its base character becomes that glyph, with no advance or offset,
  instead of being hidden or removed, so a caller can tell the font lacks the variation.
  `sigilbuzz-capi`: `hb_buffer_set_not_found_variation_selector_glyph`,
  `hb_buffer_get_not_found_variation_selector_glyph`, and `HB_CODEPOINT_INVALID`.
- `BidiParagraph` applies UAX #9 rule P1: it splits the text after each paragraph
  separator (LF, CR, NEL, U+001C to U+001E, U+2029, with CR LF as one separator, as ICU
  treats it), and each paragraph gets its own base level (or the forced direction).
  Runs, lines and visual order stop at paragraph boundaries, and shaping context and
  the `BOT` and `EOT` flags stop at paragraph edges, so each paragraph shapes as it
  would alone. `BidiParagraph::{paragraphs, paragraph_at}` and `BidiParagraphSpan`
  describe the paragraphs. `direction` and `base_level` give the first paragraph's.
  Text without a paragraph separator resolves and shapes as before. The CLI's
  `shape --bidi` gets the split too.

Changed:

- `shape()` returns right-to-left and bottom-to-top runs in visual order, reversed after
  positioning as HarfBuzz does. It used to return them in logical order, although
  `ShapedRun` promised visual order. Mark offsets use HarfBuzz's direction-aware rules.
  Code that reversed RTL output itself must stop doing so.
- Text in a direction that is not its script's own (Hebrew marked LTR, Latin marked RTL,
  bottom-to-top) is shaped the way HarfBuzz does it: grapheme clusters are reversed, the
  run is shaped in its native direction, and the result is reversed back.
- Text is normalized against the font like HarfBuzz, always: a character the font
  cannot draw whole decomposes, marks sort by HarfBuzz's modified combining classes,
  and a base and mark recompose only into a composite the font maps. Each script uses
  the normalization mode and hooks of its HarfBuzz shaper, so Indic, Khmer and USE split
  vowels stay split. Missing fallback spaces (EN SPACE, IDEOGRAPHIC SPACE, ...) are drawn
  with the space glyph at HarfBuzz's widths.
- GSUB and GPOS match rules with HarfBuzz's skipping iterator: a default ignorable a
  rule does not name is passed over, ZWJ and ZWNJ follow each feature's joiner rules,
  marks on different components of a ligature do not match together, and nested lookups
  run at the recorded match positions. Fonts without a GDEF `GlyphClassDef` get glyph
  classes synthesized from Unicode, as in HarfBuzz, so `IgnoreMarks`, mark zeroing and
  mark attachment work for them. The `tables::layout` matchers take `MatchGlyph` runs
  and a `MatchContext` (see Removed).
- A language system's required GPOS feature always joins the positioning stage.
- Fonts that GPOS does not position get HarfBuzz's fallback mark positions, placed
  from each mark's combining class and its base's extents.
- Feature order follows HarfBuzz's shapers: `ltra`, `ltrm` and `rtla` apply by
  direction, Myanmar runs `locl` and `ccmp` before its reorder, the Universal Shaping
  Engine reorders pre-base vowels after its basic features, and Thai, Lao and Hangul
  no longer run default features twice. Non-joining characters get no joining feature.
- Default ignorables stay in the script run of their neighbors, so a ZWSP or a
  variation selector no longer splits a run.
- Mongolian switches to vertical layout only when no direction was set.
  `set_direction(Direction::Ltr)` now gives horizontal Mongolian (it used to need RTL).
- Bottom-to-top runs report a negative `y_advance`, like top-to-bottom ones.
- GSUB and GPOS take every feature from one script and one language system, as
  HarfBuzz does, and a language system's required feature always applies. Latin,
  Greek, Cyrillic, Han and kana runs try their own script tag (`latn`, `grek`, `cyrl`,
  `hani`, `kana`) before `DFLT`, so fonts that keep their kerning or ligatures under the
  script tag get them now. Rubik VF, for example, had no Latin kerning before.
- GPOS runs as one stage in lookup order: `abvm` and `blwm` run by default, a lookup
  shared by several features runs once, the first matching subtable wins, and `kern`,
  `dist` and `curs` run by default only on horizontal runs.
- `locl` and `ccmp` run first, and `locl` is applied at all (it never was, so a
  language system choice changed nothing).
- Mark attachment follows HarfBuzz: GSUB records ligature components, mark-to-mark
  checks components, base searches skip default ignorables, and the lookup's mark
  coverage (not the GDEF class) decides what attaches. Attachment offsets are resolved
  once, after all positioning, so marks follow kerning on their base.
- Mark widths are zeroed in both axes, per HarfBuzz's shaper for each script.
- Legacy `kern` and `kerx` pair values are split across both glyphs as in hb-kern.hh,
  and `kerx` is preferred over `kern`.
- Every Unicode default-ignorable character is hidden, after positioning rather than
  before GSUB: its real glyph goes through GSUB and becomes the space glyph at the end.
- Right-to-left runs mirror characters that have a mirror glyph and apply `rtlm`.
- Joining types are generated from Unicode 17.0 `ArabicShaping.txt`, so marks and format
  characters are transparent.
- Leading digits and punctuation join the script run that follows them, and text with
  no script shapes under `DFLT` (this changes `Buffer::script_runs`).
- `Buffer::clear` also resets script, language, context and the explicit direction.
- `tables::Anchor` has new public fields for its device offsets. Building one with a
  struct literal needs `..Anchor::default()`.
- `sigilbuzz-capi` 0.3.0 follows HarfBuzz's object rules, which breaks C code written
  against the old ones:
  - `hb_*_reference(p)` returns `p` and adds a reference, for every object type. A
    reference followed by two destroys used to double-free.
  - `hb_subset_input_unicode_set` and `hb_subset_input_glyph_set` return a set owned by
    the input. Do not destroy it.
  - A face keeps its blob alive and a font keeps its face alive. The blob's destroy
    callback runs when HarfBuzz runs it.
  - The paint API matches HarfBuzz 8.0's `hb-paint.h`: setters take `user_data` and
    `destroy`, callbacks receive their `user_data`, `push_clip_glyph` receives the font,
    and groups are `push_group` / `pop_group`. `HB_COLOR` packs blue high and alpha low.
  - `hb_buffer_clear_contents` resets direction, script, language and context, and
    `hb_buffer_set_direction(HB_DIRECTION_INVALID)` unsets the direction.
  - `hb_buffer_guess_segment_properties` guesses the script from the Unicode Script
    property and takes the direction from it.
- `hb_font_set_ppem` is documented as a no-op. sigilbuzz does not hint, so nothing read
  the value.
- `sigilbuzz-paint` 0.2.0 keeps the COLR foreground color apart instead of painting it
  white: `PaintSource::Solid` is `Solid { color, is_foreground }` and `ColorStop` has an
  `is_foreground` field. The default foreground is opaque black, and a palette or entry
  the font lacks resolves to the foreground, as in HarfBuzz.
- `sigilbuzz-render` 0.9.0 paints COLRv1 foreground layers black by default (they were
  white), the same as COLRv0.
- `sigilbuzz-svg` 0.2.0 writes foreground paints as `currentColor`.
- COLRv1 glyphs are clipped to their ClipList box, or to bounds computed from the paint
  tree, as in HarfBuzz. A glyph with unbounded paint renders empty. This holds in
  `sigilbuzz-render` (pixmaps are sized to the clip box), `sigilbuzz-svg` and
  `hb_font_paint_glyph`, which also emits HarfBuzz's root clip rectangle and offers
  referenced glyphs to `color_glyph`.
- `sigilbuzz-paint` reads COLR variation deltas only from the COLR table's own
  ItemVariationStore and DeltaSetIndexMap. It used to fall back to GDEF's store and
  read an index map from a made-up GDEF field. The renderers and the SVG writer now
  draw from the paint walk, and `evaluate_with` emits isolated groups for composites.
- `rasterize_colrv0_glyph` paints palette entries the font cannot supply in the
  foreground color instead of returning `NoCpal` or `BadPaletteIndex`, and SVG-in-OT
  `currentColor` follows `Rasterizer::with_foreground`.
- The CLI's `--script` and `--language` flags take effect.
- The companion crate READMEs no longer claim `no_std`. Every companion crate enables the
  core crate's `std` feature.
- The minimum supported Rust version is now 1.81. The core crate already needed 1.81
  for `core::error::Error`, so the old `rust-version = "1.75"` was wrong.
- `sigilbuzz-woff` 0.3.1 and `sigilbuzz-render` 0.9.0 move to `miniz_oxide` 0.9.
  `sigilbuzz-woff` 0.3.1 also moves to `brotli` 9.
- `sigilbuzz-capi` 0.3.0 installs with `cargo cinstall` from cargo-c. That puts
  `libsigilbuzz`, the header (`include/sigilbuzz/hb.h`), a generated `sigilbuzz.pc`, and
  a CMake package in place in one step. The old pkg-config and CMake templates had to be
  filled in by hand and looked for a `libsigilbuzz` that `cargo build` never produced
  (it builds `libsigilbuzz_capi`). They are gone.
- All benchmarks moved to Criterion 0.8.
- A full `LICENSE` file now sits at the repo root, and `NOTICE` spells out the
  attribution terms. The license is still Apache-2.0.
- The documentation was rewritten, and the release history moved out of
  `docs/ROADMAP.md` into this file.
- CI and the pre-push hook lint and test the whole workspace. They used to cover only
  the root crate. CI also checks the minimum Rust version, including every `no_std`
  build.
- GSUB applies each lookup in one pass through HarfBuzz's output-buffer model
  (`out_info`, `next_glyph`, `replace_glyphs`, `output_glyph`, `move_to`, `sync`), so
  ligature and multiple substitutions take time linear in the run. They used to edit the
  glyph vector in place and resync, which was quadratic: 20,000 "fi" ligatures in Open
  Sans took 8 seconds in a debug build and now take 0.16 seconds. Ligation also follows
  HarfBuzz 14.5.0 in two details: the later pieces of a multiple substitution add no
  component to a ligature they join (`_hb_glyph_info_get_lig_num_comps_in_ligation`),
  and a ligature whose first component is a nonspacing mark stops being a mark. Once
  all seven ligature ids are live, new ligatures take them in turn.
- Khmer has its own shaper, following HarfBuzz's (`hb-ot-shaper-khmer.cc`), in place of
  the Universal Shaping Engine. HarfBuzz's Khmer syllable grammar decides the syllables,
  so ZWJ and ZWNJ stay in a syllable only before a robat, an above-base vowel sign or an
  X-group sign, and broken clusters get a dotted circle. Coeng + ro and a pre-base vowel
  sign move to the start of their syllable before any lookup runs, with HarfBuzz's
  `pref`, `blwf`, `abvf`, `pstf` and `cfar` masks. `locl`, `ccmp` and those five
  features run as one stage, each lookup only where its mask allows and one syllable at
  a time (`F_PER_SYLLABLE`), and `pres`, `abvs`, `blws` and `psts` run as one stage with
  `rlig`, `calt`, `clig`, `rclt` and the caller's features. `liga` is off for Khmer, as
  in HarfBuzz. On 2,010 Khmer test strings with Noto Sans Khmer, the output now matches
  HarfBuzz 14.5.0 at the `MonotoneGraphemes`, `MonotoneCharacters` and `Characters`
  cluster levels (before: 281 of the first 510). `ot::use_shaper::shape_khmer` runs the
  new shaper, default features included.
- Devanagari, Bengali, Gurmukhi, Gujarati, Oriya, Tamil, Telugu, Kannada and Malayalam
  run through a port of HarfBuzz's Indic shaper (`hb-ot-shaper-indic.cc`): its syllable
  grammar and character table, consonant positions read from the font's `blwf`,
  `vatu`, `pstf` and `pref`, its initial and final reordering, and its feature stages
  and masks. ZWJ and ZWNJ now act as in HarfBuzz: a joiner after Ra,H blocks an implicit
  reph, a ZWJ after a halant stops the base search and keeps a pre-base matra from
  moving past that halant, a ZWNJ turns `half` off and ends the syllable after a
  halant, and a reph or pre-base consonant moves past a joiner that follows a halant.
  Kannada Ra,H,ZWJ at the start of a syllable is shaped as Ra,ZWJ,H, so it forms no
  reph, with the halant and ZWJ clusters merged. `liga` is off for these scripts, and
  `init`, `pres`, `abvs`, `blws`, `psts` and `haln` run in one stage with `rlig`,
  `calt`, `clig`, `rclt` and the caller's features. On 7,656 test strings with the Noto
  Sans fonts of the nine scripts, the output matches HarfBuzz 14.5.0 on all of them
  (before: 4,826), at every cluster level, with the vowel constraints and the Devanagari
  stress signs below.
  `ot::indic::shape_indic` and `shape_devanagari` run the port, default features
  included.
- The Universal Shaping Engine moves a repha as HarfBuzz does (`reorder_syllable_use`
  in `hb-ot-shaper-use.cc`). `rphf` only applies to the first three glyphs of a
  syllable (the first one when it is a repha character), the glyph it substitutes
  becomes a repha, and after the basic features the repha moves to just before the
  first vowel sign, medial, final or halant that did not ligate, or to the end of the
  syllable, merging the clusters it passes. Tirhuta and Modi reph forms used to stay in
  front of the base.
- The Universal Shaping Engine follows HarfBuzz's (`hb-ot-shaper-use.cc`). Its character
  categories come from a table generated the way `gen-use-table.py` builds
  `hb-ot-shaper-use-table.hh`, from the Unicode 18.0 data and HarfBuzz's `ms-use`
  overrides (`cargo test --test use_table_gen -- --ignored` regenerates it), so
  characters such as Tirhuta sign i now join their clusters. The syllable grammar of
  `hb-ot-shaper-use-machine.rl` finds the clusters, with the default-ignorable marks and
  a ZWNJ before a mark left out of the match, and each cluster is unsafe to break.
  `locl` to `akhn`, then `rphf`, then `pref`, then `rkrf` to `cjct` run as stages one
  cluster at a time (`F_PER_SYLLABLE`), with HarfBuzz's joiner handling, its `rphf` mask
  and its repha and pre-base records. Broken clusters get a dotted circle after any
  repha, and then the repha and the pre-base vowel signs move. `isol`, `init`, `medi`
  and `fina` follow the joining of the clusters (of the letters, for N'Ko and
  Mongolian), and `abvs`, `blws`, `haln`, `pres` and `psts` run as one stage with
  `rlig`, `calt`, `clig`, `liga`, `rclt` and the caller's features. Sinhala, Tibetan,
  N'Ko and Mongolian now shape with it, as in HarfBuzz (`hb_ot_shaper_categorize`), in
  place of the earlier Indic pass and their own passes, and a USE script in a font whose
  GSUB only has `DFLT` or `latn` lookups gets the default shaper. With the vowel
  constraints below, all 1,992 USE test strings with the Noto fonts for Balinese, Cham,
  Khojki, Modi, Sharada and Tirhuta now match HarfBuzz 14.5.0 in glyphs, clusters and
  glyph flags at the `MonotoneGraphemes`, `MonotoneCharacters` and `Characters` cluster
  levels (before: 1,519 in glyphs, 1,275 with flags), and so do all 1,448 Sinhala
  strings (before: 1,036 and 983). 2,100 strings in six more USE scripts and 900 in
  Tibetan, N'Ko and Mongolian all match too (before: 1,532 and 733). This fixes Sinhala syllables with two or more pre-base vowel signs. The entry points `ot::use_shaper::shape_balinese` to `shape_modi`,
  `shape_nko`, `ot::tibetan::shape_tibetan`, `ot::mongolian::shape_mongolian`, and
  `ot::indic::shape_indic` for Sinhala run the new shaper, default features included.
  `ot::use_shaper::USE_TOPOGRAPHICAL_FEATURES` now lists `isol`, `init`, `medi` and
  `fina` too. `UnicodeScript::is_use` now holds for the scripts HarfBuzz gives the
  Universal Shaping Engine: also Sinhala, Tibetan and Mongolian, and no longer Khmer,
  Myanmar, Thai, Lao and Hangul, which have shapers of their own.
  `UnicodeScript::is_indic` no longer holds for Sinhala.
- Myanmar follows HarfBuzz's Myanmar shaper (`hb-ot-shaper-myanmar.cc`). The Myanmar
  categories of HarfBuzz's Indic table (`gen-indic-table.py`, now generated with the
  Myanmar blocks and the variation selectors) feed the syllable machine of
  `hb-ot-shaper-myanmar-machine.rl`, and each syllable is unsafe to break. `locl` and
  `ccmp` run per syllable before the reorder. Broken clusters then get a dotted
  circle, and each syllable is sorted by HarfBuzz's positions: a kinzi after the base,
  a medial ra and pre-base vowel signs before it, the marks after a below-base vowel
  before that vowel, a run of pre-base vowel signs flipped, and each move merging the
  clusters it passes. `rphf`, `pref`, `blwf` and `pstf` follow one stage each, per
  syllable and with manual ZWJ, then `pres`, `abvs`, `blws` and `psts` in one stage
  with `rlig`, `calt`, `clig`, `liga`, `rclt` (or `vert`) and the caller's features.
  All 1,540 Myanmar test strings and 3,775 more (kinzi, medials, stacks, signs, tones,
  joiners, broken clusters, digits, variation selectors, Myanmar Extended-A and -B)
  now match HarfBuzz 14.5.0 in glyphs, clusters, glyph flags and positions at the
  `MonotoneGraphemes`, `MonotoneCharacters` and `Characters` cluster levels (before:
  1,033, 1,031 and 1,032 of the 1,540 in glyphs). Myanmar text in a font whose GSUB
  picks `DFLT`, `latn` or `mymr` gets the default shaper's `ccmp`, `locl` and joiner
  handling, which it skipped before. `ot::use_shaper::shape_myanmar` runs the new
  shaper, default features included.
- Hangul follows HarfBuzz's Hangul shaper (`hb-ot-shaper-hangul.cc`) in a buffer whose
  script is Hangul. Its preprocessing runs after grapheme clusters form, as in
  HarfBuzz: jamo compose into a precomposed syllable the font has, a syllable the font
  lacks decomposes into jamo, and a tone mark (U+302E, U+302F) after a syllable moves
  in front of it, sharing its cluster, unless the font draws it with no advance. A tone
  mark with no syllable before it gets a dotted circle, which sorts with the marks as
  the tone mark does. `ljmo`, `vjmo` and `tjmo` apply only to the jamo of a syllable
  that did not compose, and they run in one stage with the default features, where
  `calt` applies to every glyph but jamo (HarfBuzz 14.5.0 turns `calt` off on jamo
  only). The tone marks are now Hangul script, as in `Scripts.txt`. On 1,200 random
  Hangul strings with Noto Sans KR and the Old Hangul fixture, the output now matches
  HarfBuzz at every cluster level (before: 558). `ot::use_shaper::shape_hangul` runs
  the new stage, default features included.
- The Indic and Universal Shaping Engine shapers insert HarfBuzz's vowel constraint
  dotted circles (`_hb_preprocess_text_vowel_constraints` in
  `hb-ot-shaper-vowel-constraints.cc`). Before normalization, a U+25CC goes before the
  last character of each sequence of the buffer's script that
  `IndicShapingInvalidCluster.txt` lists, such as Devanagari A followed by the vowel
  sign AA, which would read as the letter AA. `DO_NOT_INSERT_DOTTED_CIRCLE` turns it
  off. The circle takes the cluster, glyph flags and Unicode properties of the
  character after it, so in a font without GDEF glyph classes a circle before a
  nonspacing mark is a mark. `tests/vowel_constraints_gen.rs` generates the table from
  that file. HarfBuzz's Khmer and Myanmar shapers do not run it. Khudawadi and Takri,
  the two other scripts it lists, get it with their new buckets (below). On 1,793
  strings that put every listed sequence of the 14 other scripts in several contexts,
  the output
  matches HarfBuzz 14.5.0 on 1,746 at `MonotoneGraphemes` and `Characters` and 1,716 at
  `MonotoneCharacters` (before: 628 and 574). The rest put two scripts in one buffer,
  which sigilbuzz shapes one script run at a time unless the caller calls
  `Buffer::guess_segment_properties`. With that call, all 1,793 match.
- Hangul in a buffer of another script, and the text of other scripts in a Hangul
  buffer, shape as HarfBuzz shapes them. Such text normalizes with the shaper of the
  buffer, as HarfBuzz normalizes the whole buffer with it: a syllable followed by a mark
  in a Latin or Han buffer decomposes into jamo, and Latin text in a Hangul buffer
  composes nothing. `calt` now applies to other scripts in a horizontal Hangul buffer,
  since HarfBuzz 14.5.0 keeps it off jamo only. A Hangul tone mark after a letter of
  another script stays in that letter's run, so it sorts with the letter's marks and
  joins its cluster, and `Buffer::script_runs` keeps it in that run too. On 5,568
  strings that mix syllables, modern and old jamo, and tone marks with Latin, Han,
  kana, Greek and Cyrillic in both orders, with Noto Sans KR, its subsets and the Old
  Hangul fixture, the output matches HarfBuzz 14.5.0 at every cluster level (before:
  4,969).
- The Devanagari stress signs and accents (U+0951..U+0954) are Inherited, as in
  `Scripts.txt`: they stay in the run of the letter before them and do not give a
  buffer its script. Alone they now shape with the default shaper, as in HarfBuzz,
  where they used to get a dotted circle.
- Breaking: `UnicodeScript` has a bucket for every script HarfBuzz 14.5.0 gives a shaper
  of its own (`hb_ot_shaper_categorize` in `hb-ot-shaper.hh`): Syriac, which takes the
  Arabic shaper, and 75 scripts of the Universal Shaping Engine, from Javanese, Chakma,
  Kaithi, Khudawadi, Takri and Grantha to Adlam, Mandaic, Sogdian, and Jurchen,
  Proto-Cuneiform and Seal from Unicode 18.0. They were `Other` and shaped with the
  default shaper, so they got no syllables, no reordering, no joining forms and no
  vowel constraints. Their code points come from the Unicode Script property
  (`Scripts.txt` of Unicode 18.0.0, the version HarfBuzz 14.5.0 reads), and each tries
  its ISO 15924 code in lowercase, then `DFLT`, as its script tags
  (`hb_ot_old_tag_from_script` in `hb-ot-tag.cc`). Adlam, Chorasmian, Hanifi Rohingya,
  Mandaic, Manichaean, Old Uyghur, Phags-pa, Psalter Pahlavi and Sogdian get
  Arabic-style joining forms in the Universal Shaping Engine (`has_arabic_joining`), and
  Khudawadi and Takri get their vowel constraints. The joining glyph flags now come only
  from the shaper HarfBuzz picks for the buffer, so Mongolian and N'Ko in a font with
  only `DFLT` or `latn` lookups no longer get them. Sidetic is right to left
  (`Direction::horizontal_for_script`), and Tifinagh keeps no native direction. On
  97,234 strings in 56 of these scripts with their Noto fonts, the output matches
  HarfBuzz 14.5.0 in glyphs, clusters and glyph flags at the `MonotoneGraphemes`,
  `MonotoneCharacters` and `Characters` cluster levels (before: 47,078, 46,984 and
  46,984), and in positions too except for 50 Marchen strings (see docs/ROADMAP.md).
  The new variants break an exhaustive `match` on `UnicodeScript`, which is now
  `#[non_exhaustive]`, so later buckets will not.
- Syriac shapes with HarfBuzz's Arabic shaper (`hb-ot-shaper-arabic.cc`). The joining
  state machine of `arabic_joining`, with its ALAPH and DALATH RISH columns, gives alaph
  `fin2` after a letter that does not join it and `fin3` after dalath or rish, and the
  letter before a final alaph `med2`. The font's `stch` feature splits U+070F SYRIAC
  ABBREVIATION MARK into tiles that stretch over the rest of the word after positioning
  (`record_stch` and `apply_stch`). All 581 Syriac test strings with Noto Sans Syriac
  match HarfBuzz 14.5.0 at every cluster level (before: 268 with flags).
- The Arabic shaper keeps each glyph's joining form with the glyph through `ccmp` and
  `locl`, as HarfBuzz keeps it in the glyph info. A font whose `ccmp` splits a letter
  (Noto Sans Arabic splits dotted letters into a base and its dots) used to give the
  joining features to the wrong glyphs. The joining features run in HarfBuzz's order
  (`isol`, `fina`, `fin2`, `fin3`, `medi`, `med2`, `init`) under the segment's script
  tags, and vertical Arabic takes the default shaper, as in HarfBuzz. On 1,000 Arabic
  strings with Noto Sans Arabic and Amiri, 999 now match HarfBuzz 14.5.0 right to left
  (before: 686) and all 1,000 top to bottom (before: 334).
- Characters whose Unicode Script is Common or Inherited (the tatweel, the dandas, the
  Arabic harakat, CJK punctuation) stay in the script run around them, in `shape` and
  in `Buffer::script_runs`, as HarfBuzz gives a buffer the script of its first other
  character. They used to start a run of their own when `script_of` gave them a bucket
  or `Other`, which split a Syriac word at its tatweel.
- The Universal Shaping Engine and the Indic and Khmer shapers count a substitution that
  keeps the glyph id as a substitution (`_hb_glyph_info_substituted`). Noto Sans
  Javanese's `pref` maps cakra to itself, and HarfBuzz then moves the cakra in front of
  its base like a pre-base vowel sign (`record_pref_use`). sigilbuzz left it in place.
- The `Scripts.txt` and `PropertyValueAliases.txt` snapshots under `tests/tools/ucd/` are
  Unicode 18.0.0, and the Script property table they generate moved from
  `sigilbuzz-capi` into the core crate. The C API's `hb_buffer_guess_segment_properties`
  reads it from there and now knows the Unicode 18.0 characters.
- The Indic scripts try HarfBuzz's Indic3 script tags (`dev3`, `bng3`, `gur3`, `gjr3`,
  `ory3`, `tml3`, `tel3`, `knd3`, `mlm3`) before the ones ending in 2
  (`hb_ot_all_tags_from_script` in `hb-ot-tag.cc`), and a run whose font has one shapes
  with the Universal Shaping Engine in place of the Indic shaper, as
  `hb_ot_shaper_categorize` decides. On 2,631 Devanagari strings with a subset of Noto
  Sans Devanagari whose `dev2` script records are renamed `dev3`, the output matches
  HarfBuzz 14.5.0 at every cluster level (before: 1,656, 621 and 1,757 with flags at
  `MonotoneGraphemes`, `MonotoneCharacters` and `Characters`). Fonts without those tags
  shape as before.
- A GPOS mark takes the cross-stream offset of its parent (y in horizontal runs, x in
  vertical ones, summed over the parent's cursive chain) when it attaches, and only the
  parent's main-direction offset at the end of GPOS, as in HarfBuzz 14.5.0
  (`resolve_cross_offset` and `propagate_attachment_offsets`). A lookup that raises a
  base after its mark attached no longer moves the mark. The end-of-GPOS pass resolves
  forward runs from their start and backward runs from their end, each walk following
  at most 64 links, as HarfBuzz does. One Lepcha and one Tibetan test string now match
  HarfBuzz in positions, and so do all marks of `tests/fixtures/attach_chain.ttf`.
- With `BufferFlags::PRODUCE_UNSAFE_TO_CONCAT`, a context or chained context rule set
  of more than four rules follows HarfBuzz's fast path (`RuleSet::apply` and
  `ChainRuleSet::apply`): it reads the one or two glyphs after the cursor first, and a
  rule they rule out marks the cursor through that glyph unsafe to concatenate. Once a
  rule or a ligature of a set of two or more matches, the mark for the rules passed over
  starts at the end of the match, where HarfBuzz leaves the cursor, instead of at the
  cursor. All 1,992 USE test strings, all 2,100 strings of six more USE scripts, and
  all Indic and Khmer test strings now match HarfBuzz 14.5.0 in glyph flags with that
  buffer flag at every cluster level (before: 1,973 to 1,974, 2,077 to 2,081, and 9,621
  to 9,638 of 9,666).
- With `BufferFlags::PRODUCE_UNSAFE_TO_CONCAT`, a context or chained context rule set
  of more than four rules tried at the last glyph of the run passes over the rules that
  need a glyph after it, and they mark the cursor glyph unsafe to concatenate. When a
  later rule matches, HarfBuzz (`RuleSet::apply` and `ChainRuleSet::apply`) starts that
  mark where the match left the cursor, the end of the run, so nothing is marked.
  sigilbuzz now does the same. With `Buffer::guess_segment_properties` on both sides,
  3,774 of the 3,775 Myanmar test strings now match HarfBuzz 14.5.0 in glyph flags with
  that buffer flag at `MonotoneGraphemes` and 3,773 at the other levels (before: 3,771,
  and 3,770 at `Characters`).
- With `BufferFlags::PRODUCE_UNSAFE_TO_CONCAT`, a class-based context or chained context
  rule set of more than four rules in one of the first eight subtables of its lookup
  checks the class of the glyph after the cursor against the classes its rules start
  with, before it reads the glyph after that one, as HarfBuzz's
  `hb_ot_layout_ruleset_digest_t` does. When no rule starts with that class, the cursor
  through that glyph is unsafe to concatenate. When a ZWJ, ZWNJ or other default
  ignorable came after that glyph, sigilbuzz used to take the plain walk over the rules
  instead, which marks nothing.
  With `Buffer::guess_segment_properties` on both sides, all 3,775 Myanmar test strings,
  all 3,468 Chakma test strings and all 3,183 Javanese test strings now match HarfBuzz
  14.5.0 in glyph flags with that buffer flag at every cluster level (before: 3,773 to
  3,774, 3,464, and 3,177 at `MonotoneCharacters` and `Characters`).
- The default GSUB features run in the stages HarfBuzz builds for them
  (`hb_ot_shape_collect_features`). The default, Hebrew and Thai shapers run `ccmp`,
  `locl`, `rlig`, `calt`, `clig`, `liga` and `rclt` (or `vert` in vertical text), the
  direction features with `rtlm`, and the caller's features in one stage, so their
  lookups apply in lookup-index order whatever feature they belong to. `ccmp` and
  `locl` used to run first, the direction features before them, and each other feature
  on its own. The Arabic shaper (`collect_features_arabic`) runs `isol`, `fina`, `medi`
  and `init` in that order, then `rlig`, then `calt`, then `liga`, `clig`, `mset` and
  the rest, in both directions. It used to run `init` before `fina`, `liga` and `clig`
  before `calt`, and no `mset`. Vertical Arabic now takes the default shaper, as in
  HarfBuzz. `tests/fixtures/stage_order.ttf` tests the stages.
- With `BufferFlags::PRODUCE_UNSAFE_TO_CONCAT`, a `kern` or `kerx` table the shaping
  plan applies marks the whole run unsafe to concatenate even when kerning is off (as in
  vertical text), as HarfBuzz's `KerxTable::apply` does. The legacy `kern` table only
  applies to the shapers HarfBuzz lets fall back to it (the default, Arabic, Hebrew and
  Hangul shapers), as `hb_ot_shape_plan_t` decides. All 20 vertical test strings now
  match HarfBuzz 14.5.0 in glyph flags with that buffer flag (before: 15).
- Companion crate releases: `sigilbuzz-capi` 0.3.0, `sigilbuzz-paint` 0.2.0,
  `sigilbuzz-render` 0.9.0, `sigilbuzz-subset` 0.12.0, and `sigilbuzz-svg` 0.2.0 carry
  the breaking changes above. `sigilbuzz-pdf` 0.2.2, `sigilbuzz-gpu` 0.1.1,
  `sigilbuzz-text-layout` 0.1.1, `sigilbuzz-hyphen` 0.1.1, and `sigilbuzz-cli` 0.1.1 are
  patch releases for the fixes below.

Fixed:

A hardening pass for hostile input. Fonts, images, and text can come from anywhere,
and a malformed one must not crash, hang, or exhaust memory. Shipped code no longer
contains `unwrap`, `expect`, or panic macros, apart from `BidiParagraph`'s documented
check that a byte range the caller passes lies on character boundaries (the same
contract as slicing a `str`), and every fix has a regression test.
Fuzzing found the first bugs, and a review of every crate found the rest. Output for
valid input is unchanged except where noted.

- Panics on malformed fonts in CFF (INDEX offsets, charstring operands, subroutine
  indexes), AAT `morx`, the GSUB and GPOS skip iterator, the JPEG decoder, and WOFF2
  wrapping. One panic was reachable with an ordinary font and ordinary text: an Arabic
  letter after a decomposed Thai vowel crashed the Arabic joining step (in `shape()` and
  `hb_shape`), and on longer text it misaligned the joining forms.
- Allocations sized from counts in the file without checking the data behind them: up
  to 17 GB in CFF2, 200 GB in `morx`, 32 GB in `MultiItemVariationStore`, 25 GB in
  contextual rule sets, 8.6 GB in `gvar` subsetting, and 30 GB in the rasterizer.
  Decompression in WOFF1, WOFF2, PNG, JPEG, and TIFF is now capped by what the input
  can plausibly hold.
- Hangs and runaway work: CFF subroutine bombs, composite glyphs that fan out (`glyf`,
  VARC, EBDT), cyclic `morx` chains, nested GSUB and GPOS lookups (now bounded per
  `shape()` call, like HarfBuzz), unbounded buffer growth from multiple substitution and
  `morx` insertion, SVG `<use>` fan-out, COLR paint graphs, and quadratic passes in
  bidi resolution, Indic and USE reordering, mark attachment, line wrapping, and
  subsetting. Hyphenation checked all 4,938 US English patterns at every letter. It now
  checks only the patterns that start with that letter, about 10 times faster with the
  same result.
- `BASE` offsets past 64 KB were truncated, so baseline tags were read from the wrong
  place.
- The `no_std` builds did not compile on Rust 1.81, the declared minimum.
- `sigilbuzz-capi`: `hb_font_paint_glyph` truncated glyph ids above 65535, and a
  language string with an embedded NUL leaked memory on every call. Every `unsafe`
  block now says why it is sound.
- `sigilbuzz-cli`: writing to a closed pipe panicked. It now reports an error.
- `sigilbuzz-woff`: the `woff2` feature did not build without the default features.

The new limits only affect fonts far beyond anything real, for example a glyph with
more than 65,536 points, or a `shape()` call that needs more than 64 lookup
applications per glyph (never fewer than 16,384 in total).

Settings and table data that were read and then ignored:

- Subsetting with `retain_hints` dropped `cvt `, `fpgm`, and `prep`. CFF and CFF2 fonts
  ignored `retain_layout`, `retain_variations`, and `drop_unhandled`, so OTF subsets
  lost GSUB, GPOS, `fvar`, and `HVAR`.
- `sigilbuzz-capi`: `hb_font_set_scale` did not change the output, `hb_blob_create`
  ignored its memory mode and dropped the destroy callback for empty blobs, and
  `hb_shape_full` ignored the shaper list.
- `sigilbuzz-woff`: WOFF1 wrapping wrote the input length as `totalSfntSize` instead of
  the padded size.
- `sigilbuzz-text-layout`: `break_at_word_boundaries = false` did nothing, mandatory
  breaks ignored `max_width`, newline characters counted toward the line width, and a
  lone CR produced no mandatory line break.
- `sigilbuzz-render` SVG glyphs: `stop-opacity` inside a `style` attribute was ignored,
  a trailing `;` in `style` dropped the whole gradient stop, and
  `stroke-linejoin="bevel"` left a notch at every outer corner instead of drawing the
  bevel.

Output that differed from HarfBuzz:

- An Indic script whose lookups the font does not have, so that GSUB picks `DFLT`,
  `dflt` or `latn` for it, shapes with the default shaper, as HarfBuzz's
  `hb_ot_shaper_categorize` decides. So does Myanmar in such a font or one with only
  `mymr` lookups. Bengali text in Noto Sans Devanagari used to get the Indic shaper, so
  a leading stress sign got a dotted circle and stress signs shared their letter's
  cluster at `MonotoneCharacters`. 336 stress sign strings now all match HarfBuzz
  14.5.0 (before: 328, and 312 at `MonotoneCharacters`).
- Vertical text no longer runs `liga`, `clig`, `calt` and `rclt` by default. HarfBuzz
  turns them on for horizontal text only (`horizontal_features` in `hb-ot-shape.cc`).
  Vertical text now gets `vert` alone. sigilbuzz used to prefer `vrt2` when the font had
  it, and to fall back to it when the caller turned `vert` off. `vrt2` now runs only when
  the caller turns it on, as in HarfBuzz, and a caller can also turn `vert` on in
  horizontal text.
- Mark positioning ignored AnchorFormat3 device tables, so marks in variable fonts stayed
  at the default instance. VariationIndex deltas now apply to mark, mark-to-mark,
  mark-to-ligature and cursive anchors.
- PairPos format 1 device offsets are measured from the PairSet, as the spec says
  (variable kerning in fonts like Rubik was wrong). The `var_kern.ttf` fixture is
  rebuilt spec-correct by a Rust builder.
- Default ignorables were hidden by matching cluster values, so a visible glyph that
  shared a cluster with one lost its advance. `Glyph::unicode_props` was written but
  never read. It now carries the per-glyph flag, and a GSUB substitution un-hides the
  glyph, as in HarfBuzz.
- Indic and USE shaping of runs that do not start the text.
- The bidi pairs for the tick square brackets (U+298D to U+2990).
- Vertical runs now start each glyph from its vertical origin.
- `sigilbuzz-capi`: `hb_buffer_set_script` and `hb_buffer_set_language` did nothing.
  `hb_buffer_add_utf8` and `hb_buffer_add_utf16` reported clusters in the wrong units,
  ignored the text around the item, and dropped a whole call on malformed input instead
  of replacing bad code units with U+FFFD.
- `sigilbuzz-capi`: `hb_set_t` kept its contents in a `RefCell` while claiming to be
  thread-safe, so two threads reading one set (two `hb_subset_or_fail` calls sharing an
  input) could corrupt it and abort. It now sits behind a lock.
- `sigilbuzz-capi`: `hb_font_paint_glyph` ignored `palette_index`, `foreground_color`
  and the font's variation coordinates, fired nothing for glyphs without color data,
  and did not paint COLRv0 layers.
- `sigilbuzz-paint`: sweep gradient angles lacked the COLRv1 half-turn bias, and
  variable scale, rotate, skew, affine and sweep-angle deltas were added in raw units.
- `sigilbuzz-render`: `rasterize_colrv1_glyph` ignored its `palette_index`, and sweep
  gradients were mirrored.
- The COLR v1 header was read with four offsets instead of five, so
  `Colr::var_store_offset` returned the DeltaSetIndexMap offset and variable COLRv1 fonts
  built to the spec got no deltas. A v1 table cut short before its last offset is now
  rejected.
- COLRv1 rendering in `sigilbuzz-render`, `sigilbuzz-svg` and `evaluate_with`: a
  transform below a `PaintGlyph` distorted the glyph outline (visible on gradient emoji),
  composites blended into earlier layers instead of isolated groups, linear gradients
  ignored their rotation point p2, and radial gradients under a non-uniform scale were
  approximated.
- `sigilbuzz-subset`: the subsetter dropped GDEF `MarkGlyphSetsDef`, `AttachList`,
  `LigCaretList` and the ItemVariationStore. Dropping the mark glyph sets broke every
  lookup that uses a mark filtering set, and dropping the store left subset variable
  fonts without their kerning deltas.
- `sigilbuzz-subset`: the instancer resolved AnchorFormat3 and PairSet device offsets
  against the wrong table, so instanced fonts kept default-instance anchors and kerning,
  and it now folds ligature caret variations too.
- `sigilbuzz-subset`: GPOS Device and VariationIndex tables were not copied into
  subsets, leaving dangling offsets. Static subsets now leave VariationIndex tables out.
- `sigilbuzz-subset`: rebuilt GSUB and GPOS tables over 64 KiB wrapped their 16-bit
  offsets and silently corrupted their lookups and subtables. Lookups are promoted to
  Extension lookups, oversized mark and PairPos format 1 subtables are split the way
  HarfBuzz splits them, and any other overflow is reported as an error.
- `sigilbuzz-subset`: context rules with no lookup records (`ignore sub`,
  `ignore pos`) were dropped, so guarded rules such as Amiri's Allah ligature shaped
  differently after subsetting.
- `sigilbuzz-subset`: a malformed GDEF piece is left out instead of failing the whole
  subset. There are no more panics on truncated mark arrays or SinglePos headers, or on
  32-bit targets from crafted GDEF offsets. Full and partial instancing rebuild GDEF
  instead of truncating it at the store, and partial instancing renumbers
  VariationIndex rows.
- `sigilbuzz-subset`: subsets and instances of variable fonts keep GSUB and GPOS
  FeatureVariations. Subsets remap their indices, full instances apply the record that
  matches their coordinates, and partial instances settle pinned-axis conditions and
  renumber the kept axes. They used to be written as version 1.0 and lose them.
- `sigilbuzz-subset`: the closure keeps GSUB reverse chaining (type 8) substitutes.
- `sigilbuzz-subset`: partial instancing checks every ItemVariationStore, HVAR, VVAR
  and MVAR offset and size (no wraparound on 32-bit targets). A table it cannot rebuild
  is dropped and reported instead of carried through with stale axes, and MVAR records
  past 64 KiB are an error.
- A null ClassDef offset is read as every glyph in class 0, as in HarfBuzz. The shaper
  used to parse the subtable itself as the ClassDef, so chained context format 2
  subtables with a null backtrack ClassDef (as fontmake writes them) failed to parse or
  matched invented classes. PairPos format 2 and the subsetter's class-based rewriters
  had the same bug.
- A multiple substitution with an empty sequence deletes its glyph, as HarfBuzz's
  `Sequence::apply` does, and the glyph's cluster merges into a neighbor the way
  `delete_glyph` merges it. It used to leave the glyph in place. Noto Sans Lepcha
  deletes vowel signs this way.
- A GPOS lookup whose contextual or chained contextual subtable does not match at a
  glyph goes on to its next subtable, as in HarfBuzz. sigilbuzz stopped trying the
  lookup there, so later subtables never applied: Amiri's kerning, for one, lost its
  hamza and teh marbuta rules.
- A GSUB feature that only some glyphs carry (the Arabic, Mongolian and N'Ko positional
  forms, Indic `half`, `rtlm`) checks its mask at every input glyph a rule matches, as
  HarfBuzz's skipping iterator does (`matcher_t::may_match`), not only at the cursor:
  a ligature or contextual rule no longer matches across a glyph the feature is off at.
  Contextual lookups of such features used to run over the whole run. They now start
  only where the feature is on. The mask moves with its glyph through all of the
  feature's lookups, where it used to stay at its index when an earlier lookup changed
  the run's length. Every GPOS feature applies to every glyph, as in HarfBuzz, so GPOS
  matching has no mask to check.
- The bidi algorithm passes every line of the Unicode 17.0 BidiTest.txt (3,878 failed
  before) and BidiCharacterTest.txt (19 failed before). An isolate inside a directional
  override opens at its own direction and still matches its PDI (X5a to X5c, BD9), an
  override leaves boundary neutrals to rule X9 (X6), a paragraph separator takes the
  paragraph level (X8), marks after a bracket that N0 resolves take its type, bracket
  pairing stops when the stack is full, and U+2329 and U+232A pair with U+3008 and
  U+3009 (BD16).
- `bidi_class` and the paired-bracket table are generated from the Unicode 17.0
  `DerivedBidiClass.txt` (with the defaults of its `@missing` lines) and
  `BidiBrackets.txt` by `tests/unicode_table_gen.rs`, like the other UCD tables. The
  hand-picked tables had thousands of wrong values (Devanagari and other Indic marks as
  L instead of NSM, Samaritan, Mandaic, Adlam and other right-to-left scripts as ON,
  U+002A as ET, U+06F0 to U+06F9 as AN) and 30 of the 128 bracket pairs missing.
  `BidiParagraph` levels change for such text. `shape` changes only where the class
  decides the native direction, for scripts sigilbuzz has no shaper for: text in
  Samaritan, Mandaic, Adlam, Kharoshthi, Phoenician, Garay and the other right-to-left
  scripts is now reversed like HarfBuzz reverses it, and Old Hungarian, Old Italic, Runic
  and Tifinagh, which HarfBuzz gives no native direction, are never reversed.

Removed:

- The root crate's `alloc` feature. It gated nothing: the crate always needs `alloc`.
  `std` now only adds filesystem helpers such as `Blob::from_path`.
- `Buffer::set_text_bidi`. It reordered the text into visual order and shaped it left
  to right, which broke joining and mark attachment across direction changes. Shape a
  `BidiParagraph` instead.
- `Buffer::{set_normalize_nfc, normalize_nfc}` and
  `unicode::normalize::{compose_pair, compose_str}`. Normalization always runs now.
- `tables::layout::SkipIter`, `MatchFilter::{next_unskipped, prev_unskipped}`, and the
  `matches_filtered` context matchers, replaced by the `MatchGlyph` and `MatchContext`
  matching in `tables::layout::skip_iter`.
- The hidden `unicode::use_category` module (`UseCategory`, `UsePosition`,
  `use_category`, `use_position`, `is_hangul_l`, `is_hangul_v`, `is_hangul_t`) and
  `unicode::indic_category` module (`IndicSyllabicCategory`, `IndicPositionalCategory`,
  `syllabic_category`, `positional_category`). The Universal Shaping Engine reads its
  generated category table, and the Myanmar pass keeps the Myanmar part of the old
  table. Nothing else read them.
- The hidden `ot::tibetan::TIBT_FEATURES`. Tibetan runs every feature of the Universal
  Shaping Engine.

## 0.21.0 (2026-04-25)

`sigilbuzz-render` gains two more embedded-image formats and finishes SVG masks and
text on a path.

- sbix TIFF decoder (#240): baseline TIFF with II/MM headers, a single IFD, 8-bit RGB
  and RGBA, no compression or PackBits, strip-organized, chunky planar. CCITT, LZW,
  JPEG-in-TIFF, multiple IFDs, and other photometrics return `Unsupported`.
- Progressive JPEG (#241): SOF2 decoding with DC first and refinement scans, plus AC
  first scans with EOB-run tracking. AC refinement scans return a `BadJpeg` error for
  now.
- SVG `mask-type="alpha"` and `maskUnits="objectBoundingBox"` (#242). Nested masks are
  still unsupported.
- SVG `<textPath>` (#243): places glyph runs the caller has already shaped along a
  path by arc length. Tangent rotation and `side="right"` are not done yet.

Fixed:

- A mask that referenced itself could overflow the stack. A depth guard now stops it
  (#244).

## 0.20.0 (2026-04-25)

- sbix JPEG decoder (#237): a small baseline JPEG decoder with Huffman decoding, a
  float IDCT, and YCbCr to RGB conversion. Handles 3-component and grayscale images at
  4:4:4, 4:2:2, and 4:2:0 sampling.
- SVG `<mask>` (#236), using luminance from BT.709.
- Stable crate-root names (#238). These items were only reachable through hidden
  `ot::*` and `unicode::*` paths. They are now exported from the crate root and covered
  by the stability commitment: the `feature` tag constants, `JoiningForm`,
  `UnicodeScript`, `script_of`, `is_hangul_jamo`, `BidiInfo`, `BidiClass`,
  `bidi_class`, and `JoiningType`.
- `clippy::pedantic` now applies to every crate in the workspace, with each allowed lint
  justified in `Cargo.toml` (#239).

## 0.19.0 (2026-04-25)

- SVG filter primitives (#233): `feGaussianBlur`, `feColorMatrix`, `feOffset`,
  `feFlood`, and `feMerge`. A drop-shadow chain works end to end.
- Stroke dashes now follow true Bezier arc length (#234). Straight-segment paths render
  exactly as before.
- Pre-publish API audit (#235). About 75 exports are documented as stable in
  `docs/STABILITY.md`. Internal modules are marked `#[doc(hidden)]`, and
  `missing_docs = warn` applies to every crate.

Fixed:

- The PNG decoder accepted a malformed grayscale `tRNS` chunk (#231, #232).

## 0.18.0 (2026-04-25)

- `flatten_grouped()` in `sigilbuzz-render` returns flattened segments grouped by the
  curve they came from, which MSDF generators need for edge coloring (#218, #224).
- SVG `<polygon>`, `<polyline>`, `<line>`, `stroke-dasharray`, and
  `stroke-dashoffset` (#227).
- EBDT formats 8 and 9, composite monochrome bitmaps (#229).
- VARC subsetting now drops unused variation regions (#228).

Fixed:

- Rasterizing SVG or bitmap glyphs at huge sizes could panic while allocating the
  pixel buffer. Output is now capped at 16384 pixels per side and returns `BadSize`
  (#225, #226, #230).

## 0.17.0 (2026-04-25)

- PNG encoder in `sigilbuzz-render`: `encode_png` and `encode_png_alpha` write
  deterministic PNGs (#217).
- EBDT/EBLC monochrome bitmaps and sbix `dupe` glyphs (#221). Other sbix image types
  return `UnsupportedBitmap` instead of panicking.
- SVG strokes, linear and radial gradients, `<use>`, basic `clipPath`, `<rect>`,
  `<circle>`, and `<ellipse>` (#219).
- VARC subsetting prunes unused variation data (#220).

Fixed:

- `sigilbuzz-svg` and `sigilbuzz-pdf` wrote `NaN` and `inf` into their output for
  non-finite numbers. Both now write 0 (#216, #222).

## 0.16.0 (2026-04-25)

Three gaps found while moving an MSDF glyph generator onto sigilbuzz.

- `flatten()`, `Segment`, and `DEFAULT_TOLERANCE` are public in `sigilbuzz-render`
  (#208, #211).
- `Face::glyph_outline` works for CFF2 fonts (#209, #213). This also fixed two CFF2
  bugs: INDEX counts were read as 16-bit instead of 32-bit, and `blend` could corrupt
  the operand stack at default coordinates.
- `name` table parser with `Face::name()` (#210, #212).

## 0.15.0 (2026-04-25)

`sigilbuzz-render` now handles every kind of glyph a modern emoji font ships.

- COLRv1 rendering (#204): linear, radial, and sweep gradients with pad, repeat, and
  reflect, Porter-Duff compositing, clipping, nested color glyphs, and variations.
- SVG-in-OT rendering (#205): paths, transforms, and fills. Gzipped SVG documents
  return `SvgGzipped`.
- CBDT and sbix PNG bitmaps (#207), with a small PNG decoder that uses `miniz_oxide`
  for inflate.

Fixed:

- The rasterizer could panic on non-finite or extreme coordinates (#202).
- COLRv0 rendering accepted an out-of-range palette index when every layer used the
  foreground color (#203).

## 0.14.0 (2026-04-25)

- Partial instancing of `gvar` fonts (#194) and CFF2 fonts (#195): pin some axes and
  keep the rest variable.
- VARC subsetting (#193).
- New crate `sigilbuzz-render` (#199): a CPU rasterizer for outlines and COLRv0 color
  glyphs, with 8x vertical supersampling and variable-font support.
- Non-finite inputs no longer break partial instancing math (#185, #186, #192).

Fixed:

- VARC component glyph IDs above 0xFFFF were silently truncated (#196).
- CFF2 operand decoding missed the `shortint` form (#197).
- A CFF2 `return` inside an inlined subroutine cleared the caller's stack tracking
  (#198).

## 0.13.0 (2026-04-25)

- VARC variable composite glyphs from OpenType 1.10 (#184), including the new
  `MultiItemVariationStore`. Outlines resolve through `Face::glyph_outline_at_coords`.
- Partial instancing API, `AxisPin::{Pin, Keep}` (#183), with `fvar`, `avar`, and
  variation-store rewriting (#190).
- `instance()` now folds GPOS variation deltas into static values (#175, #188).
- The C API test build uses a cross-process file lock, so parallel test runs no
  longer race (#172).

Fixed:

- CFF operand re-encoding corrupted 5-byte values outside the i16 range. It now
  returns `Unsupported` (#187, #189).

## 0.12.0 (2026-04-25)

- `instance()` bakes CFF2 `blend` operators and applies `VVAR` and `MVAR` metrics
  (#163).
- AAT `kerx` format 4 control-point and anchor-point actions, with a new `ankr` parser
  and `Face::glyph_points` (#166).
- CFF subsetting now prunes unused subroutines (#167).
- OpenType `BASE` table (#170).
- New crate `sigilbuzz-hyphen` (#171): Liang hyphenation with bundled US English
  patterns and an optional bridge to `sigilbuzz-text-layout`.

Fixed:

- Two bugs in the instance bake: dropped VVAR advance deltas (#176, #177) and
  duplicate MVAR tags applied twice (#178, #179).

## 0.11.0 (2026-04-25)

- Variable-font instancing (#161): `sigilbuzz_subset::instance` bakes a set of axis
  coordinates into a static font.
- `MVAR` and `VVAR` variable metrics (#162).
- The OpenType `MATH` table, all five subtables (#164). Math layout itself is up to
  the caller.
- UAX 9 paired-bracket handling (#148), CFF2 subsetting for single-FD fonts, and
  `kerx` format 4 inline-coordinate actions (#165).

## 0.10.0 (2026-04-25)

- Full UAX 9 bidi ordering (#147), with `BidiInfo` and `Buffer::set_text_bidi`.
- New crate `sigilbuzz-text-layout` (#149): UAX 14 line breaking, width-based line
  wrapping, and a simplified UAX 29 word iterator.
- Real CFF1 and CFF2 fixture fonts for the subsetter tests (#150).
- AAT `kerx` format 6, `morx` formats 4 and 5, and parsing for `kerx` format 4 (#151).

Fixed:

- FSI direction was hard-coded to RTL (#155).
- `wrap_lines` reported widths that ignored its own trailing-space trim (#153), and
  trimmed only 3 of the 10 UAX 14 space characters (#159).
- `kerx` format 6 was missing a bounds check (#157).

## 0.9.0 (2026-04-25)

- WOFF1 zlib compression and decompression through `miniz_oxide`, behind the
  `woff1-deflate` feature (#137).
- CFF subsetting handles global subroutines shared across font DICTs (#138).
- AAT `kerx` format 1 state-machine kerning (#139).
- New crate `sigilbuzz-cli` (#141): the `sigilbuzz` binary with `shape`, `subset`,
  `paint`, `slug`, `svg`, `woff`, `pdf`, and `info` subcommands.

Fixed:

- A WOFF2 test panicked under `--no-default-features` (#142, #143).
- CLI glyph-range parsing was off by one (#144, #145).

## 0.8.0 (2026-04-25)

- C API additions: `hb_set_t`, `hb_subset_*`, `hb_paint_funcs_t` with
  `hb_font_paint_glyph`, `hb_face_collect_unicodes`, and
  `hb_ot_layout_collect_features` (#102, #103, #104).
- The subsetter rewrites every GSUB and GPOS lookup type at the byte level (#109,
  #112, #121, #127, #128, #131).
- Full CFF subsetting: non-CID CFF1, CID-keyed CFF1, and CFF2 (#120, #135).
- Brahmi, Sharada, Khojki, Tirhuta, and Modi through the Universal Shaping Engine
  (#123). `nukt` and `akhn` were added to the USE basic features.

Fixed:

- Mongolian ligatures advanced the cursor by the input span instead of the output
  span (#118, #134).
- Mark advances are zeroed at the right stage for each script, found through Limbu
  (#115, #124).
- Cham `pref` ordering (#116) and N'Ko cursive joining (#114).
- The CFF charset and Encoding writers overflowed or truncated on large inputs (#129,
  #130, #132, #133).

## 0.7.0 (2026-04-25)

- New crate `sigilbuzz-capi`: a C library exporting HarfBuzz's `hb_*` symbols, so C
  programs can link it in place of HarfBuzz. Ships with a header, pkg-config template,
  and CMake module.
- CFF emitter building blocks for the subsetter, and byte-level GSUB and GDEF
  rewriting for the first lookup types.
- `wrap_woff2`, the WOFF2 compression direction.
- CBDT/CBLC and sbix color bitmap tables, and the `SVG ` table.
- Tibetan, Mongolian, and eight more USE scripts: N'Ko, Buginese, Tai Tham, Balinese,
  Sundanese, Lepcha, Limbu, and Cham.
- Each GSUB lookup now runs once per pass, matching HarfBuzz.

## 0.6.0 (2026-04-24)

- The subsetter keeps GSUB, GPOS, and GDEF when it is safe to, and keeps variable-font
  tables by default.
- Complex-script shaping got much faster (#74, #75, #76). Devanagari went from 173x
  slower than rustybuzz to 4.98x, Arabic from 32x to 2.72x, and Khmer from 17x to
  1.88x. Latin and Hebrew are now faster than rustybuzz. See
  [docs/PERFORMANCE.md](docs/PERFORMANCE.md).
- New crate `sigilbuzz-woff`: WOFF1 and WOFF2 unwrap, plus uncompressed WOFF1 wrap.
  This added the workspace's first runtime dependency (Brotli), confined to this crate.

Fixed:

- The Coverage writer wasted bytes on non-identity inputs (#94, #95).
- WOFF2 empty-glyph and bbox-bitmap alignment (#96, #97), and `transformVersion`
  validation (#98, #99).

## 0.5.0 (2026-04-24)

- New crate `sigilbuzz-subset`, the `hb-subset` equivalent for TrueType fonts.
- `sigilbuzz-pdf` gains Type 1 and embedded OpenType fonts.
- Criterion benchmarks against rustybuzz.
- Every crate is ready for crates.io, with full metadata and the Apache 2.0 license
  text.
- Composite glyphs anchored to phantom points resolve correctly.

Fixed:

- Type 3 PDF fonts returned an infinite font matrix for malformed faces.

## 0.4.0 (2026-04-24)

- Amiri `rlig` parity (#21). GSUB now applies lookups the way HarfBuzz does. Amiri
  parity with rustybuzz went to 6710 of 6710.
- AAT `kerx` format 2.
- New crate `sigilbuzz-svg`: glyph outlines and COLRv1 glyphs as SVG.
- New crate `sigilbuzz-pdf`: Type 3 font output.

Fixed:

- Composite glyphs with a 2x2 transform read the matrix in the wrong order.
- COLRv1 variable alpha and color-stop values were scaled wrong, and fonts that keep
  their variation store in GDEF ignored variations.
- An SVG path list could misalign when a glyph had no outline, and one malformed
  `kerx` subtable could spoil the whole table.

## 0.3.0 (2026-04-24)

- The repository became a Cargo workspace.
- Glyph outlines from `glyf` (including composites and `gvar`), CFF, and CFF2, through
  `Face::glyph_outline` and `Face::glyph_outline_at_coords`.
- New crate `sigilbuzz-gpu`: Slug outline encoding for GPU rendering.
- New crate `sigilbuzz-paint`: COLRv1 paint evaluation.
- Myanmar kinzi (#44), Thai and Lao AM decomposition (#45), and Old Hangul in mixed
  buffers (#46).

Fixed:

- CFF stack depth cap, Slug handling of NaN and zero tolerance, and a CFF `hflex1`
  operand index.

## 0.2.0 (2026-04-24)

- Universal Shaping Engine with Khmer, Myanmar, Thai, Lao, and Old Hangul.
- The rest of the Indic family: Bengali, Gurmukhi, Gujarati, Oriya, Tamil, Telugu,
  Kannada, Malayalam, and Sinhala.
- Hebrew, including niqqud and cantillation marks.
- Mixed-script buffers split into script runs automatically.
- AAT `morx` and `kerx` fallback when a font has no GSUB or GPOS.
- GPOS variation deltas in variable fonts (#13).

## 0.1.0 (2026-04-24)

The first release.

- Core API: `Blob`, `Face`, `Font`, `Buffer`, `Glyph`, and `shape`.
- Parsers for `head`, `maxp`, `hhea`, `hmtx`, `cmap`, `GDEF`, `loca`, and `glyf`.
- Every GSUB and GPOS lookup type, lookup flags, and script and language selection.
- Arabic joining and Devanagari reordering.
- Vertical text and the legacy `kern` table.
- Variable fonts: `fvar`, `avar`, `gvar`, `HVAR`, and `Font::with_coords`.
- COLRv0, COLRv1, and CPAL.
- Latin shaping matches rustybuzz output on Open Sans.
