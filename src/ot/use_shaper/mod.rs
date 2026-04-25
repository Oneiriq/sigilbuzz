//! Universal Shaping Engine (USE).
//!
//! The USE is Microsoft's generic complex-script shaper — the one
//! every SE-Asian, SE-Indic and archaic-South-Asian script that does
//! not fit Arabic or Indic2 runs through. Khmer, Myanmar, Tai Tham,
//! Buginese, New Tai Lue, Cham, Old Hangul, Hanifi Rohingya are all
//! USE clients. 0.2.0 wires up Khmer as the pilot; the other scripts
//! add incrementally by extending the per-codepoint tables in
//! [`crate::unicode::use_category`] and registering their script tag
//! in this module's script priority table.
//!
//! # Pipeline
//!
//! 1. **Categorise** every codepoint in the run via
//!    [`use_category`](crate::unicode::use_category::use_category)
//!    and [`use_position`](crate::unicode::use_category::use_position).
//! 2. **Segment** into USE syllables. The grammar (simplified to the
//!    shape Khmer actually emits) is:
//!
//!    ```text
//!      R? (B | GB | IV) (H B)* VPre* VAbv* VBlw* VPst* M* FM*
//!    ```
//!
//!    Non-matching codepoints emit a one-wide Symbol/Broken syllable
//!    so the segmenter always makes progress.
//! 3. **Reorder** each syllable in place:
//!    - Move every pre-base vowel sign (VPre) to sit immediately
//!      before the base.
//!    - Promote a leading Repha (R) to the USE reph slot — a no-op
//!      for Khmer which has no repha, but wired so Myanmar's
//!      kinzi slots straight in.
//! 4. **Basic features** — per-syllable, applied via the GSUB
//!    dispatcher in the script-tag order `khmr`/`khm2` → DFLT. Order:
//!
//!    ```text
//!      locl → ccmp → rphf → pref → rkrf → abvf → blwf → half
//!           → pstf → vatu → cjct → isol
//!    ```
//!
//! 5. **Topographical features** — run after basic substitutions:
//!
//!    ```text
//!      abvs → blws → haln → pres → psts
//!    ```
//!
//! 6. **GPOS** — the generic pipeline in [`crate::shape`] runs the
//!    standard kern/mark/mkmk plus the Khmer `dist` feature. This
//!    module returns control to it after topographical GSUB.
//!
//! # Cluster integrity
//!
//! Every reorder preserves cluster byte offsets — pre-base matra
//! movement copies the source glyph (cluster and all), shifts the
//! intervening glyphs right by one, and drops the matra in. The
//! generic GSUB dispatcher already merges clusters when a ligature
//! collapses components, so the surviving glyph carries the minimum
//! byte offset of its source run.

use alloc::vec::Vec;

use crate::buffer::Glyph;
use crate::shape::apply_gsub_feature_in_scripts;
use crate::tables::gdef::Gdef;
use crate::tables::Gsub;
use crate::unicode::use_category::{use_category, use_position, UseCategory, UsePosition};

/// Script-tag priority for USE GSUB / GPOS feature lookup.
///
/// Khmer fonts advertise their USE features under `khmr` (Indic2
/// tag) and the legacy `khmr` form is identical; HarfBuzz also
/// accepts `khm2` on fonts built against the 2005+ Indic2 revision.
/// DFLT falls through for fonts that register features only in the
/// default LangSys (rare for Khmer but cheap to probe).
pub const KHMER_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"khmr", *b"khm2", *b"DFLT"];

/// Myanmar script-tag priority — `mym2` is the Indic2 (2012+) tag
/// that modern Noto / Padauk builds use; `mymr` is the legacy tag
/// that older fonts still carry.
pub const MYANMAR_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"mym2", *b"mymr", *b"DFLT"];

/// Thai script-tag priority.
pub const THAI_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"thai", *b"DFLT"];

/// Lao script-tag priority. The OpenType tag is `lao ` with a
/// trailing space — the 4-byte tag convention is padded that way.
pub const LAO_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"lao ", *b"DFLT"];

/// Hangul script-tag priority. Old Hangul fonts register their
/// `ljmo`/`vjmo`/`tjmo` features under `hang`; `jamo` is the legacy
/// tag that a few fonts still emit.
pub const HANGUL_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"hang", *b"jamo", *b"DFLT"];

/// N'Ko script tag — `nko ` (trailing space) is the canonical
/// OpenType tag for N'Ko. No v2 form.
pub const NKO_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"nko ", *b"DFLT"];

/// Buginese (Lontara) script tag.
pub const BUGINESE_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"bugi", *b"DFLT"];

/// Tai Tham (Lanna) script tag.
pub const TAI_THAM_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"lana", *b"DFLT"];

/// Balinese script tag — `bali` is the only OT tag in current use.
pub const BALINESE_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"bali", *b"DFLT"];

/// Sundanese script tag.
pub const SUNDANESE_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"sund", *b"DFLT"];

/// Lepcha script tag.
pub const LEPCHA_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"lepc", *b"DFLT"];

/// Limbu script tag.
pub const LIMBU_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"limb", *b"DFLT"];

/// Cham script tag.
pub const CHAM_SCRIPT_PRIORITY: &[[u8; 4]] = &[*b"cham", *b"DFLT"];

/// USE basic features, applied per-syllable before reordering
/// finalisation. Order matters — `rphf` must run before `half` so
/// the ra+halant that would otherwise fold into a half-form is
/// consumed as a reph first.
pub const USE_BASIC_FEATURES: &[&[u8; 4]] = &[
    b"locl", b"ccmp", b"rphf", b"pref", b"rkrf", b"abvf", b"blwf", b"half", b"pstf", b"vatu",
    b"cjct", b"isol",
];

/// USE topographical features — run after basic substitutions have
/// collapsed conjuncts into display forms.
pub const USE_TOPOGRAPHICAL_FEATURES: &[&[u8; 4]] = &[b"abvs", b"blws", b"haln", b"pres", b"psts"];

/// Myanmar's USE basic features. Adds `rphf` + `pref` + `blwf` +
/// `pstf` + `cjct` for kinzi and medial consonant handling. The
/// order mirrors the MS Myanmar shaping-model doc; `locl`/`ccmp`
/// open the chain so contextual fixups settle before positional
/// substitutions.
pub const MYANMAR_BASIC_FEATURES: &[&[u8; 4]] = &[
    b"locl", b"ccmp", b"rphf", b"pref", b"blwf", b"pstf", b"abvf", b"cjct",
];

/// Myanmar's USE topographical features — display-form selection
/// after the basic subs collapse conjuncts.
pub const MYANMAR_TOPOGRAPHICAL_FEATURES: &[&[u8; 4]] =
    &[b"abvs", b"blws", b"haln", b"pres", b"psts", b"calt"];

/// Thai / Lao's feature set — no halant, no subjoining, so the
/// shaper just needs contextual shaping + mark positioning. `liga`
/// and `calt` handle most tone-mark placement adjustments.
pub const THAI_LAO_FEATURES: &[&[u8; 4]] = &[b"ccmp", b"liga", b"calt"];

/// Hangul Old-Hangul features — the three positional jamo features
/// pick Leading/Vowel/Trailing variant shapes.
pub const HANGUL_FEATURES: &[&[u8; 4]] = &[b"ccmp", b"ljmo", b"vjmo", b"tjmo", b"calt"];

/// Classification of one USE syllable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SyllableKind {
    /// Consonant-based syllable — the common case.
    Consonant,
    /// Vowel syllable — starts with an independent vowel (IV).
    Vowel,
    /// A single symbol / number / generic-base pass-through. The
    /// state machine should not reorder these.
    Symbol,
    /// A broken syllable — codepoint we could not fit into any
    /// grammar production. Emitted as a one-wide unit so the
    /// segmenter always advances.
    Broken,
}

/// One USE syllable's footprint in the codepoint run.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Syllable {
    pub kind: SyllableKind,
    /// Start codepoint/glyph index (inclusive). Kept for debugging
    /// and test assertions even when the reorder pass only needs
    /// `end` and `base_index` — tagging a syllable by its left edge
    /// is the cheapest way to cross-reference against the original
    /// codepoint slice.
    #[allow(dead_code)]
    pub start: usize,
    /// End codepoint/glyph index (exclusive).
    pub end: usize,
    /// Codepoint-space index of the base consonant inside the
    /// syllable, or `None` for non-consonant syllables.
    pub base_index: Option<usize>,
    /// Codepoint-space index of a pre-base consonant pair
    /// (`coeng + ra` in Khmer), or `None`. When present, the two
    /// glyphs at `pre_base_cons_index` and `pre_base_cons_index + 1`
    /// get moved to before the base after GSUB has had a chance to
    /// collapse them into a single subscript form.
    pub pre_base_cons_index: Option<usize>,
    /// Codepoint-space index of a Myanmar kinzi prefix — the triple
    /// `Nga (U+1004) + Asat (U+103A) + Virama (U+1039)` at the start
    /// of a consonant syllable. When present, those three glyphs move
    /// to immediately after the base before the rphf feature fires,
    /// so the collapsed kinzi glyph sits in the reph slot (after
    /// the base consonant in logical order). Matches rustybuzz's
    /// `initial_reordering_consonant_syllable` POS_AFTER_MAIN path.
    pub kinzi_index: Option<usize>,
}

/// Entry point — shapes one Khmer run. `codepoints` is in
/// one-to-one correspondence with `glyphs` on entry; after the call
/// `glyphs` may be shorter (GSUB collapses) and reordered. Clusters
/// track back to original byte offsets so the caller can map glyphs
/// to input.
pub fn shape_khmer(
    gsub: Option<&Gsub<'_>>,
    gdef: Option<&Gdef<'_>>,
    codepoints: &[char],
    glyphs: &mut Vec<Glyph>,
) {
    if codepoints.is_empty() || glyphs.is_empty() {
        return;
    }

    // 1. Segment. One pass over the codepoints, emitting Syllable
    //    records that the reorder pass can consume directly.
    let syllables = segment_syllables(codepoints);

    // 2. Initial reordering — pre-base vowel signs move before the
    //    base. Done BEFORE GSUB so features see the logical order
    //    fonts expect. Reordering is length-preserving, so glyph
    //    indices stay aligned with codepoints across this pass.
    for syllable in &syllables {
        initial_reorder(codepoints, glyphs, syllable);
    }

    // 3. Basic features. The generic dispatcher in `shape.rs`
    //    handles script-tag fallback; we just hand it the tags in
    //    the USE-mandated order.
    if let Some(gsub) = gsub {
        for tag in USE_BASIC_FEATURES {
            apply_gsub_feature_in_scripts(gsub, glyphs, gdef, **tag, 0, KHMER_SCRIPT_PRIORITY);
        }
    }

    // 4. Topographical features — after basic, to pick display
    //    forms for the collapsed conjuncts.
    if let Some(gsub) = gsub {
        for tag in USE_TOPOGRAPHICAL_FEATURES {
            apply_gsub_feature_in_scripts(gsub, glyphs, gdef, **tag, 0, KHMER_SCRIPT_PRIORITY);
        }
    }

    // 5. Cluster merge. Every glyph belonging to a syllable gets
    //    its cluster rewritten to the byte offset of the syllable's
    //    first codepoint — matches HarfBuzz / rustybuzz so the
    //    parity tests see identical cluster ids even after GSUB
    //    has collapsed parts of the syllable.
    let byte_offsets = cluster_byte_offsets(codepoints);
    merge_syllable_clusters(glyphs, &syllables, &byte_offsets);

    // Final GPOS (kern, mark, mkmk, dist) runs in the caller — see
    // shape.rs. That lets the generic mark-attachment machinery
    // handle Khmer's tone marks without a script-specific branch.
}

/// Splits the codepoint run into USE syllables using a simple greedy
/// parser over [`UseCategory`]. Each iteration either matches one
/// of the known grammar productions or emits a one-wide Broken
/// syllable so the outer loop terminates.
pub(crate) fn segment_syllables(codepoints: &[char]) -> Vec<Syllable> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < codepoints.len() {
        let syl = scan_one_syllable(codepoints, i);
        i = syl.end;
        out.push(syl);
    }
    out
}

/// Parses a single syllable starting at `start`. Always makes
/// progress: `end > start` on return.
fn scan_one_syllable(cps: &[char], start: usize) -> Syllable {
    let len = cps.len();
    let first = use_category(cps[start]);

    match first {
        UseCategory::B => scan_consonant_syllable(cps, start),
        UseCategory::IV => scan_vowel_syllable(cps, start),
        UseCategory::GB | UseCategory::N | UseCategory::S => {
            // One-wide Symbol syllable. Runs of digits or generic
            // bases are kept as separate syllables so each keeps
            // its own cluster id after the merge pass — matches
            // rustybuzz, where e.g. the three Khmer digits ០១២
            // emit clusters 0/3/6 rather than a single merged 0.
            let _ = len;
            Syllable {
                kind: SyllableKind::Symbol,
                start,
                end: start + 1,
                base_index: None,
                pre_base_cons_index: None,
                kinzi_index: None,
            }
        }
        UseCategory::R => {
            // Repha prefix — followed by a consonant syllable. The
            // Myanmar kinzi case is handled inline in
            // `scan_consonant_syllable` because kinzi's codepoints
            // (Nga / Asat / Virama) are categorized as B/H/H, not R;
            // this arm stays for potential future R-category repha
            // in other USE scripts.
            let syl = scan_consonant_syllable(cps, start + 1);
            Syllable {
                kind: syl.kind,
                start,
                end: syl.end,
                base_index: syl.base_index,
                pre_base_cons_index: syl.pre_base_cons_index,
                kinzi_index: syl.kinzi_index,
            }
        }
        UseCategory::ZWJ | UseCategory::ZWNJ | UseCategory::WS | UseCategory::O => Syllable {
            kind: SyllableKind::Symbol,
            start,
            end: start + 1,
            base_index: None,
            pre_base_cons_index: None,
            kinzi_index: None,
        },
        _ => Syllable {
            kind: SyllableKind::Broken,
            start,
            end: start + 1,
            base_index: None,
            pre_base_cons_index: None,
            kinzi_index: None,
        },
    }
}

/// Matches a consonant syllable:
///
/// ```text
///   B (H B)* (VPre | VAbv | VBlw | VPst)* M* FM* VS?
/// ```
fn scan_consonant_syllable(cps: &[char], start: usize) -> Syllable {
    let len = cps.len();
    let mut i = start;

    // Myanmar kinzi prefix: `Nga (U+1004) + Asat (U+103A) + Virama
    // (U+1039)` at the syllable head. Consume the triple up front
    // and remember its position so `initial_reorder` can move it
    // to POS_AFTER_MAIN once we know the base consonant index.
    // Matches the first branch of rustybuzz's
    // `initial_reordering_consonant_syllable` (the `Ra + As + H`
    // check). Leaves the outer grammar intact — after the kinzi
    // triple we still require a leading base consonant.
    let kinzi_index: Option<usize> = if i + 3 <= len
        && cps[i] == '\u{1004}'
        && cps[i + 1] == '\u{103A}'
        && cps[i + 2] == '\u{1039}'
        && i + 3 < len
        && matches!(use_category(cps[i + 3]), UseCategory::B | UseCategory::GB)
    {
        let kz = i;
        i += 3;
        Some(kz)
    } else {
        None
    };

    // Required leading base.
    let mut base_index: Option<usize> =
        if i < len && matches!(use_category(cps[i]), UseCategory::B | UseCategory::GB) {
            i += 1;
            Some(i - 1)
        } else {
            // Degenerate case — caller routed us here with a non-B
            // first codepoint. Emit Broken so the outer loop advances.
            return Syllable {
                kind: SyllableKind::Broken,
                start,
                end: start + 1,
                base_index: None,
                pre_base_cons_index: None,
                kinzi_index: None,
            };
        };

    // Zero or more halant-consonant pairs (Khmer coeng stacks). The
    // last base wins — it is the visible consonant; earlier bases
    // become subscripts via the `blwf`/`pstf` GSUB features.
    //
    // Exception: Khmer `coeng + ra` (U+17D2 + U+179A) is a pre-base
    // subscript. When we hit it, remember the pair's index but
    // keep the previous consonant as the visible base — so the
    // post-GSUB reorder can move the subscript-ra glyph in front
    // of the base. This is the Khmer-specific `pref` positioning
    // rule that USE bakes in for every script with pre-base
    // subscripts (Myanmar has similar behaviour for medial ra).
    let mut pre_base_cons_index: Option<usize> = None;
    while i + 1 < len
        && use_category(cps[i]) == UseCategory::H
        && matches!(use_category(cps[i + 1]), UseCategory::B | UseCategory::GB)
    {
        let halant_idx = i;
        let cons_idx = i + 1;
        let is_khmer_coeng_ra = cps[halant_idx] == '\u{17D2}' && cps[cons_idx] == '\u{179A}';
        i += 2;
        if is_khmer_coeng_ra && pre_base_cons_index.is_none() {
            // Pre-base subscript. Do NOT update base_index — the
            // visible base stays the consonant before the coeng.
            pre_base_cons_index = Some(halant_idx);
        } else {
            base_index = Some(cons_idx);
        }
    }

    // Trailing vowel signs and marks. Order the grammar is lenient
    // about — we accept any interleaving of V* / M* / FM* because
    // the reorder pass handles positions explicitly.
    while i < len {
        match use_category(cps[i]) {
            UseCategory::VPre
            | UseCategory::VAbv
            | UseCategory::VBlw
            | UseCategory::VPst
            | UseCategory::M
            | UseCategory::FM
            | UseCategory::CM
            | UseCategory::VS
            | UseCategory::ZWJ
            | UseCategory::ZWNJ => {
                i += 1;
            }
            // A trailing halant without a following base ends the
            // syllable (Khmer viriam usage). Consume and stop.
            UseCategory::H => {
                i += 1;
                break;
            }
            _ => break,
        }
    }

    Syllable {
        kind: SyllableKind::Consonant,
        start,
        end: i,
        base_index,
        pre_base_cons_index,
        kinzi_index,
    }
}

/// Matches a vowel-led syllable — independent vowel + optional
/// trailing marks.
fn scan_vowel_syllable(cps: &[char], start: usize) -> Syllable {
    let len = cps.len();
    let mut i = start + 1;
    while i < len {
        match use_category(cps[i]) {
            UseCategory::VAbv
            | UseCategory::VBlw
            | UseCategory::VPst
            | UseCategory::M
            | UseCategory::FM
            | UseCategory::CM
            | UseCategory::VS => {
                i += 1;
            }
            _ => break,
        }
    }
    Syllable {
        kind: SyllableKind::Vowel,
        start,
        end: i,
        base_index: Some(start),
        pre_base_cons_index: None,
        kinzi_index: None,
    }
}

/// Initial reorder for one syllable. Moves every pre-base vowel sign
/// in the syllable to sit immediately before the base consonant, and
/// promotes pre-base consonant pairs (Khmer `coeng + ra`) to the
/// syllable head so the `pref` GSUB feature sees them adjacent AND
/// their output glyph naturally sits before the base.
/// Length-preserving: glyph count and codepoint count stay aligned.
fn initial_reorder(codepoints: &[char], glyphs: &mut [Glyph], syllable: &Syllable) {
    if !matches!(syllable.kind, SyllableKind::Consonant) {
        return;
    }
    let Some(base) = syllable.base_index else {
        return;
    };
    if syllable.end > glyphs.len() || syllable.end > codepoints.len() {
        return;
    }

    // Gather pre-base matra indices that live after the base — those
    // are the ones that need to move. A pre-base sign sitting before
    // the base is already in position (unusual but legal for
    // broken-cluster repair).
    let mut to_move: Vec<usize> = Vec::new();
    for (offset, &ch) in codepoints[base + 1..syllable.end].iter().enumerate() {
        if matches!(use_category(ch), UseCategory::VPre) || use_position(ch) == UsePosition::PreBase
        {
            to_move.push(base + 1 + offset);
        }
    }

    // Pre-base consonant pair indices (Khmer `coeng + ra` = the two
    // codepoints at `pre_base_cons_index` and that + 1). These
    // move to the start of the syllable, BEFORE the pre-base
    // matras — so the visual order ends up as
    // `[matras, pre-base cons pair, everything else, base, ...]`.
    let pre_cons_idx = syllable.pre_base_cons_index;

    // Myanmar kinzi prefix — three codepoints at `kinzi_index`,
    // `kinzi_index + 1`, `kinzi_index + 2` (Nga + Asat + Virama).
    // rustybuzz's Myanmar reorder tags them POS_AFTER_MAIN so the
    // sort drops them after the base consonant; sigilbuzz replicates
    // the resulting glyph order here. After reorder, `rphf` fires on
    // the still-adjacent triple and collapses it to the font's kinzi
    // glyph, which naturally sits in the reph slot (immediately after
    // the base consonant).
    let kinzi_idx = syllable.kinzi_index;

    if to_move.is_empty() && pre_cons_idx.is_none() && kinzi_idx.is_none() {
        return;
    }

    // Rebuild the syllable slice in one pass so we handle the
    // multi-matra and coeng-stack cases without index drift.
    //
    // Target layout (USE pre-base rule per MS USE spec):
    //
    //   [pre-base matras in logical order]
    //   [everything else, in original order]
    //
    // Pre-base matras move to the very start of the syllable — not
    // just before the base. This keeps coeng stacks intact so GSUB
    // `blwf` / `pstf` can still see `halant + consonant` pairs
    // adjacent and collapse them into a single subscript glyph.
    //
    // For `sa + coeng + ta + sign-e` the result is
    // `[sign-e, sa, coeng, ta]`. The subsequent `blwf` pass sees
    // `coeng + ta` still adjacent and collapses to a single
    // subscript-ta glyph, matching rustybuzz.
    //
    // `base` is used below as the anchor for Myanmar kinzi
    // placement: the kinzi triple gets injected immediately after
    // the base consonant in the rebuilt slice, matching rustybuzz's
    // POS_AFTER_MAIN semantics.
    let syl_start = syllable.start;
    let syl_end = syllable.end;
    let original: Vec<Glyph> = glyphs[syl_start..syl_end].to_vec();
    let mut rebuilt: Vec<Glyph> = Vec::with_capacity(syl_end - syl_start);

    // Set of indices whose glyphs are consumed by the earlier
    // buckets and must not be re-emitted by the fall-through.
    let mut consumed: Vec<usize> = Vec::new();

    // 1. Pre-base matras, in logical order.
    for &idx in &to_move {
        rebuilt.push(original[idx - syl_start]);
        consumed.push(idx);
    }
    // 2. Pre-base consonant pair (coeng + ra). Both glyphs move to
    //    the start of the syllable so the `pref` GSUB feature sees
    //    the pair adjacent AND the collapsed subscript-ra glyph
    //    already sits before the base.
    if let Some(pc) = pre_cons_idx {
        if pc >= syl_start && pc + 1 < syl_end {
            rebuilt.push(original[pc - syl_start]);
            rebuilt.push(original[pc + 1 - syl_start]);
            consumed.push(pc);
            consumed.push(pc + 1);
        }
    }
    // 3. Mark the kinzi triple as consumed so the fall-through
    //    doesn't re-emit them at the syllable head; we inject them
    //    right after the base consonant below.
    if let Some(kz) = kinzi_idx {
        if kz + 2 < syl_end {
            consumed.push(kz);
            consumed.push(kz + 1);
            consumed.push(kz + 2);
        }
    }
    // 4. Everything else, in original order — with the kinzi triple
    //    injected immediately after the base consonant.
    for idx in syl_start..syl_end {
        if consumed.contains(&idx) {
            continue;
        }
        rebuilt.push(original[idx - syl_start]);
        if idx == base {
            if let Some(kz) = kinzi_idx {
                if kz + 2 < syl_end {
                    rebuilt.push(original[kz - syl_start]);
                    rebuilt.push(original[kz + 1 - syl_start]);
                    rebuilt.push(original[kz + 2 - syl_start]);
                }
            }
        }
    }

    debug_assert_eq!(rebuilt.len(), syl_end - syl_start);
    glyphs[syl_start..syl_end].copy_from_slice(&rebuilt);
}

/// Returns a length-`codepoints.len() + 1` array mapping codepoint
/// index to UTF-8 byte offset. `out[i]` is the byte offset of the
/// i'th codepoint; `out[len]` is the total byte length.
fn cluster_byte_offsets(codepoints: &[char]) -> Vec<u32> {
    let mut out = Vec::with_capacity(codepoints.len() + 1);
    let mut byte = 0u32;
    for &c in codepoints {
        out.push(byte);
        byte = byte.saturating_add(c.len_utf8() as u32);
    }
    out.push(byte);
    out
}

/// Merges all cluster byte offsets that belong to a syllable to the
/// minimum offset in that syllable's byte range. Matches HarfBuzz /
/// rustybuzz behaviour — downstream callers see one cluster id per
/// syllable (the byte offset of the first codepoint) even when GSUB
/// substitutions have collapsed glyphs inside the syllable.
fn merge_syllable_clusters(glyphs: &mut [Glyph], syllables: &[Syllable], byte_offsets: &[u32]) {
    for syl in syllables {
        if syl.end == syl.start {
            continue;
        }
        let byte_start = byte_offsets[syl.start];
        let byte_end = byte_offsets[syl.end];
        for g in glyphs.iter_mut() {
            if g.cluster >= byte_start && g.cluster < byte_end {
                g.cluster = byte_start;
            }
        }
    }
}

/// Generic USE shaping entry point — used by Myanmar, Thai, Lao and
/// Old-Hangul runs. Mirrors [`shape_khmer`] but takes the script-
/// priority table and the (basic, topographical) feature slices as
/// parameters so each script can supply its own set. The syllable
/// segmenter and pre-base reorder are script-agnostic: they run off
/// the [`UseCategory`] / [`UsePosition`] tables which already encode
/// per-script positional rules.
///
/// `reorder_prebase` controls whether the pre-base vowel reorder
/// runs. Thai and Lao pre-base vowels (sara e and friends) are
/// logically typed *before* the base consonant already — so the
/// reorder pass would be a no-op at best and break clustering at
/// worst. Passing `false` skips it.
#[allow(clippy::too_many_arguments)]
pub fn shape_use(
    gsub: Option<&Gsub<'_>>,
    gdef: Option<&Gdef<'_>>,
    codepoints: &[char],
    glyphs: &mut Vec<Glyph>,
    script_priority: &[[u8; 4]],
    basic_features: &[&[u8; 4]],
    topographical_features: &[&[u8; 4]],
    reorder_prebase: bool,
) {
    if codepoints.is_empty() || glyphs.is_empty() {
        return;
    }

    // 1. Segment.
    let syllables = segment_syllables(codepoints);

    // 2. Initial reordering. Some scripts (Thai, Lao) type pre-base
    //    vowels before the base already, so the reorder would break
    //    cluster alignment — skip it in that case.
    if reorder_prebase {
        for syllable in &syllables {
            initial_reorder(codepoints, glyphs, syllable);
        }
    }

    // 3. Basic features.
    if let Some(gsub) = gsub {
        for tag in basic_features {
            apply_gsub_feature_in_scripts(gsub, glyphs, gdef, **tag, 0, script_priority);
        }
    }

    // 4. Topographical features.
    if let Some(gsub) = gsub {
        for tag in topographical_features {
            apply_gsub_feature_in_scripts(gsub, glyphs, gdef, **tag, 0, script_priority);
        }
    }

    // 5. Cluster merge.
    let byte_offsets = cluster_byte_offsets(codepoints);
    merge_syllable_clusters(glyphs, &syllables, &byte_offsets);
}

/// Entry point for Myanmar runs. Routes through the generic USE
/// dispatch with the Myanmar script-tag priority and the Myanmar-
/// specific feature chain (adds `rphf` for kinzi and keeps
/// `pref`/`blwf`/`pstf`/`cjct` for medial + subjoined handling).
pub fn shape_myanmar(
    gsub: Option<&Gsub<'_>>,
    gdef: Option<&Gdef<'_>>,
    codepoints: &[char],
    glyphs: &mut Vec<Glyph>,
) {
    shape_use(
        gsub,
        gdef,
        codepoints,
        glyphs,
        MYANMAR_SCRIPT_PRIORITY,
        MYANMAR_BASIC_FEATURES,
        MYANMAR_TOPOGRAPHICAL_FEATURES,
        true,
    );
}

/// Entry point for Thai runs. Thai has no halant and no subjoining;
/// the shaping reduces to contextual forms + mark positioning. We
/// still segment into syllables so the cluster-merge pass groups
/// tone marks with their consonant — matches HarfBuzz's Thai shaper
/// for every string in the 0.2.0 corpus.
pub fn shape_thai(
    gsub: Option<&Gsub<'_>>,
    gdef: Option<&Gdef<'_>>,
    codepoints: &[char],
    glyphs: &mut Vec<Glyph>,
) {
    shape_use(
        gsub,
        gdef,
        codepoints,
        glyphs,
        THAI_SCRIPT_PRIORITY,
        THAI_LAO_FEATURES,
        &[],
        false,
    );
}

/// Entry point for Lao runs. Lao is structurally near-identical to
/// Thai — same feature set, no reorder, different script tag.
pub fn shape_lao(
    gsub: Option<&Gsub<'_>>,
    gdef: Option<&Gdef<'_>>,
    codepoints: &[char],
    glyphs: &mut Vec<Glyph>,
) {
    shape_use(
        gsub,
        gdef,
        codepoints,
        glyphs,
        LAO_SCRIPT_PRIORITY,
        THAI_LAO_FEATURES,
        &[],
        false,
    );
}

/// Entry point for Hangul runs — specifically Jamo (Old Hangul)
/// decomposed text. Precomposed syllables still flow through the
/// default path in [`crate::shape`]; only runs containing at least
/// one Jamo codepoint land here. The feature chain drives
/// `ljmo`/`vjmo`/`tjmo` so Leading / Vowel / Trailing jamo pick
/// their positional variant glyphs.
pub fn shape_hangul(
    gsub: Option<&Gsub<'_>>,
    gdef: Option<&Gdef<'_>>,
    codepoints: &[char],
    glyphs: &mut Vec<Glyph>,
) {
    shape_use(
        gsub,
        gdef,
        codepoints,
        glyphs,
        HANGUL_SCRIPT_PRIORITY,
        HANGUL_FEATURES,
        &[],
        false,
    );
}

/// Entry point for N'Ko runs. N'Ko is alphabetic + tone marks — no
/// pre-base reorder, no halant. Uses the USE basic feature chain
/// without subjoining (only `ccmp` / `liga` / `calt` in practice
/// drive shaping for the current Noto Sans NKo build).
pub fn shape_nko(
    gsub: Option<&Gsub<'_>>,
    gdef: Option<&Gdef<'_>>,
    codepoints: &[char],
    glyphs: &mut Vec<Glyph>,
) {
    shape_use(
        gsub,
        gdef,
        codepoints,
        glyphs,
        NKO_SCRIPT_PRIORITY,
        THAI_LAO_FEATURES,
        &[],
        false,
    );
}

/// Entry point for Buginese runs. Brahmic — pre-base reorder fires
/// for sara e (U+1A19). Uses the full USE feature chain.
pub fn shape_buginese(
    gsub: Option<&Gsub<'_>>,
    gdef: Option<&Gdef<'_>>,
    codepoints: &[char],
    glyphs: &mut Vec<Glyph>,
) {
    shape_use(
        gsub,
        gdef,
        codepoints,
        glyphs,
        BUGINESE_SCRIPT_PRIORITY,
        USE_BASIC_FEATURES,
        USE_TOPOGRAPHICAL_FEATURES,
        true,
    );
}

/// Entry point for Tai Tham (Lanna) runs.
pub fn shape_tai_tham(
    gsub: Option<&Gsub<'_>>,
    gdef: Option<&Gdef<'_>>,
    codepoints: &[char],
    glyphs: &mut Vec<Glyph>,
) {
    shape_use(
        gsub,
        gdef,
        codepoints,
        glyphs,
        TAI_THAM_SCRIPT_PRIORITY,
        USE_BASIC_FEATURES,
        USE_TOPOGRAPHICAL_FEATURES,
        true,
    );
}

/// Entry point for Balinese runs.
pub fn shape_balinese(
    gsub: Option<&Gsub<'_>>,
    gdef: Option<&Gdef<'_>>,
    codepoints: &[char],
    glyphs: &mut Vec<Glyph>,
) {
    shape_use(
        gsub,
        gdef,
        codepoints,
        glyphs,
        BALINESE_SCRIPT_PRIORITY,
        USE_BASIC_FEATURES,
        USE_TOPOGRAPHICAL_FEATURES,
        true,
    );
}

/// Entry point for Sundanese runs.
pub fn shape_sundanese(
    gsub: Option<&Gsub<'_>>,
    gdef: Option<&Gdef<'_>>,
    codepoints: &[char],
    glyphs: &mut Vec<Glyph>,
) {
    shape_use(
        gsub,
        gdef,
        codepoints,
        glyphs,
        SUNDANESE_SCRIPT_PRIORITY,
        USE_BASIC_FEATURES,
        USE_TOPOGRAPHICAL_FEATURES,
        true,
    );
}

/// Entry point for Lepcha runs.
pub fn shape_lepcha(
    gsub: Option<&Gsub<'_>>,
    gdef: Option<&Gdef<'_>>,
    codepoints: &[char],
    glyphs: &mut Vec<Glyph>,
) {
    shape_use(
        gsub,
        gdef,
        codepoints,
        glyphs,
        LEPCHA_SCRIPT_PRIORITY,
        USE_BASIC_FEATURES,
        USE_TOPOGRAPHICAL_FEATURES,
        true,
    );
}

/// Entry point for Limbu runs.
pub fn shape_limbu(
    gsub: Option<&Gsub<'_>>,
    gdef: Option<&Gdef<'_>>,
    codepoints: &[char],
    glyphs: &mut Vec<Glyph>,
) {
    shape_use(
        gsub,
        gdef,
        codepoints,
        glyphs,
        LIMBU_SCRIPT_PRIORITY,
        USE_BASIC_FEATURES,
        USE_TOPOGRAPHICAL_FEATURES,
        true,
    );
}

/// Entry point for Cham runs.
pub fn shape_cham(
    gsub: Option<&Gsub<'_>>,
    gdef: Option<&Gdef<'_>>,
    codepoints: &[char],
    glyphs: &mut Vec<Glyph>,
) {
    shape_use(
        gsub,
        gdef,
        codepoints,
        glyphs,
        CHAM_SCRIPT_PRIORITY,
        USE_BASIC_FEATURES,
        USE_TOPOGRAPHICAL_FEATURES,
        true,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn cps(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    fn fake_glyphs(n: usize) -> Vec<Glyph> {
        (0..n).map(|i| Glyph::new(i as u32 + 1, i as u32)).collect()
    }

    #[test]
    fn single_consonant_is_one_syllable() {
        // ក U+1780 — one base, one syllable.
        let cp = cps("\u{1780}");
        let syl = segment_syllables(&cp);
        assert_eq!(syl.len(), 1);
        assert_eq!(syl[0].kind, SyllableKind::Consonant);
        assert_eq!(syl[0].start, 0);
        assert_eq!(syl[0].end, 1);
        assert_eq!(syl[0].base_index, Some(0));
    }

    #[test]
    fn consonant_plus_post_base_matra_is_one_syllable() {
        // កា — ka + aa.
        let cp = cps("\u{1780}\u{17B6}");
        let syl = segment_syllables(&cp);
        assert_eq!(syl.len(), 1);
        assert_eq!(syl[0].end, 2);
        assert_eq!(syl[0].base_index, Some(0));
    }

    #[test]
    fn coeng_conjunct_keeps_last_base() {
        // ស្ត — sa + coeng + ta. One syllable, base is the ta at idx 2.
        let cp = cps("\u{179F}\u{17D2}\u{178F}");
        let syl = segment_syllables(&cp);
        assert_eq!(syl.len(), 1);
        assert_eq!(syl[0].kind, SyllableKind::Consonant);
        assert_eq!(syl[0].end, 3);
        assert_eq!(syl[0].base_index, Some(2));
    }

    #[test]
    fn pre_base_vowel_moves_before_base() {
        // កេ = ka + sign-e. Pre-base matra is typed after the base
        // but renders before it.
        let cp = cps("\u{1780}\u{17C1}");
        let mut glyphs = fake_glyphs(2);
        let original = glyphs.clone();
        let syls = segment_syllables(&cp);
        for s in &syls {
            initial_reorder(&cp, &mut glyphs, s);
        }
        assert_eq!(glyphs[0], original[1], "sign-e should sit first visually");
        assert_eq!(glyphs[1], original[0], "ka should sit second");
    }

    #[test]
    fn post_base_vowel_stays_put() {
        // កា — sign-aa is post-base; no reorder.
        let cp = cps("\u{1780}\u{17B6}");
        let mut glyphs = fake_glyphs(2);
        let before = glyphs.clone();
        let syls = segment_syllables(&cp);
        for s in &syls {
            initial_reorder(&cp, &mut glyphs, s);
        }
        assert_eq!(glyphs, before);
    }

    #[test]
    fn independent_vowel_is_vowel_syllable() {
        // ឣ U+17A3 historically independent vowel a. After our
        // table it is classed as B (base) — still a single
        // syllable. Use U+17A5 (real IV) for the vowel path.
        let cp = cps("\u{17A5}");
        let syl = segment_syllables(&cp);
        assert_eq!(syl.len(), 1);
        assert_eq!(syl[0].kind, SyllableKind::Vowel);
    }

    #[test]
    fn khmer_digits_are_symbol_pass_through() {
        // ០១២ — Khmer digits 0,1,2 — three Symbol syllables, one
        // per digit. Each keeps its own cluster id (not merged to
        // the first byte offset), matching rustybuzz.
        let cp = cps("\u{17E0}\u{17E1}\u{17E2}");
        let syl = segment_syllables(&cp);
        assert_eq!(syl.len(), 3);
        assert!(syl.iter().all(|s| s.kind == SyllableKind::Symbol));
    }

    #[test]
    fn multi_syllable_run_segments_correctly() {
        // សួស្តី — SUS TI (hello). 6 codepoints, 2 syllables:
        //   សួ (sa + below-base u)                — 3 codepoints
        //   ស្តី (sa + coeng + ta + pre-base ii)   — 3 codepoints? No:
        //       ស 179F, ្ 17D2, ត 178F, ី 17B8 — 4 cps
        // Input: 179F 17BD 179F 17D2 178F 17B8 — six cps.
        // Hmm, សួ = sa(179F) + ua(17BD); ស្តី = sa(179F) + coeng(17D2)
        //       + ta(178F) + ii(17B8). Two syllables.
        let cp = cps("\u{179F}\u{17BD}\u{179F}\u{17D2}\u{178F}\u{17B8}");
        let syl = segment_syllables(&cp);
        assert_eq!(syl.len(), 2);
        assert_eq!(syl[0].end, 2);
        assert_eq!(syl[1].end, 6);
        assert_eq!(syl[1].base_index, Some(4)); // ta is visible base
    }

    #[test]
    fn empty_input_produces_no_syllables() {
        assert!(segment_syllables(&[]).is_empty());
    }

    #[test]
    fn shape_khmer_without_gsub_only_reorders() {
        // កេ — reorder, no GSUB. After reorder the sign-e sits
        // first; after the cluster-merge pass both glyphs share the
        // syllable's head byte offset (0 for `fake_glyphs` which
        // mirrors a UTF-8 buffer where ka starts at byte 0). We
        // verify the glyph IDs moved (1 → 0 by original mapping) so
        // the test still catches a reorder regression.
        let cp = cps("\u{1780}\u{17C1}");
        let mut glyphs = fake_glyphs(2);
        let original = glyphs.clone();
        shape_khmer(None, None, &cp, &mut glyphs);
        assert_eq!(glyphs[0].glyph_id, original[1].glyph_id);
        assert_eq!(glyphs[1].glyph_id, original[0].glyph_id);
        // Cluster merge: both glyphs carry cluster 0 (syllable
        // start) after shaping.
        assert_eq!(glyphs[0].cluster, 0);
        assert_eq!(glyphs[1].cluster, 0);
    }

    #[test]
    fn pre_base_with_coeng_moves_matra_to_syllable_head() {
        // ស្តេ = sa + coeng + ta + sign-e.
        //
        // The USE pre-base rule moves the sign-e to the very front
        // of the syllable, not just before the base. Keeping
        // `coeng + ta` adjacent is what lets the GSUB `blwf`
        // feature collapse them into a single subscript-ta glyph
        // in a later pass — matches rustybuzz output.
        //
        // Codepoint indices: sa=0, coeng=1, ta=2, sign-e=3.
        // fake_glyphs(4) uses index as cluster, so post-reorder we
        // expect clusters [3, 0, 1, 2].
        let cp = cps("\u{179F}\u{17D2}\u{178F}\u{17C1}");
        let mut glyphs = fake_glyphs(4);
        let syls = segment_syllables(&cp);
        assert_eq!(syls.len(), 1);
        assert_eq!(syls[0].base_index, Some(2));
        for s in &syls {
            initial_reorder(&cp, &mut glyphs, s);
        }
        assert_eq!(glyphs[0].cluster, 3); // sign-e
        assert_eq!(glyphs[1].cluster, 0); // sa
        assert_eq!(glyphs[2].cluster, 1); // coeng
        assert_eq!(glyphs[3].cluster, 2); // ta
    }

    #[test]
    fn broken_leading_matra_advances_one_codepoint() {
        // Leading matra with no base — broken cluster. The
        // segmenter must still advance so the outer loop ends.
        let cp = cps("\u{17B6}\u{1780}");
        let syl = segment_syllables(&cp);
        assert_eq!(syl.len(), 2);
        assert_eq!(syl[0].kind, SyllableKind::Broken);
        assert_eq!(syl[0].end, 1);
    }

    #[test]
    fn syllable_with_final_mark_absorbs_nikahit() {
        // កំ = ka + nikahit (final mark, U+17C6). One syllable.
        let cp = cps("\u{1780}\u{17C6}");
        let syl = segment_syllables(&cp);
        assert_eq!(syl.len(), 1);
        assert_eq!(syl[0].end, 2);
    }

    #[test]
    fn multiple_pre_base_matras_all_move() {
        // Synthetic: ka + VPre + VPre + aa. Rare but legal. Both
        // pre-base signs end up before the base in typing order.
        let cp = cps("\u{1780}\u{17C1}\u{17C2}\u{17B6}");
        let mut glyphs = fake_glyphs(4);
        let syls = segment_syllables(&cp);
        for s in &syls {
            initial_reorder(&cp, &mut glyphs, s);
        }
        // After reorder: VPre, VPre, ka, aa.
        assert_eq!(glyphs[0].cluster, 1);
        assert_eq!(glyphs[1].cluster, 2);
        assert_eq!(glyphs[2].cluster, 0);
        assert_eq!(glyphs[3].cluster, 3);
    }

    #[test]
    fn feature_lists_are_deterministic() {
        // Compile-time check — the const slice of features is the
        // one the state machine dispatches, in the order the MS spec
        // specifies. Assertion is about order so downstream reviewers
        // can eyeball the slice instead of re-deriving it.
        assert_eq!(
            USE_BASIC_FEATURES,
            &[
                b"locl", b"ccmp", b"rphf", b"pref", b"rkrf", b"abvf", b"blwf", b"half", b"pstf",
                b"vatu", b"cjct", b"isol",
            ]
        );
        assert_eq!(
            USE_TOPOGRAPHICAL_FEATURES,
            &[b"abvs", b"blws", b"haln", b"pres", b"psts"]
        );
    }

    #[test]
    fn script_priority_starts_with_khmr() {
        assert_eq!(KHMER_SCRIPT_PRIORITY[0], *b"khmr");
    }

    #[test]
    fn cluster_merge_collapses_syllable_to_head_offset() {
        // កេ — pre-base reorder followed by the cluster-merge pass
        // leaves every glyph in the syllable carrying the head
        // byte offset (0 here — ka is first in the UTF-8 stream).
        // Matches HarfBuzz / rustybuzz behaviour so callers see one
        // cluster id per syllable.
        let cp = cps("\u{1780}\u{17C1}");
        let mut glyphs = vec![Glyph::new(10, 0), Glyph::new(20, 3)];
        shape_khmer(None, None, &cp, &mut glyphs);
        assert_eq!(glyphs[0].cluster, 0);
        assert_eq!(glyphs[1].cluster, 0);
        // Glyph IDs confirm reorder happened (20 was at cluster 3,
        // now it is first).
        assert_eq!(glyphs[0].glyph_id, 20);
        assert_eq!(glyphs[1].glyph_id, 10);
    }
}
