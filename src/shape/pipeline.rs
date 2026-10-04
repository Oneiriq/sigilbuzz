//! The [`shape`] entry point: cmap lookup, script segmentation, the
//! per-segment pre-shaper and GSUB pass, advances, and positioning,
//! in that order.

use alloc::vec::Vec;

use super::aat::apply_morx;
use super::features::{
    apply_arabic_positional_features, apply_gsub_features_merged_budgeted, apply_stch,
    run_default_gsub, DefaultGsub, DefaultShaper,
};
use super::hangul;
use super::normalize::{self, Normalizer};
use super::required::SegmentShaper;
use super::segment::{build_segments, guess_script, remap_segments, ProcessedSegment, Segment};
use super::shaper::Shaper;
use super::{
    arabic_joining, cluster, glyph_flags, ignorables, native_direction, position, required, rotate,
    stch, thai, vowel_constraints, Feature, JoinerTable, LookupBudget, VarCtx,
};
use crate::buffer::{script_priority_for, Buffer, BufferFlags, Direction, Glyph, ShapedRun};
use crate::error::{Error, Result};
use crate::font::{f2dot14_coords, Font};
use crate::ot::arabic::{assign_from_types_in_context, JoiningContext, JoiningForm};
use crate::unicode::joining::{joining_type, JoiningType};
use crate::unicode::{script_of, Script};

/// Whether text whose first character is `first` takes the implicit
/// vertical layout of Mongolian: `first` is a character of the
/// Mongolian script in the Mongolian block (U+1800..U+18AF: a letter, a
/// digit, the birga, a variation selector). The Common punctuation of
/// the block and the ornaments of the Mongolian Supplement start no
/// vertical text, and neither does a space, a digit, a quotation mark,
/// or a letter of another script before the Mongolian.
fn starts_mongolian(first: char) -> bool {
    matches!(first as u32, 0x1800..=0x18AF) && script_of(first) == Script::Mongolian
}

/// Shapes `buffer` against `font` with optional feature overrides.
///
/// Feature tags with `value: 0` disable the corresponding feature
/// for this call. Non-zero values enable a feature if the font
/// supports it. Unknown tags are accepted and ignored rather than
/// returning an error. When the list names a tag more than once, the
/// last entry wins, as in HarfBuzz: `liga=0` then `liga=1` turns
/// ligatures on.
///
/// # Output order
///
/// Glyphs come back in visual order, like HarfBuzz: logical order for
/// LTR and TTB, reversed after positioning for RTL and BTT. See the
/// `ShapedRun` docs for the exact contract.
///
/// # Errors
///
/// Returns an error if the font is missing any of the tables required
/// for basic shaping (`cmap`, `maxp`, `hhea`, `hmtx`) or if one of
/// them is malformed. Returns [`Error::Unsupported`] when the text is
/// longer than `u32::MAX` bytes, since [`Glyph::cluster`] is a `u32`
/// byte offset.
// The pipeline is a straight-line sequence of passes so the order is
// visible in one place.
pub fn shape(font: &Font<'_>, buffer: &Buffer, features: &[Feature]) -> Result<ShapedRun> {
    // A tag the list repeats takes its last value, as in HarfBuzz, so
    // every pass below sees each tag once.
    let features = super::last_values(features);
    let features: &[Feature] = &features;
    // Vertical layout: explicit when the buffer direction is TTB/BTT,
    // *implicit* for Mongolian text when the caller never chose a
    // direction (see `starts_mongolian`). Mongolian's traditional
    // writing axis is top-to-bottom; auto-vertical here lets simple
    // callers shape Mongolian without having to know the default.
    // HarfBuzz has no such default. An explicit direction always wins,
    // so `set_direction(Direction::Ltr)` gives horizontal Mongolian.
    let mongolian_dominant = buffer.text().chars().next().is_some_and(starts_mongolian);
    // The direction every later pass works with: the buffer's, or TTB
    // for implicit vertical Mongolian.
    let direction = if !buffer.has_explicit_direction() && mongolian_dominant {
        Direction::Ttb
    } else {
        buffer.direction()
    };
    // The direction the output is laid out in, which HarfBuzz builds
    // its shape plan for (it picks `ltra`/`ltrm` or `rtla`/`rtlm`),
    // before a non-native direction flips the shaping direction below.
    let target_direction = direction;
    let is_vertical = !direction.is_horizontal();

    let face = font.face();
    // HarfBuzz keeps normalized coordinates as F2DOT14 integers
    // (`hb_font_t::coords`), so every variation below reads the
    // caller's coordinates rounded to multiples of 1/16384, halves up.
    // Coordinates that all round to zero are the default instance,
    // where HarfBuzz reads no variations at all.
    let rounded_coords = f2dot14_coords(font.coords());
    let coords = rounded_coords.as_slice();
    let cmap = face.cmap()?;
    let hmtx = face.hmtx()?;

    let text = buffer.text();
    if text.is_empty() {
        return Ok(ShapedRun::default());
    }
    // Clusters are `u32` byte offsets. Past this length every
    // `as u32` cluster cast below would wrap.
    if u32::try_from(text.len()).is_err() {
        return Err(Error::Unsupported {
            context: "text longer than u32::MAX bytes",
        });
    }

    // Step 1: the characters to shape, before normalization maps them
    // to glyphs. Clusters are byte offsets from the start of the text
    // so later passes can track which input characters coalesce into
    // a single output glyph; until normalization, `glyphs` only
    // carries those clusters.
    //
    // Default-ignorable characters (ZWJ, ZWNJ, bidi controls,
    // variation selectors, ...) map through cmap like any other, so
    // GSUB rules that name their glyphs still match; the passes in
    // the `ignorables` module hide them after positioning, as
    // HarfBuzz does.
    //
    // We also keep the `char` list alongside the glyphs so the
    // complex shapers can consult Unicode properties per code point
    // without re-scanning the UTF-8 stream.
    let mut glyphs: Vec<Glyph> = Vec::with_capacity(text.len());
    let mut codepoints: Vec<char> = Vec::with_capacity(text.len());
    // `(byte_offset, char)` for each character. Hangul syllables
    // compose later, in the Hangul preprocessing below.
    let composed_chars: Vec<(u32, char)> =
        text.char_indices().map(|(i, c)| (i as u32, c)).collect();
    // Which characters continue the grapheme before them, read off the
    // text before any character is split below (HarfBuzz sets the bit
    // in `hb_set_unicode_props`, ahead of its own splits). `cont` has
    // one entry per code point; the later parts of a split character
    // continue its first part.
    let typed: Vec<char> = composed_chars.iter().map(|&(_, c)| c).collect();
    let typed_cont = cluster::continuations(&typed);
    let mut cont: Vec<bool> = Vec::with_capacity(text.len());
    let flags = buffer.flags();
    let level = buffer.cluster_level();
    // Backward runs mirror paired punctuation (see `rotate`); this
    // says which entries of `codepoints` were replaced.
    let backward = !direction.is_forward();
    let mut mirrored_mask: Vec<bool> = Vec::with_capacity(text.len());
    // HarfBuzz's `hb_insert_dotted_circle`: a paragraph start (BOT)
    // with no pre-context that opens with a combining mark gets a
    // dotted circle, with the mark's cluster, for the mark to sit on.
    if flags.contains(BufferFlags::BOT)
        && !flags.contains(BufferFlags::DO_NOT_INSERT_DOTTED_CIRCLE)
        && buffer.pre_context().is_empty()
        && typed.first().is_some_and(|&c| cluster::is_unicode_mark(c))
        && cmap.glyph_id('\u{25CC}').is_some()
    {
        if let Some(&(cluster, _)) = composed_chars.first() {
            glyphs.push(Glyph::new(0, cluster));
            codepoints.push('\u{25CC}');
            cont.push(false);
            mirrored_mask.push(false);
        }
    }
    for (k, (cluster, ch)) in composed_chars.iter().copied().enumerate() {
        if let Some(parts) = split_before_cmap(ch) {
            for (n, &component) in parts.iter().enumerate() {
                glyphs.push(Glyph::new(0, cluster));
                codepoints.push(component);
                cont.push(n > 0 || typed_cont[k]);
                mirrored_mask.push(false);
            }
            continue;
        }
        cont.push(typed_cont[k]);
        let (ch, mirrored) = if backward {
            rotate::mirror(ch, &cmap)
        } else {
            (ch, false)
        };
        glyphs.push(Glyph::new(0, cluster));
        codepoints.push(ch);
        mirrored_mask.push(mirrored);
    }
    // `hb_form_clusters`: at the grapheme levels each grapheme takes
    // one cluster.
    cluster::form_clusters(&mut glyphs, &cont, level);

    // Step 1.5: Segment the run into maximal same-script spans. Each
    // segment carries its own script priority (e.g. Arabic `arab` ->
    // DFLT, Hebrew `hebr` -> DFLT), its codepoint range in the
    // `codepoints` vec we just filled, and (after we finish GSUB
    // below) its post-substitution glyph range. Pre-GSUB the two
    // ranges coincide: normalization (below) keeps one glyph per code
    // point.
    //
    // Running each segment through its own cmap -> pre-shaper -> GSUB
    // -> GPOS chain is what lets mixed-script runs like `Hi שלום`
    // dispatch the Hebrew half under `hebr` features and the Latin
    // half under `latn` in a single call. A single global priority
    // would miss script-specific lookups on whichever half lost the
    // tie-break.
    //
    // A caller-set script (Buffer::set_script) replaces the
    // segmentation: the whole buffer is one run under that script, the
    // way HarfBuzz shapes one buffer with one script.
    //
    // The buffer's script, as HarfBuzz guesses it: the caller's, or
    // that of the first script-bearing character in the text's order.
    let buffer_script: Option<Script> = buffer
        .script()
        .or_else(|| guess_script(codepoints.iter().copied()));
    // A caller-chosen direction that is not the script's native one
    // reads the text as already in that visual order: shape its
    // graphemes reversed, in the native direction (see
    // `native_direction`). Mirroring above followed the caller's.
    // HarfBuzz decides this for a buffer of one script; a buffer that
    // sigilbuzz splits into several script runs keeps the direction.
    let one_run = buffer.script().is_some() || build_segments(&codepoints).len() <= 1;
    let direction = if buffer.has_explicit_direction() && one_run {
        let native = native_direction::resolve(direction, buffer_script, &codepoints);
        if native != direction {
            let run = native_direction::Run {
                cps: &mut codepoints,
                glyphs: &mut glyphs,
                mirrored: &mut mirrored_mask,
            };
            native_direction::reverse_graphemes(run, &cont, level);
        }
        native
    } else {
        direction
    };
    // The buffer language picks each script's language system for
    // every GSUB and GPOS feature lookup, including the ones the
    // complex shapers run (see `crate::ot::layout_select`).
    let language_tags: &[[u8; 4]] = buffer
        .language()
        .map_or(&[], crate::Language::ot_language_tags);
    let concat = flags.contains(BufferFlags::PRODUCE_UNSAFE_TO_CONCAT);
    // GDEF is consulted up-front: its ItemVariationStore feeds the
    // FeatureVariations conditions below, and the LookupFlag
    // skip-iterator needs its classes for every GSUB context match.
    // GPOS reuses the same handle further down.
    let gdef = face.gdef()?;
    // Each table's FeatureVariations record for the font's coordinates
    // (`hb_ot_layout_table_find_feature_variations`), picked once per
    // call: there is no shape plan cache. HarfBuzz selects one even at
    // the default instance, where every axis reads as 0. HarfBuzz
    // 14.5.0 rejects a GSUB or GPOS whose FeatureVariations fail its
    // sanitizer, so a table whose FeatureVariations do not parse is
    // left out here, as if the font had none.
    let var_store = gdef.as_ref().and_then(|g| g.item_variation_store());
    let select = |variations| {
        crate::tables::layout::feature_variations::select(variations, coords, var_store)
    };
    // What the font keeps between calls: the lookup accelerators, which
    // let every pass below skip the lookups and subtables a glyph cannot
    // start without parsing them, the resolved language systems, and the
    // per-glyph metrics that walk outlines. A font's first call builds
    // none of it: a font shaped once, which many callers build per run,
    // would spend more building them than they save.
    let warm = font.caches().note_use();
    let face_cache = warm.then(|| font.caches().face());
    let gsub = face.gsub()?.and_then(|g| {
        let variation = select(g.feature_variations().ok()?);
        let cache = face_cache.map(|c| c.gsub(g.lookup_list().len(), g.table_len()));
        Some(
            g.with_language_tags(language_tags)
                .with_cluster_level(level)
                .with_unsafe_to_concat(concat)
                .with_feature_variation(variation)
                .with_cache(cache),
        )
    });
    let gpos = face.gpos()?.and_then(|g| {
        let variation = select(g.feature_variations().ok()?);
        let cache = face_cache.map(|c| c.gpos(g.lookup_list().len(), g.table_len()));
        Some(
            g.with_language_tags(language_tags)
                .with_feature_variation(variation)
                .with_cache(cache),
        )
    });

    // The shaper HarfBuzz would pick for the whole buffer, from its
    // script and the script tag the font's GSUB picks for it. It runs
    // its `preprocess_text` on the whole buffer and decides mark
    // zeroing and fallback mark positioning. Each segment's own script
    // picks its GSUB, and its normalization unless the buffer's shaper
    // shapes it (`Shaper::normalizer_for`).
    let buffer_shaper = {
        let script = buffer_script.unwrap_or(Script::Other);
        let priority = script_priority_for(script);
        Shaper::for_run(script, !is_vertical, gsub.as_ref(), priority)
    };
    // The rest of HarfBuzz's SARA AM handling, which (like its Thai
    // shaper) runs once the text is in the direction it shapes in.
    thai::preprocess(&mut codepoints, &mut glyphs, &mut mirrored_mask, level);
    if buffer_shaper.vowel_constraints() {
        let (cps, marks) = (&mut codepoints, &mut mirrored_mask);
        vowel_constraints::insert_dotted_circles(buffer_script, flags, cps, &mut glyphs, marks);
    }
    // HarfBuzz's Hangul shaper, which shapes a buffer whose script is
    // Hangul, composes and decomposes syllables and moves tone marks
    // at the same point (see `hangul`). The jamo features it gives stay
    // with the characters until the Hangul GSUB stage.
    let hangul_buffer = buffer_script == Some(Script::Hangul);
    let mut jamo: Option<(Vec<char>, Vec<u8>)> = None;
    if hangul_buffer {
        let has_glyph = |c: char| cmap.glyph_id(c).is_some();
        let zero_width = |c: char| {
            cmap.glyph_id(c)
                .is_some_and(|g| hmtx.advance(g).unwrap_or(0) == 0)
        };
        let font = hangul::HangulFont {
            has_glyph: &has_glyph,
            zero_width: &zero_width,
            dotted_circle: !flags.contains(BufferFlags::DO_NOT_INSERT_DOTTED_CIRCLE)
                && has_glyph('\u{25CC}'),
        };
        let features = hangul::preprocess(
            &mut codepoints,
            &mut glyphs,
            &mut mirrored_mask,
            &font,
            level,
        );
        jamo = Some((codepoints.clone(), features));
    }
    let mut segments = match buffer.script() {
        Some(script) => alloc::vec![Segment {
            cp_range: 0..codepoints.len(),
            script,
            script_priority: script_priority_for(script),
        }],
        None => build_segments(&codepoints),
    };

    let applies_morx = gsub.is_none() && face.table_bytes(crate::tables::tag::MORX).is_ok();
    let fallback_marks = position::fallback_mark_positioning(
        face,
        gpos.as_ref(),
        gsub.is_some(),
        applies_morx,
        buffer_shaper,
    )?;

    // HarfBuzz's `has_gpos_mark`, which its Hebrew shaper reads when it
    // composes: its feature map has `mark` unless the caller turned the
    // feature off or the language systems GSUB and GPOS pick both leave
    // it out. Lookups play no part, so a FeatureVariations record that
    // leaves `mark` none changes nothing.
    let mark_enabled = !super::feature_disabled(features, *b"mark");
    let has_gpos_mark = |priority: &[[u8; 4]]| {
        let mark = *b"mark";
        let in_gpos = || {
            gpos.as_ref()
                .is_some_and(|g| g.layout_view().lists(mark, priority))
        };
        let in_gsub = || {
            gsub.as_ref()
                .is_some_and(|g| g.layout_view().lists(mark, priority))
        };
        mark_enabled && (in_gpos() || in_gsub())
    };

    // Step 1.75: normalization, which also maps the characters to
    // glyphs. Each segment normalizes with the mode and hooks of the
    // shaper that shapes it (see `normalize` and
    // `Shaper::normalizer_for`), so its code points and glyphs stay one
    // to one.
    normalize::normalize_segments(
        &mut codepoints,
        &mut glyphs,
        &mut mirrored_mask,
        &mut segments,
        |seg| Normalizer {
            cmap: &cmap,
            shaper: Shaper::for_run(seg.script, !is_vertical, gsub.as_ref(), seg.script_priority)
                .normalizer_for(buffer_shaper),
            has_gpos_mark: has_gpos_mark(seg.script_priority),
            level,
            recategorize_marks: fallback_marks,
            not_found_variation_selector: buffer.not_found_variation_selector_glyph(),
        },
    );
    // The jamo features of the normalized text: those of the Hangul
    // preprocessing when normalization kept every character, as it
    // does unless the font lacks one with a decomposition, and
    // otherwise read off the characters.
    let jamo: Option<Vec<u8>> = jamo.map(|(before, features)| {
        if before == codepoints {
            features
        } else {
            hangul::jamo_features(&codepoints)
        }
    });

    // The glyph flags of cursive joining, which HarfBuzz sets while its
    // Arabic and Universal Shaping Engine shapers assign the joining
    // forms (`arabic_joining`), over the whole buffer.
    let flag_cx = glyph_flags::FlagCx::new(level, flags);
    let joins = match buffer_shaper {
        Shaper::Arabic => true,
        Shaper::Use => buffer_script.is_some_and(Script::has_arabic_joining),
        _ => false,
    };
    let (pre, post) = (buffer.pre_context(), buffer.post_context());
    if joins {
        arabic_joining::set_flags(&mut glyphs, &codepoints, pre, post, flag_cx);
    }

    // One work budget for every lookup this call applies directly,
    // across all segments (see `LookupBudget`). The `ot` pre-shapers
    // run each feature under a budget of its own.
    let mut budget = LookupBudget::for_shape(glyphs.len());

    // Dominant script: the first non-COMMON/INHERITED script in the
    // buffer. HarfBuzz (and rustybuzz) compute this once in
    // `guess_segment_properties` and use it to select a single shaper
    // for the whole run; features the shaper activates only fire when
    // the buffer's dominant script matches. sigilbuzz's per-segment
    // dispatch still runs each segment under its own script priority
    // (Hebrew half under `hebr`, Latin half under `latn`), but the
    // complex-shaper pre-pass for Old Hangul needs the dominant-script
    // gate to match HarfBuzz: a mixed `Hi 가` run hands `ljmo`/`vjmo`
    // the jamo segment under HarfBuzz's default shaper (no positional
    // variant forms picked), not the Hangul shaper. Gating the Jamo
    // pre-pass on dominant-script is the smallest knob that keeps
    // parity clean on pure Hangul runs while matching HarfBuzz on
    // Latin-majority mixed runs.
    let dominant_script = buffer_script;

    // Arabic joining forms are computed once, over the whole run,
    // because the state machine depends on surrounding letters (the
    // previous/next Arabic joining-type). A segment-local view would
    // lose the cross-boundary context, but in sigilbuzz every Arabic
    // segment is bounded by non-Arabic neighbors anyway, so global
    // computation is both correct and cheaper than recomputing per
    // segment. The buffer's pre- and post-context stand in for the
    // letters beyond the text's ends, as in HarfBuzz's arabic_joining.
    //
    // The forms index `codepoints`, not the text: the split-vowel
    // decompositions above make `codepoints` longer than the text,
    // and segments slice the forms by their `cp_range`.
    let arabic_shaped = |s: Script| matches!(s, Script::Arabic | Script::Syriac);
    let has_arabic = buffer.script().is_some_and(arabic_shaped)
        || codepoints.iter().any(|&c| arabic_shaped(script_of(c)));
    let arabic_actions = if has_arabic {
        arabic_joining::actions(&codepoints, pre, post)
    } else {
        Vec::new()
    };
    // Joining context for the Mongolian and N'Ko shapers, which
    // compute forms per segment: the segment's neighbors, then the
    // buffer context beyond the text.
    let joining_context = |range: &core::ops::Range<usize>| {
        JoiningContext::around(
            &codepoints,
            range.clone(),
            buffer.pre_context(),
            buffer.post_context(),
        )
    };

    // Step 2 (per segment): pre-shaper -> GSUB. We build the result by
    // concatenating per-segment processed glyph sub-vecs; each segment
    // processes its own slice of codepoints/glyphs so contextual
    // lookups in one script can never see the other script's glyphs
    // as context. Track the post-GSUB glyph range for each segment so
    // the downstream GPOS pass can dispatch under the same priority.
    let mut processed_glyphs: Vec<Glyph> = Vec::with_capacity(glyphs.len());
    let mut seg_glyph_ranges: Vec<ProcessedSegment> = Vec::with_capacity(segments.len());
    // Whether `stch` left stretch tiles for after positioning.
    let mut has_stch = false;

    for seg in &segments {
        // Take an owned sub-vec of this segment's glyphs so ligature
        // substitution can shrink or multiple-sub can grow the slice
        // without touching the rest of the run.
        // Segments come from `codepoints`, and glyphs are still one
        // per code point here, so both ranges exist. A range that does
        // not is skipped rather than trusted.
        let (Some(seg_glyphs_src), Some(seg_cps)) = (
            glyphs.get(seg.cp_range.clone()),
            codepoints.get(seg.cp_range.clone()),
        ) else {
            continue;
        };
        let mut seg_glyphs = seg_glyphs_src.to_vec();
        // The segment's shaper. The Indic, Myanmar and USE scripts take
        // the default shaper in a font without lookups of their own, as
        // in HarfBuzz, and the Tibetan and Mongolian runs of a buffer of
        // another script take that buffer's shaper.
        let seg_shaper =
            Shaper::for_run(seg.script, !is_vertical, gsub.as_ref(), seg.script_priority);
        let use_run = seg_shaper == Shaper::Use
            && (!matches!(seg.script, Script::Tibetan | Script::Mongolian)
                || dominant_script == Some(seg.script));
        // The Arabic shaper's joining forms (Arabic and Syriac). Vertical
        // Arabic takes the default shaper, as in HarfBuzz.
        let seg_arabic = seg_shaper == Shaper::Arabic && !arabic_actions.is_empty();
        if seg_arabic && gsub.is_some() {
            let actions = arabic_actions.get(seg.cp_range.clone()).unwrap_or_default();
            arabic_joining::stash(&mut seg_glyphs, actions);
        }
        let indic = crate::ot::indic::indic_config_for(seg.script)
            .filter(|c| c.script != Script::Sinhala && seg_shaper == Shaper::Indic);
        let myanmar = seg_shaper == Shaper::Myanmar;
        let khmer = seg.script == Script::Khmer;
        // A Hangul segment of a buffer the Hangul shaper shapes runs
        // HarfBuzz's Hangul GSUB stage, default features included, with
        // the jamo features of the preprocessing. In a buffer of
        // another script (`Hi \u{1100}\u{1161}`), HarfBuzz shapes the
        // jamo with that script's shaper, so they get no jamo features.
        let hangul_jamo = jamo
            .as_ref()
            .filter(|_| seg.script == Script::Hangul)
            .and_then(|j| j.get(seg.cp_range.clone()));
        // The shaper whose GSUB passes run for the segment, which stage
        // 0 reads to tell the tags a later pass applies.
        let segment_shaper = if indic.is_some() {
            SegmentShaper::Indic
        } else if use_run {
            SegmentShaper::Use
        } else if khmer {
            SegmentShaper::Khmer
        } else if myanmar {
            SegmentShaper::Myanmar
        } else if hangul_jamo.is_some() {
            SegmentShaper::Hangul
        } else if seg_arabic {
            SegmentShaper::Arabic
        } else {
            SegmentShaper::Default
        };
        let shaper_ran_defaults = !matches!(
            segment_shaper,
            SegmentShaper::Default | SegmentShaper::Arabic
        );
        // HarfBuzz's default, Hebrew and Thai shapers add no stage of
        // their own, so the direction features join the default ones.
        let plain_default = segment_shaper == SegmentShaper::Default;

        // GSUB stage 0 runs first: `rvrn`, and a required feature
        // whose tag no later pass applies, merged by lookup index as
        // HarfBuzz merges a stage. The direction features (`ltra` and
        // `ltrm`, or `rtla`, then `rtlm` on backward runs) follow in a
        // stage of their own, except for the default shaper, which runs
        // them with its default features.
        if let Some(ref gsub) = gsub {
            let plan = required::SegmentPlan {
                shaper: segment_shaper,
                vertical: is_vertical,
                backward,
                direction_features: rotate::direction_features(target_direction),
                features,
            };
            let priority = seg.script_priority;
            let gdef = gdef.as_ref();
            required::apply_stage_zero(gsub, &mut seg_glyphs, gdef, priority, &plan, &mut budget);
            let direction_tags = rotate::direction_features(target_direction);
            let table = JoinerTable::for_segment(seg_arabic);
            if !plain_default {
                apply_gsub_features_merged_budgeted(
                    gsub,
                    &mut seg_glyphs,
                    gdef,
                    features,
                    direction_tags,
                    priority,
                    table,
                    &mut budget,
                );
                if backward {
                    let mirrored = mirrored_mask.get(seg.cp_range.clone()).unwrap_or_default();
                    rotate::apply_rtlm(gsub, &mut seg_glyphs, gdef, priority, features, mirrored);
                }
            }
            if seg_arabic {
                has_stch |=
                    apply_stch(gsub, &mut seg_glyphs, gdef, features, priority, &mut budget);
            }
        }

        // Broken syllables get a dotted circle to sit on, which the
        // syllabic shapers insert after their syllable machines.
        let circle = cmap
            .glyph_id('\u{25CC}')
            .filter(|_| !flags.contains(BufferFlags::DO_NOT_INSERT_DOTTED_CIRCLE));

        // Per-script pre-shapers. Each is gated on the segment's
        // resolved script so a Hebrew segment never runs the Indic
        // state machine, and vice versa. The Indic, Khmer, Myanmar, and
        // USE shapers run every GSUB feature of their run, the default
        // ones in their last stage, as HarfBuzz's do, and insert their
        // own dotted circles.
        if let Some(config) = indic {
            let run = crate::ot::indic::shaper::IndicRun {
                gsub: gsub.as_ref(),
                gdef: gdef.as_ref(),
                level,
                features,
                vertical: is_vertical,
                dotted_circle: circle,
                virama_glyph: char::from_u32(config.virama).and_then(|v| cmap.glyph_id(v)),
            };
            crate::ot::indic::shaper::shape(&run, &config, seg_cps, &mut seg_glyphs);
        }
        if use_run {
            // The scripts with Arabic-style joining pick their
            // topographical features by joining form.
            let context = joining_context(&seg.cp_range);
            let joining: Option<Vec<JoiningForm>> = match seg.script {
                Script::Mongolian => Some(crate::ot::mongolian::assign_mongolian_forms_in_context(
                    seg_cps, context,
                )),
                s if s.has_arabic_joining() => {
                    let types: Vec<JoiningType> =
                        seg_cps.iter().map(|&c| joining_type(c)).collect();
                    Some(assign_from_types_in_context(&types, context))
                }
                _ => None,
            };
            let run = crate::ot::use_shaper::UseRun {
                gsub: gsub.as_ref(),
                gdef: gdef.as_ref(),
                script_priority: seg.script_priority,
                level,
                features,
                vertical: is_vertical,
                dotted_circle: circle,
                joining: joining.as_deref(),
            };
            crate::ot::use_shaper::shape(&run, seg_cps, &mut seg_glyphs);
        }
        if khmer {
            let run = crate::ot::khmer::KhmerRun {
                gsub: gsub.as_ref(),
                gdef: gdef.as_ref(),
                level,
                features,
                vertical: is_vertical,
                dotted_circle: circle,
            };
            crate::ot::khmer::shape(&run, seg_cps, &mut seg_glyphs);
        }
        if myanmar {
            let run = crate::ot::myanmar::MyanmarRun {
                gsub: gsub.as_ref(),
                gdef: gdef.as_ref(),
                level,
                features,
                vertical: is_vertical,
                dotted_circle: circle,
            };
            crate::ot::myanmar::shape(&run, seg_cps, &mut seg_glyphs);
        }
        // Thai and Lao need no pass of their own: HarfBuzz's Thai shaper
        // adds no features to the default ones, and its sara am
        // preprocessing ran with the other preprocessing above.

        if let Some(seg_jamo) = hangul_jamo {
            let run = crate::ot::hangul::HangulRun {
                gsub: gsub.as_ref(),
                gdef: gdef.as_ref(),
                features,
                vertical: is_vertical,
            };
            crate::ot::hangul::shape(&run, seg_cps, seg_jamo, &mut seg_glyphs);
        }

        if let Some(ref gsub) = gsub {
            // Arabic positional + default GSUB for this segment.
            let joiner_table = JoinerTable::for_segment(seg_arabic);
            if seg_arabic {
                // ccmp and locl must run before positional features so
                // any composition/decomposition and localized forms
                // have settled first (HarfBuzz's Arabic shaper puts
                // them in one stage ahead of isol/fina/medi/init).
                apply_gsub_features_merged_budgeted(
                    gsub,
                    &mut seg_glyphs,
                    gdef.as_ref(),
                    features,
                    &[*b"ccmp", *b"locl"],
                    seg.script_priority,
                    joiner_table,
                    &mut budget,
                );
                // The joining features read the actions stashed in the
                // glyphs (see `apply_arabic_positional_features`).
                apply_arabic_positional_features(
                    gsub,
                    &mut seg_glyphs,
                    gdef.as_ref(),
                    seg.script_priority,
                    &mut budget,
                );
                arabic_joining::clear_actions(&mut seg_glyphs);
            }
            if !shaper_ran_defaults {
                let rtlm: Option<Vec<bool>> = backward.then(|| {
                    let mirrored = mirrored_mask.get(seg.cp_range.clone()).unwrap_or_default();
                    mirrored.iter().map(|m| !m).collect()
                });
                let shaper = if seg_arabic {
                    DefaultShaper::Arabic
                } else {
                    DefaultShaper::Plain {
                        direction: rotate::direction_features(target_direction),
                        rtlm: rtlm.as_deref(),
                    }
                };
                let stages = DefaultGsub {
                    features,
                    vertical: is_vertical,
                    script_priority: seg.script_priority,
                    table: joiner_table,
                    shaper,
                };
                run_default_gsub(gsub, &mut seg_glyphs, gdef.as_ref(), &stages, &mut budget);
            }
        }

        let start = processed_glyphs.len();
        processed_glyphs.extend(seg_glyphs);
        let end = processed_glyphs.len();
        seg_glyph_ranges.push(ProcessedSegment {
            range: start..end,
            script_priority: seg.script_priority,
        });
    }

    // Reassemble: segments were concatenated in left-to-right order
    // so the buffer's visual ordering survives the round-trip.
    glyphs = processed_glyphs;

    // AAT fallback. Consulted only when the font has no GSUB at all
    // (that is how HarfBuzz decides between OpenType and AAT).
    // Legacy macOS Zapfino, older Apple Chancery variants, and most
    // third-party AAT-only fonts land here. Runs after the segmented
    // GSUB pass so a font that carries both only exercises the AAT
    // path when the OpenType side is absent.
    let mut applied_morx = false;
    if gsub.is_none() {
        if let Some(morx) = face.morx()? {
            let old_len = glyphs.len();
            if let Some(origins) = apply_morx(&morx, &mut glyphs) {
                // Ligatures and insertions change the glyph count, so
                // the segment ranges recorded above no longer line up.
                if glyphs.len() != old_len {
                    seg_glyph_ranges = remap_segments(&seg_glyph_ranges, &origins);
                }
            }
            applied_morx = true;
        }
    }

    // Step 3: advance lookup. Runs *after* GSUB so ligatures receive
    // their ligature-glyph advance, not the sum of component advances.
    // Horizontal layout pulls from hmtx and drives the pen along X;
    // vertical layout pulls from vmtx (when present) and drives the
    // pen along Y, while x_advance stays zero so the glyphs stack
    // rather than walk right. In the horizontal path, a non-empty
    // Font coord slice combined with an HVAR table adjusts each
    // advance by the per-coord delta.
    //
    // Default-ignorable glyphs get their font advance here like any
    // other; `ignorables::zero_width` zeroes it after positioning, as
    // HarfBuzz does, so a kerning pair that involves one cannot leave
    // it with an advance.
    //
    // One `FontAdvances` serves the whole call: the origins, the
    // fallback spaces, and the `stch` stretch below ask it too, and it
    // keeps each glyph's phantom-point advance once computed.
    let instance_cache = warm.then(|| font.caches().instance());
    let advances = position::FontAdvances::new(face, coords, is_vertical, instance_cache)?;
    if is_vertical {
        // VVAR carries per-glyph vertical-advance deltas; applies
        // only when the font is variable and the user requested
        // non-default coords. Without VVAR, a varied glyf font takes
        // the advance from the glyph's varied phantom points. With
        // no vmtx at all, every glyph advances by the ascender-to-
        // descender height, as in HarfBuzz.
        for glyph in &mut glyphs {
            // HarfBuzz convention: vertical y_advance is negative in
            // both TTB and BTT, so the pen moves downward; BTT only
            // differs by the final reversal.
            glyph.y_advance = advances.v_advance(glyph.glyph_id).saturating_neg();
            glyph.x_advance = 0;
        }
        // Offsets are relative to each glyph's horizontal origin.
        position::subtract_vertical_origins(&advances, &mut glyphs);
    } else {
        // Without HVAR, a varied glyf font takes the advance from the
        // glyph's varied phantom points.
        for glyph in &mut glyphs {
            glyph.x_advance = advances.h_advance(glyph.glyph_id);
        }
    }

    // Step 4: positioning. Mark-width zeroing, the GPOS stage per
    // segment (so each segment dispatches under its own script-tag
    // priority), the `kerx` / legacy `kern` fallbacks, and attachment
    // resolution, in HarfBuzz's order; see the `position` submodule.
    // Build the variable-font resolution context once. Passing this
    // through every GPOS apply site is what lets VariationIndex
    // deltas inside a ValueRecord actually respond to the user's
    // axis coords.
    let var = VarCtx {
        coords,
        store: gdef.as_ref().and_then(|g| g.item_variation_store()),
    };
    let inputs = position::Inputs {
        face,
        gdef: gdef.as_ref(),
        gpos: gpos.as_ref(),
        var: &var,
        direction,
        features,
        dominant_script,
        shaper: buffer_shaper,
        has_gsub: gsub.is_some(),
        applied_morx,
        zero_ignorables: ignorables::zeroes(flags),
        fallback_marks,
        flags: flag_cx,
        advances: &advances,
    };
    position::position(&inputs, &mut glyphs, &seg_glyph_ranges, &mut budget)?;

    // Backward directions shaped in logical order; hand them back in
    // visual order, as HarfBuzz does at the end of positioning.
    if !direction.is_forward() {
        glyphs.reverse();
    }

    // Unresolved variation selectors take the not-found glyph, if set.
    normalize::show_variation_selectors(&mut glyphs, buffer.not_found_variation_selector_glyph());

    // Then the ignorables become the invisible space glyph (or are
    // kept or removed, as the buffer flags ask).
    let space = cmap.glyph_id(' ').map(u32::from);
    ignorables::hide(&mut glyphs, space, flags, level);
    // The Arabic shaper's `postprocess_glyphs`: the `stch` stretch.
    if has_stch {
        let advance = |id: u32| advances.h_advance(id);
        let stretch = stch::Stretch {
            rtl: direction == Direction::Rtl,
            advance: &advance,
            text,
            level,
            max_len: typed.len().saturating_mul(64).max(16_384),
        };
        stch::apply_stch(&mut glyphs, &stretch);
    }
    glyph_flags::propagate(&mut glyphs, flags);

    Ok(ShapedRun { glyphs })
}

/// The parts `ch` is split into before normalization, each keeping its
/// cluster, or `None` for a character that is not split.
///
/// Thai SARA AM (U+0E33) and Lao AM (U+0EB3) become NIKHAHIT plus SARA
/// AA, as HarfBuzz's Thai shaper does before normalization: the font's
/// mark positioning targets the pair (see the `thai` module for the
/// rest of that step). Split vowels of the Indic, Khmer, and USE
/// scripts decompose in normalization, with those shapers' hooks.
fn split_before_cmap(ch: char) -> Option<&'static [char]> {
    match ch {
        '\u{0E33}' => Some(&['\u{0E4D}', '\u{0E32}']),
        '\u{0EB3}' => Some(&['\u{0ECD}', '\u{0EB2}']),
        _ => None,
    }
}
