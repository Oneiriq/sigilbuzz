//! The [`shape`] entry point: cmap lookup, script segmentation, the
//! per-segment pre-shaper and GSUB pass, advances, and positioning,
//! in that order.

use alloc::borrow::Cow;
use alloc::vec::Vec;

use super::aat::apply_morx;
use super::features::{
    apply_arabic_positional_features, apply_gsub_features_merged, early_default_features,
    run_default_gsub,
};
use super::hangul::hangul_compose;
use super::segment::{build_segments, is_common_for_segmentation, ProcessedSegment, Segment};
use super::{
    dotted_circle, feature_disabled, ignorables, native_direction, position, required, rotate,
    Feature, VarCtx,
};
use crate::buffer::{script_priority_for, Buffer, Direction, Glyph, ShapedRun};
use crate::error::Result;
use crate::font::Font;
use crate::ot::arabic::{assign_from_types_in_context, JoiningContext, JoiningForm};
use crate::unicode::joining::{joining_type, JoiningType};
use crate::unicode::{script_of, Script};

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
/// them is malformed.
// The pipeline is a straight-line sequence of passes so
// the order is visible in one place; breaking it into five stage
// helpers would cost more in indirection than it buys in LOC.
#[allow(clippy::too_many_lines)]
pub fn shape(font: &Font<'_>, buffer: &Buffer, features: &[Feature]) -> Result<ShapedRun> {
    let want_liga = !feature_disabled(features, *b"liga");
    // Vertical layout: explicit when the buffer direction is TTB/BTT,
    // *implicit* when the run is dominantly Mongolian and the caller
    // never chose a direction. Mongolian's traditional writing axis is
    // top-to-bottom; auto-vertical here lets simple callers shape
    // Mongolian without having to know the default. An explicit
    // direction always wins, so `set_direction(Direction::Ltr)` gives
    // horizontal Mongolian.
    let mongolian_dominant = buffer
        .text()
        .chars()
        .any(|c| crate::unicode::script_of(c) == crate::unicode::Script::Mongolian)
        && buffer
            .text()
            .chars()
            .find(|c| !matches!(crate::unicode::script_of(*c), crate::unicode::Script::Other))
            .is_some_and(|c| crate::unicode::script_of(c) == crate::unicode::Script::Mongolian);
    // The direction every later pass works with: the buffer's, or TTB
    // for implicit vertical Mongolian.
    let direction = if !buffer.has_explicit_direction() && mongolian_dominant {
        Direction::Ttb
    } else {
        buffer.direction()
    };
    let is_vertical = !direction.is_horizontal();

    let face = font.face();
    let cmap = face.cmap()?;
    let hmtx = face.hmtx()?;
    // Vertical metrics and origin overrides are optional; only look
    // them up when the caller has asked for vertical layout so
    // horizontal callers keep the cheap "hmtx only" path.
    let vmtx = if is_vertical { face.vmtx()? } else { None };

    let raw_text = buffer.text();
    if raw_text.is_empty() {
        return Ok(ShapedRun::default());
    }
    // NFC composition pass runs before cmap lookup so precomposed
    // forms (é, ñ, ...) find their precomposed glyphs instead of
    // the decomposed base + combining-mark pair. Opt-in; see
    // Buffer::set_normalize_nfc.
    let text: Cow<'_, str> = if buffer.normalize_nfc() {
        Cow::Owned(crate::unicode::normalize::compose_str(raw_text))
    } else {
        Cow::Borrowed(raw_text)
    };
    let text: &str = &text;

    // Step 1: codepoint -> glyph id via cmap. Clusters are byte
    // offsets from the start of the text so later passes can track
    // which input characters coalesce into a single output glyph.
    //
    // Default-ignorable characters (ZWJ, ZWNJ, bidi controls,
    // variation selectors, ...) map through cmap like any other, so
    // GSUB rules that name their glyphs still match; the passes in
    // the `ignorables` module hide them after positioning, as
    // HarfBuzz does.
    //
    // We also capture the raw `char` list alongside the glyphs so
    // the Indic shaper can consult Unicode properties per-codepoint
    // without re-scanning the UTF-8 stream.
    let mut glyphs: Vec<Glyph> = Vec::with_capacity(text.len());
    let mut codepoints: Vec<char> = Vec::with_capacity(text.len());
    // Preprocess Hangul Jamo NFC composition: L + V (+ optional T)
    // sequences collapse into the precomposed syllable in
    // U+AC00..U+D7A3 when the font carries a cmap entry for the
    // precomposed codepoint. The Jamo sub-blocks outside the modern
    // compositional range (Extended-A L, Extended-B T) suppress
    // composition so the font's `ljmo` / `vjmo` / `tjmo` features can
    // shape each jamo independently (matches HarfBuzz / rustybuzz).
    //
    // Returns `(byte_offset, char)` pairs; the byte offset is always
    // the first codepoint of the composed cluster, so cluster
    // tracking stays aligned with the original UTF-8 stream.
    let composed_chars: Vec<(u32, char)> = hangul_compose(text, &cmap);
    // Backward runs mirror paired punctuation (see `rotate`); these
    // are the indices in `codepoints` that were replaced.
    let backward = !direction.is_forward();
    let mut mirrored: Vec<usize> = Vec::new();
    for (cluster, ch) in composed_chars.iter().copied() {
        let cluster = cluster as usize;
        // Khmer split-vowel decomposition. HarfBuzz's USE
        // preprocessing hook splits U+17C4 / U+17C5 into a
        // pre-base component (sign-e) and a post-base component
        // (sign-aa / sign-au) so the syllable machine can see the
        // pre-base part directly. sigilbuzz does it at codepoint
        // push time, before the cmap lookup, so the rest of the
        // pipeline never sees the composed form.
        if matches!(ch, '\u{17C4}' | '\u{17C5}') {
            let (pre, post) = if ch == '\u{17C4}' {
                ('\u{17C1}', '\u{17B6}')
            } else {
                ('\u{17C1}', '\u{17B7}')
            };
            for &component in &[pre, post] {
                let gid = u32::from(cmap.glyph_id(component).unwrap_or(0));
                let glyph = Glyph::new(gid, cluster as u32);
                glyphs.push(glyph);
                codepoints.push(component);
            }
            continue;
        }
        // Thai sara am (U+0E33) and Lao lao am (U+0EB3). HarfBuzz
        // decomposes these composed vowels into
        // `nikkhahit / niggahita + sara aa` at buffer-prep time,
        // before shape enters the state machine: the font's
        // mark-positioning tables target the decomposed pair, not
        // the composed codepoint. We do the same here so the cmap
        // lookup lands on the two components and every downstream
        // pass (GSUB, GPOS, cluster merge) sees the decomposed form
        // rustybuzz does.
        if matches!(ch, '\u{0E33}' | '\u{0EB3}') {
            let (pre, post) = if ch == '\u{0E33}' {
                // Thai sara am -> nikkhahit (U+0E4D) + sara aa (U+0E32).
                ('\u{0E4D}', '\u{0E32}')
            } else {
                // Lao lao am -> niggahita (U+0ECD) + sara aa (U+0EB2).
                ('\u{0ECD}', '\u{0EB2}')
            };
            for &component in &[pre, post] {
                let gid = u32::from(cmap.glyph_id(component).unwrap_or(0));
                let glyph = Glyph::new(gid, cluster as u32);
                glyphs.push(glyph);
                codepoints.push(component);
            }
            continue;
        }
        // Tamil and Sinhala split-matra decomposition. These matras
        // decompose into a pre-base + post-base (occasionally
        // three-part) sequence. HarfBuzz's Indic shaper runs this
        // before syllable reordering so the pre-base half can be
        // picked up by the positional-category reorder. sigilbuzz
        // does it at codepoint push time (same entry point as Khmer)
        // so downstream passes never see the composed form.
        if let Some(parts) = crate::ot::indic::split_matra_decompose(ch) {
            for &component in parts {
                let gid = u32::from(cmap.glyph_id(component).unwrap_or(0));
                let glyph = Glyph::new(gid, cluster as u32);
                glyphs.push(glyph);
                codepoints.push(component);
            }
            // Script detection for the segmenter below runs off the
            // `codepoints` vec (not the original text), so pushing
            // the decomposed components is all we need. The components
            // keep their parent's script (Tamil / Sinhala) because
            // they come from the same Unicode block.
            continue;
        }
        let ch = if backward {
            let (m, replaced) = rotate::mirror(ch, &cmap);
            if replaced {
                mirrored.push(codepoints.len());
            }
            m
        } else {
            ch
        };
        let glyph_id = u32::from(cmap.glyph_id(ch).unwrap_or(0));
        // `unicode_props` is set once here and follows the glyph
        // through ligation, multiple substitution and final reorder.
        // The passes after positioning read the DEFAULT_IGNORABLE bit;
        // GSUB clears it on any glyph it substitutes
        // (`substitute_glyph`), matching HarfBuzz.
        let mut glyph = Glyph::new(glyph_id, cluster as u32);
        glyph.unicode_props = ignorables::unicode_props(ch);
        glyphs.push(glyph);
        codepoints.push(ch);
    }

    // Step 1.5: Segment the run into maximal same-script spans. Each
    // segment carries its own script priority (e.g. Arabic `arab` ->
    // DFLT, Hebrew `hebr` -> DFLT), its codepoint range in the
    // `codepoints` vec we just filled, and (after we finish GSUB
    // below) its post-substitution glyph range. Pre-GSUB the two
    // ranges coincide because cmap is 1:1 (Khmer's split-vowel
    // preprocessor above added both codepoints and glyphs in lockstep,
    // so the 1:1 invariant still holds here).
    //
    // Running each segment through its own cmap -> pre-shaper -> GSUB
    // -> GPOS chain is what lets mixed-script runs like `Hi שלום`
    // dispatch the Hebrew half under `hebr` features and the Latin
    // half under DFLT in a single call. The pre-segmenter implementation
    // resolved one global priority and missed script-specific lookups
    // on whichever half lost the tie-break.
    //
    // A caller-set script (Buffer::set_script) replaces the
    // segmentation: the whole buffer is one run under that script, the
    // way HarfBuzz shapes one buffer with one script.
    //
    // The buffer's script, as HarfBuzz guesses it: the caller's, or
    // that of the first script-bearing character in the text's order.
    let buffer_script: Option<Script> = buffer.script().or_else(|| {
        codepoints
            .iter()
            .copied()
            .find(|&c| !is_common_for_segmentation(c))
            .map(script_of)
    });
    // A caller-chosen direction that is not the script's native one
    // reads the text as already in that visual order: shape its
    // graphemes reversed, in the native direction (see
    // `native_direction`). Mirroring above followed the caller's.
    // HarfBuzz decides this for a buffer of one script; a buffer that
    // sigilbuzz splits into several script runs keeps the direction.
    let mut mirrored_mask = alloc::vec![false; codepoints.len()];
    for &i in &mirrored {
        mirrored_mask[i] = true;
    }
    let one_run = buffer.script().is_some() || build_segments(&codepoints).len() <= 1;
    let direction = if buffer.has_explicit_direction() && one_run {
        let native = native_direction::resolve(direction, buffer_script, &codepoints);
        if native != direction {
            native_direction::reverse_graphemes(&mut codepoints, &mut glyphs, &mut mirrored_mask);
        }
        native
    } else {
        direction
    };
    let segments = match buffer.script() {
        Some(script) => alloc::vec![Segment {
            cp_range: 0..codepoints.len(),
            script,
            script_priority: script_priority_for(script),
        }],
        None => build_segments(&codepoints),
    };

    // Dominant script: the first non-COMMON/INHERITED script in the
    // buffer. HarfBuzz (and rustybuzz) compute this once in
    // `guess_segment_properties` and use it to select a single shaper
    // for the whole run; features the shaper activates only fire when
    // the buffer's dominant script matches. sigilbuzz's per-segment
    // dispatch still runs each segment under its own script priority
    // (Hebrew half under `hebr`, Latin half under DFLT), but the
    // complex-shaper pre-pass for Old Hangul needs the dominant-script
    // gate to match HarfBuzz: a mixed `Hi 가` run hands `ljmo`/`vjmo`
    // the jamo segment under HarfBuzz's default shaper (no positional
    // variant forms picked), not the Hangul shaper. Gating the Jamo
    // pre-pass on dominant-script is the smallest knob that keeps
    // parity clean on pure Hangul runs while matching HarfBuzz on
    // Latin-majority mixed runs.
    let dominant_script = buffer_script;

    // The buffer language picks each script's language system for
    // every GSUB and GPOS feature lookup, including the ones the
    // complex shapers run (see `crate::ot::layout_select`).
    let language_tags: &[[u8; 4]] = buffer
        .language()
        .map_or(&[], crate::Language::ot_language_tags);
    let gsub = face.gsub()?.map(|g| g.with_language_tags(language_tags));
    // GDEF is consulted up-front so the LookupFlag skip-iterator has
    // it available for every GSUB context match. GPOS reuses the same
    // handle further down.
    let gdef = face.gdef()?;

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
    let has_arabic = buffer.script() == Some(Script::Arabic)
        || codepoints.iter().any(|&c| script_of(c) == Script::Arabic);
    let arabic_forms: Vec<JoiningForm> = if has_arabic {
        let context = JoiningContext::from_context(buffer.pre_context(), buffer.post_context());
        let types: Vec<JoiningType> = codepoints.iter().map(|&c| joining_type(c)).collect();
        assign_from_types_in_context(&types, context)
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

    for seg in &segments {
        // Take an owned sub-vec of this segment's glyphs so ligature
        // substitution can shrink or multiple-sub can grow the slice
        // without touching the rest of the run.
        let seg_glyphs_src = glyphs[seg.cp_range.clone()].to_vec();
        let seg_cps = &codepoints[seg.cp_range.clone()];
        let mut seg_glyphs = seg_glyphs_src;

        // A required feature whose tag no later pass applies runs
        // first, as HarfBuzz runs it in GSUB stage 0; `rtlm` follows
        // on backward runs.
        if let Some(ref gsub) = gsub {
            let plan = required::SegmentPlan {
                script: seg.script,
                dominant: dominant_script,
                codepoints: seg_cps,
                arabic: seg.script == Script::Arabic && !arabic_forms.is_empty(),
                vertical: is_vertical,
                backward,
                features,
            };
            let priority = seg.script_priority;
            required::apply_unscheduled(gsub, &mut seg_glyphs, gdef.as_ref(), priority, &plan);
            if backward {
                let mirrored = &mirrored_mask[seg.cp_range.clone()];
                let gdef = gdef.as_ref();
                rotate::apply_rtlm(gsub, &mut seg_glyphs, gdef, priority, features, mirrored);
            }
        }

        // Broken syllables get a dotted circle to sit on.
        let circled = cmap
            .glyph_id('\u{25CC}')
            .filter(|_| buffer.insert_dotted_circle())
            .and_then(|circle| dotted_circle::insert(seg.script, seg_cps, &mut seg_glyphs, circle));
        let seg_cps = circled.as_deref().unwrap_or(seg_cps);

        // Per-script pre-shapers. Each is gated on the segment's
        // resolved script so a Hebrew segment never runs the Indic
        // state machine, and vice versa.
        if let Some(config) = crate::ot::indic::indic_config_for(seg.script) {
            crate::ot::indic::shape_indic(
                gsub.as_ref(),
                gdef.as_ref(),
                seg_cps,
                &mut seg_glyphs,
                &config,
            );
        }
        if seg.script == Script::Khmer {
            crate::ot::use_shaper::shape_khmer(
                gsub.as_ref(),
                gdef.as_ref(),
                seg_cps,
                &mut seg_glyphs,
            );
        }
        if seg.script == Script::Tibetan && dominant_script == Some(Script::Tibetan) {
            crate::ot::tibetan::shape_tibetan(
                gsub.as_ref(),
                gdef.as_ref(),
                seg_cps,
                &mut seg_glyphs,
            );
        }
        if seg.script == Script::Mongolian && dominant_script == Some(Script::Mongolian) {
            crate::ot::mongolian::shape_mongolian_in_context(
                gsub.as_ref(),
                gdef.as_ref(),
                seg_cps,
                &mut seg_glyphs,
                joining_context(&seg.cp_range),
            );
        }
        if seg.script == Script::Myanmar {
            crate::ot::use_shaper::shape_myanmar(
                gsub.as_ref(),
                gdef.as_ref(),
                seg_cps,
                &mut seg_glyphs,
            );
        }
        if seg.script == Script::Thai {
            crate::ot::use_shaper::shape_thai(
                gsub.as_ref(),
                gdef.as_ref(),
                seg_cps,
                &mut seg_glyphs,
            );
        }
        if seg.script == Script::Lao {
            crate::ot::use_shaper::shape_lao(
                gsub.as_ref(),
                gdef.as_ref(),
                seg_cps,
                &mut seg_glyphs,
            );
        }
        if seg.script == Script::NKo {
            crate::ot::use_shaper::shape_nko_in_context(
                gsub.as_ref(),
                gdef.as_ref(),
                seg_cps,
                &mut seg_glyphs,
                joining_context(&seg.cp_range),
            );
        }
        if seg.script == Script::Buginese {
            crate::ot::use_shaper::shape_buginese(
                gsub.as_ref(),
                gdef.as_ref(),
                seg_cps,
                &mut seg_glyphs,
            );
        }
        if seg.script == Script::TaiTham {
            crate::ot::use_shaper::shape_tai_tham(
                gsub.as_ref(),
                gdef.as_ref(),
                seg_cps,
                &mut seg_glyphs,
            );
        }
        if seg.script == Script::Balinese {
            crate::ot::use_shaper::shape_balinese(
                gsub.as_ref(),
                gdef.as_ref(),
                seg_cps,
                &mut seg_glyphs,
            );
        }
        if seg.script == Script::Sundanese {
            crate::ot::use_shaper::shape_sundanese(
                gsub.as_ref(),
                gdef.as_ref(),
                seg_cps,
                &mut seg_glyphs,
            );
        }
        if seg.script == Script::Lepcha {
            crate::ot::use_shaper::shape_lepcha(
                gsub.as_ref(),
                gdef.as_ref(),
                seg_cps,
                &mut seg_glyphs,
            );
        }
        if seg.script == Script::Limbu {
            crate::ot::use_shaper::shape_limbu(
                gsub.as_ref(),
                gdef.as_ref(),
                seg_cps,
                &mut seg_glyphs,
            );
        }
        if seg.script == Script::Cham {
            crate::ot::use_shaper::shape_cham(
                gsub.as_ref(),
                gdef.as_ref(),
                seg_cps,
                &mut seg_glyphs,
            );
        }
        if seg.script == Script::Brahmi {
            crate::ot::use_shaper::shape_brahmi(
                gsub.as_ref(),
                gdef.as_ref(),
                seg_cps,
                &mut seg_glyphs,
            );
        }
        if seg.script == Script::Sharada {
            crate::ot::use_shaper::shape_sharada(
                gsub.as_ref(),
                gdef.as_ref(),
                seg_cps,
                &mut seg_glyphs,
            );
        }
        if seg.script == Script::Khojki {
            crate::ot::use_shaper::shape_khojki(
                gsub.as_ref(),
                gdef.as_ref(),
                seg_cps,
                &mut seg_glyphs,
            );
        }
        if seg.script == Script::Tirhuta {
            crate::ot::use_shaper::shape_tirhuta(
                gsub.as_ref(),
                gdef.as_ref(),
                seg_cps,
                &mut seg_glyphs,
            );
        }
        if seg.script == Script::Modi {
            crate::ot::use_shaper::shape_modi(
                gsub.as_ref(),
                gdef.as_ref(),
                seg_cps,
                &mut seg_glyphs,
            );
        }
        // Hangul routes through USE only for Jamo-decomposed text.
        // Precomposed syllables (U+AC00..U+D7A3) still pass through
        // the default GSUB/GPOS chain: `ljmo`/`vjmo`/`tjmo` are
        // no-ops on them, so running the pipeline is harmless but
        // wasteful. Additionally gate on the buffer's dominant
        // script: HarfBuzz picks one shaper for the whole run based
        // on the first non-COMMON script, so a Latin-majority mix
        // like `Hi \u{1100}\u{1161}` shapes the jamo under the
        // default shaper (no positional variant forms). sigilbuzz
        // matches that here so mixed runs round-trip glyph-for-glyph.
        if seg.script == Script::Hangul
            && dominant_script == Some(Script::Hangul)
            && seg_cps.iter().any(|&c| crate::unicode::is_hangul_jamo(c))
        {
            crate::ot::use_shaper::shape_hangul(
                gsub.as_ref(),
                gdef.as_ref(),
                seg_cps,
                &mut seg_glyphs,
            );
        }

        if let Some(ref gsub) = gsub {
            // Arabic positional + default GSUB for this segment.
            let seg_arabic_active = seg.script == Script::Arabic && !arabic_forms.is_empty();
            if seg_arabic_active {
                // ccmp and locl must run before positional features so
                // any composition/decomposition and localized forms
                // have settled first (HarfBuzz's Arabic shaper puts
                // them in one stage ahead of isol/fina/medi/init).
                apply_gsub_features_merged(
                    gsub,
                    &mut seg_glyphs,
                    gdef.as_ref(),
                    features,
                    &[*b"ccmp", *b"locl"],
                    seg.script_priority,
                );
                // Arabic positional pass consumes only the segment's
                // slice of the forms vector: cps/glyphs are 1:1 at
                // this point (ccmp can rewrite ids but not lengths in
                // practice for Arabic), so the slice aligns.
                let forms_slice = arabic_forms.get(seg.cp_range.clone()).unwrap_or(&[]);
                apply_arabic_positional_features(gsub, &mut seg_glyphs, gdef.as_ref(), forms_slice);
            }
            run_default_gsub(
                gsub,
                &mut seg_glyphs,
                gdef.as_ref(),
                features,
                want_liga,
                is_vertical,
                seg.script_priority,
                early_default_features(seg_arabic_active, seg.script, dominant_script),
            );
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
    // (that is how HarfBuzz decides between OpenType and AAT), and
    // matches the issue scope. Legacy macOS Zapfino, older Apple
    // Chancery variants, and most third-party AAT-only fonts land
    // here. Runs after the segmented GSUB pass so a font that
    // carries both only exercises the AAT path when the OpenType
    // side is absent.
    let mut applied_morx = false;
    if gsub.is_none() {
        if let Some(morx) = face.morx()? {
            apply_morx(&morx, &mut glyphs);
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
    if is_vertical {
        if let Some(ref vmtx) = vmtx {
            // VVAR carries per-glyph vertical-advance deltas;
            // applies only when the font is variable and the user
            // requested non-default coords. Same pattern as the
            // horizontal branch's HVAR usage below: we resolve
            // once and consult per-glyph inside the loop.
            let coords = font.coords();
            let vvar = if coords.is_empty() {
                None
            } else {
                face.vvar()?
            };
            for glyph in &mut glyphs {
                let id = glyph.glyph_id as u16;
                // HarfBuzz convention: vertical y_advance is negative
                // in both TTB and BTT, so the pen moves downward; BTT
                // only differs by the final reversal.
                let mut raw = i32::from(vmtx.advance(id).unwrap_or(0));
                if let Some(ref vvar) = vvar {
                    let delta = vvar.advance_height_delta(id, coords);
                    let rounded = if delta >= 0.0 {
                        (delta + 0.5) as i32
                    } else {
                        (delta - 0.5) as i32
                    };
                    raw = raw.saturating_add(rounded);
                }
                glyph.y_advance = -raw;
                glyph.x_advance = 0;
            }
        } else {
            // No vmtx: fall back to an em-square advance so the run
            // still stacks deterministically. Use the hhea-reported
            // line height as a reasonable default.
            let hhea = face.hhea()?;
            let fallback = (hhea.ascent as i32) - (hhea.descent as i32);
            for glyph in &mut glyphs {
                glyph.y_advance = -fallback;
                glyph.x_advance = 0;
            }
        }
        // Offsets are relative to each glyph's horizontal origin.
        position::subtract_vertical_origins(face, font.coords(), &mut glyphs)?;
    } else {
        let coords = font.coords();
        let hvar = if coords.is_empty() {
            None
        } else {
            face.hvar()?
        };
        for glyph in &mut glyphs {
            let id = glyph.glyph_id as u16;
            let base = i32::from(hmtx.advance(id).unwrap_or(0));
            glyph.x_advance = if let Some(ref hvar) = hvar {
                let delta = hvar.advance_delta(id, coords);
                // Round-to-nearest without pulling in libm: the
                // delta arithmetic is small, so add-0.5 / subtract-0.5
                // suffices.
                let rounded = if delta >= 0.0 {
                    (delta + 0.5) as i32
                } else {
                    (delta - 0.5) as i32
                };
                base.saturating_add(rounded)
            } else {
                base
            };
        }
    }

    // Step 4: positioning. Mark-width zeroing, the GPOS stage per
    // segment (so each segment dispatches under its own script-tag
    // priority), the `kerx` / legacy `kern` fallbacks, and attachment
    // resolution, in HarfBuzz's order; see the `position` submodule.
    let gpos = face.gpos()?.map(|g| g.with_language_tags(language_tags));
    // Build the variable-font resolution context once. Passing this
    // through every GPOS apply site is what lets VariationIndex
    // deltas inside a ValueRecord actually respond to the user's
    // axis coords.
    let var = VarCtx {
        coords: font.coords(),
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
        has_gsub: gsub.is_some(),
        applied_morx,
    };
    position::position(&inputs, &mut glyphs, &seg_glyph_ranges)?;

    // Backward directions shaped in logical order; hand them back in
    // visual order, as HarfBuzz does at the end of positioning.
    if !direction.is_forward() {
        glyphs.reverse();
    }

    // Then the ignorables become the invisible space glyph.
    ignorables::hide(&mut glyphs, cmap.glyph_id(' ').map(u32::from));

    Ok(ShapedRun { glyphs })
}
