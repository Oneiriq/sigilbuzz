//! HarfBuzz's font-aware normalizer (`_hb_ot_shape_normalize` in
//! `hb-ot-shape-normalize.cc`, HarfBuzz 14.5.0), which also maps the
//! run's characters to glyphs.
//!
//! It closely follows the Unicode normalization algorithm but lets the
//! font decide. Each cluster (a base and the marks after it) goes
//! through three rounds:
//!
//! 1. Decompose: a character the font does not map is replaced by its
//!    canonical decomposition when the font maps every piece. In the
//!    modes that do not short-circuit (the Indic, Khmer, Myanmar and
//!    USE shapers), clusters decompose even when the font maps the
//!    precomposed character. A cluster holding a variation selector is
//!    mapped character by character instead.
//! 2. Reorder: each run of marks with a nonzero modified combining
//!    class is sorted by class (stably), then the shaper's hook adjusts
//!    it (Arabic modifier combining marks, the Hebrew patah and qamats
//!    order).
//! 3. Recompose (composed modes only): a mark composes with the
//!    starter before it when nothing in between blocks it, the shaper
//!    allows the pair, and the font maps the composite.
//!
//! Each shaper picks its mode and hooks (see [`Shaper`]). Every
//! character produced keeps the cluster of the character it came
//! from; moving a mark during reordering merges the clusters it moves
//! across, and recomposing merges the pair's clusters, as HarfBuzz's
//! `merge_clusters` and `merge_out_clusters` do: at the monotone
//! cluster levels only.
//!
//! Characters a font maps neither directly nor through a decomposition
//! get glyph 0, with two exceptions: a space character (U+2002 EN
//! SPACE, U+202F NARROW NO-BREAK SPACE, ...) takes the space glyph and
//! records its kind so positioning can fix its width (see
//! `fallback::adjust_spaces`), and U+2011 NON-BREAKING HYPHEN falls
//! back to the U+2010 HYPHEN glyph.
//!
//! A COMBINING GRAPHEME JOINER starts hidden (GSUB cannot skip it, see
//! `glyph_props`); after reordering, one that blocked no reordering is
//! un-hidden, as in HarfBuzz.
//!
//! A character followed by a variation selector maps through the
//! font's cmap format 14 subtable, as in HarfBuzz: when the font has a
//! glyph for the sequence, that glyph replaces both characters.
//! Otherwise both map on their own and GSUB sees them.

mod hooks;

use alloc::vec::Vec;

use super::cluster::{merge_clusters, Clustered};
use super::fallback;
use super::glyph_flags;
use super::segment::Segment;
use super::shaper::{NormalizationMode, Shaper};
use super::{glyph_props, ignorables};
use crate::buffer::{char_class, unicode_prop, ClusterLevel, Glyph, GlyphFlags};
use crate::tables::cmap::Cmap;
use crate::tables::layout::skip_iter::match_prop;
use crate::unicode::general_category::{
    general_category_class, is_nonspacing_mark, GeneralCategoryClass,
};
use crate::unicode::normalize::modified_combining_class;

/// Longest run of marks the reorder round sorts
/// (`HB_OT_SHAPE_MAX_COMBINING_MARKS`); longer runs keep their order.
const MAX_COMBINING_MARKS: usize = 32;

/// U+034F COMBINING GRAPHEME JOINER.
const CGJ: char = '\u{034F}';

/// One character of a run while it is being normalized: HarfBuzz's
/// glyph info before glyph ids take over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct NormChar {
    /// The character.
    pub(super) ch: char,
    /// Its cluster (a byte offset into the buffer text).
    pub(super) cluster: u32,
    /// True when the character is the mirror of a character a backward
    /// run replaced (so `rtlm` skips it).
    pub(super) mirrored: bool,
    /// The glyph the font maps it to, once the decompose round ran.
    glyph: u32,
    /// `char_class` bits of the character.
    class: u8,
    /// Its modified combining class when it is a mark, zero otherwise.
    mcc: u8,
    /// A COMBINING GRAPHEME JOINER that blocked no mark reordering, so
    /// GSUB may skip it like any other ignorable.
    unhidden: bool,
    /// Glyph flags the character carries from before normalization
    /// (grapheme merges a cluster level skipped).
    flags: GlyphFlags,
    /// A variation selector the font could not resolve after its base,
    /// shown as the buffer's not-found variation selector glyph: it is
    /// no longer default ignorable, so it is neither hidden nor removed.
    shown_selector: bool,
}

impl Clustered for NormChar {
    fn cluster(&self) -> u32 {
        self.cluster
    }

    fn set_cluster(&mut self, cluster: u32) {
        self.cluster = cluster;
    }

    fn flags(&self) -> GlyphFlags {
        self.flags
    }

    fn set_flags(&mut self, flags: GlyphFlags) {
        self.flags = flags;
    }
}

impl NormChar {
    /// A character of the run as it enters normalization.
    pub(super) fn new(ch: char, cluster: u32, mirrored: bool) -> Self {
        let mut c = Self {
            ch,
            cluster,
            mirrored,
            glyph: 0,
            class: 0,
            mcc: 0,
            unhidden: false,
            flags: GlyphFlags::empty(),
            shown_selector: false,
        };
        c.set_char(ch);
        c
    }

    /// Replaces the character and recomputes its Unicode properties
    /// (HarfBuzz's `_hb_glyph_info_set_unicode_props`).
    fn set_char(&mut self, ch: char) {
        self.ch = ch;
        (self.class, self.mcc) = mark_props(ch);
    }

    /// `_hb_glyph_info_is_unicode_mark`.
    const fn is_mark(&self) -> bool {
        self.class & char_class::MARK != 0
    }

    /// The same character mapped to `glyph`.
    const fn with_glyph(mut self, glyph: u32) -> Self {
        self.glyph = glyph;
        self
    }

    /// The shaping glyph for this character: the mapped glyph id and
    /// cluster, the ignorable and matching props of its character (see
    /// `glyph_props`), and the character class.
    pub(super) fn glyph(&self) -> Glyph {
        let mut g = Glyph::new(self.glyph, self.cluster);
        g.unicode_props =
            ignorables::unicode_props(self.ch) | glyph_props::initial(self.ch, self.class);
        if self.unhidden {
            g.unicode_props &= !match_prop::HIDDEN;
        }
        if self.shown_selector {
            g.unicode_props &= !unicode_prop::DEFAULT_IGNORABLE;
        }
        g.char_class = self.class;
        g.combining_class = self.mcc;
        g.flags = self.flags;
        g
    }
}

/// The `char_class` bits and modified combining class of `ch`, as
/// HarfBuzz's `_hb_glyph_info_set_unicode_props` records them: the mark
/// bits and the class of a mark, and zero for anything else.
pub(super) fn mark_props(ch: char) -> (u8, u8) {
    if general_category_class(ch) != Some(GeneralCategoryClass::Mark) {
        return (0, 0);
    }
    let class = if is_nonspacing_mark(ch) {
        char_class::MARK | char_class::NONSPACING_MARK
    } else {
        char_class::MARK
    };
    (class, modified_combining_class(ch))
}

/// Normalizes each segment of a run with the normalizer
/// `normalizer_for` builds for it. On entry `glyphs` only carries each
/// code point's cluster; on return `codepoints`, `glyphs`, and
/// `mirrored` hold the normalized run, one entry per character, and
/// each segment's range covers its normalized characters.
pub(super) fn normalize_segments<'a>(
    codepoints: &mut Vec<char>,
    glyphs: &mut Vec<Glyph>,
    mirrored: &mut Vec<bool>,
    segments: &mut [Segment],
    normalizer_for: impl Fn(&Segment) -> Normalizer<'a>,
) {
    let mut out_codepoints = Vec::with_capacity(codepoints.len());
    let mut out_glyphs = Vec::with_capacity(glyphs.len());
    let mut out_mirrored = Vec::with_capacity(mirrored.len());
    for seg in segments.iter_mut() {
        let chars: Vec<NormChar> = seg
            .cp_range
            .clone()
            .map(|i| {
                let mut c = NormChar::new(codepoints[i], glyphs[i].cluster, mirrored[i]);
                c.flags = glyphs[i].flags;
                // A character a shaper inserted in place of a mark
                // keeps that mark's properties, as HarfBuzz's
                // `replace_glyphs` copies them (the Hangul shaper's
                // dotted circle for a tone mark, and the dotted circle
                // of the vowel constraints).
                if glyphs[i].char_class & char_class::MARK != 0 {
                    c.class = glyphs[i].char_class;
                    c.mcc = glyphs[i].combining_class;
                }
                c
            })
            .collect();
        let start = out_codepoints.len();
        for c in normalizer_for(seg).run(&chars) {
            out_codepoints.push(c.ch);
            out_glyphs.push(c.glyph());
            out_mirrored.push(c.mirrored);
        }
        seg.cp_range = start..out_codepoints.len();
    }
    *codepoints = out_codepoints;
    *glyphs = out_glyphs;
    *mirrored = out_mirrored;
}

/// The normalizer for one run: the font's character map, the run's
/// shaper, and whether the font has GPOS mark positioning for the run
/// (which turns off the Hebrew presentation-form compositions).
pub(super) struct Normalizer<'a> {
    pub(super) cmap: &'a Cmap<'a>,
    pub(super) shaper: Shaper,
    pub(super) has_gpos_mark: bool,
    /// The buffer's cluster level, which decides which merges happen.
    pub(super) level: ClusterLevel,
    /// The run gets fallback mark positioning: once normalized, its
    /// nonspacing marks take the positional classes their combining
    /// classes stand for (`recategorize_combining_class`).
    pub(super) recategorize_marks: bool,
    /// The buffer's not-found variation selector glyph
    /// ([`crate::Buffer::set_not_found_variation_selector_glyph`]).
    pub(super) not_found_variation_selector: Option<u32>,
}

impl Normalizer<'_> {
    /// Normalizes `chars` and maps them to glyphs.
    pub(super) fn run(&self, chars: &[NormChar]) -> Vec<NormChar> {
        if chars.is_empty() {
            return Vec::new();
        }
        let mode = self.shaper.normalization_mode();
        let always_short_circuit = mode == NormalizationMode::None;
        let might_short_circuit = always_short_circuit
            || !matches!(
                mode,
                NormalizationMode::Decomposed | NormalizationMode::ComposedDiacriticsNoShortCircuit
            );
        let (mut out, all_simple) = self.decompose_round(chars, might_short_circuit);
        if !all_simple {
            self.reorder_round(&mut out);
        }
        unhide_cgjs(&mut out);
        if !all_simple
            && matches!(
                mode,
                NormalizationMode::ComposedDiacritics
                    | NormalizationMode::ComposedDiacriticsNoShortCircuit
            )
        {
            self.compose_round(&mut out);
        }
        if self.recategorize_marks {
            for c in out
                .iter_mut()
                .filter(|c| c.class & char_class::NONSPACING_MARK != 0)
            {
                c.mcc = fallback::recategorize_combining_class(c.ch, c.mcc);
            }
        }
        out
    }

    /// The font's glyph for `ch`, if it maps it.
    fn nominal(&self, ch: char) -> Option<u32> {
        self.cmap.glyph_id(ch).map(u32::from)
    }

    /// First round. Returns the decomposed run and whether every
    /// cluster was a single character (then the other rounds are
    /// skipped, as in HarfBuzz).
    fn decompose_round(
        &self,
        input: &[NormChar],
        might_short_circuit: bool,
    ) -> (Vec<NormChar>, bool) {
        let always_short_circuit = self.shaper.normalization_mode() == NormalizationMode::None;
        let count = input.len();
        let mut out = Vec::with_capacity(count);
        let mut all_simple = true;
        let mut idx = 0;
        loop {
            let mut end = idx + 1;
            while end < count && !input[end].is_mark() {
                end += 1;
            }
            if end < count {
                // Leave one base for the marks to cluster with.
                end -= 1;
            }
            // From idx to end are simple clusters.
            if might_short_circuit {
                while idx < end {
                    let Some(glyph) = self.nominal(input[idx].ch) else {
                        break;
                    };
                    out.push(input[idx].with_glyph(glyph));
                    idx += 1;
                }
            }
            while idx < end {
                self.decompose_current(input[idx], might_short_circuit, &mut out);
                idx += 1;
            }
            if idx >= count {
                break;
            }
            all_simple = false;
            // idx to end is one cluster with marks.
            end = idx + 1;
            while end < count && input[end].is_mark() {
                end += 1;
            }
            self.decompose_cluster(&input[idx..end], always_short_circuit, &mut out);
            idx = end;
            if idx >= count {
                break;
            }
        }
        (out, all_simple)
    }

    /// `decompose_multi_char_cluster`.
    fn decompose_cluster(
        &self,
        cluster: &[NormChar],
        short_circuit: bool,
        out: &mut Vec<NormChar>,
    ) {
        if cluster.iter().any(|c| is_variation_selector(c.ch)) {
            self.map_variation_selector_cluster(cluster, out);
            return;
        }
        for &c in cluster {
            self.decompose_current(c, short_circuit, out);
        }
    }

    /// `handle_variation_selector_cluster`: a cluster with a variation
    /// selector is not normalized. A character followed by a selector
    /// takes the glyph the font's format 14 subtable gives the pair
    /// ([`Cmap::variation_glyph`]), and the selector goes away, its
    /// cluster merged into the character's (`replace_glyphs (2, 1)`).
    /// When the font has no glyph for the pair, both characters map on
    /// their own, and so does any further selector. The selector right
    /// after the character becomes the buffer's not-found variation
    /// selector glyph after positioning when one is set, and stays
    /// visible (see [`show_variation_selectors`]). Every other character
    /// maps on its own (glyph 0 when the font lacks it).
    fn map_variation_selector_cluster(&self, cluster: &[NormChar], out: &mut Vec<NormChar>) {
        let mut chars = cluster.to_vec();
        let nominal = |c: NormChar| c.with_glyph(self.nominal(c.ch).unwrap_or(0));
        let mut i = 0;
        while i + 1 < chars.len() {
            if !is_variation_selector(chars[i + 1].ch) {
                out.push(nominal(chars[i]));
                i += 1;
                continue;
            }
            match self.cmap.variation_glyph(chars[i].ch, chars[i + 1].ch) {
                Some(glyph) => {
                    merge_clusters(&mut chars, i, i + 2, self.level);
                    out.push(chars[i].with_glyph(u32::from(glyph)));
                }
                None => {
                    out.push(nominal(chars[i]));
                    // HarfBuzz's `_hb_glyph_info_set_variation_selector`: no
                    // mark until `show_variation_selectors`, and no longer
                    // ignorable when a not-found glyph will replace it.
                    let mut selector = nominal(chars[i + 1]);
                    selector.class = char_class::UNRESOLVED_SELECTOR;
                    selector.shown_selector = self.not_found_variation_selector.is_some();
                    out.push(selector);
                }
            }
            i += 2;
            while i < chars.len() && is_variation_selector(chars[i].ch) {
                out.push(nominal(chars[i]));
                i += 1;
            }
        }
        if let Some(&last) = chars.get(i) {
            out.push(nominal(last));
        }
    }

    /// `decompose_current_character`: maps `cur`, decomposing it when
    /// the mode or the font calls for it.
    fn decompose_current(&self, cur: NormChar, shortest: bool, out: &mut Vec<NormChar>) {
        let u = cur.ch;
        if shortest {
            if let Some(glyph) = self.nominal(u) {
                out.push(cur.with_glyph(glyph));
                return;
            }
        }
        if self.decompose(cur, shortest, u, out) > 0 {
            return;
        }
        if !shortest {
            if let Some(glyph) = self.nominal(u) {
                out.push(cur.with_glyph(glyph));
                return;
            }
        }
        // Every space character with a fallback is a space separator
        // (General_Category Zs), which is what HarfBuzz checks first.
        let kind = fallback::space_fallback(u);
        if kind != fallback::space::NOT_SPACE {
            if let Some(space) = self.nominal(' ') {
                let mut c = cur.with_glyph(space);
                c.class |= kind << char_class::SPACE_SHIFT;
                out.push(c);
                return;
            }
        }
        if u == '\u{2011}' {
            // The only no-break variant of a character that is not a
            // space: fall back to U+2010 HYPHEN.
            if let Some(glyph) = self.nominal('\u{2010}') {
                out.push(cur.with_glyph(glyph));
                return;
            }
        }
        out.push(cur.with_glyph(0));
    }

    /// `decompose`: outputs the decomposition of `ab` (recursively, for
    /// the first piece) when the font maps every piece, and returns how
    /// many characters it output; zero when it output nothing.
    fn decompose(&self, cur: NormChar, shortest: bool, ab: char, out: &mut Vec<NormChar>) -> usize {
        let Some((a, b)) = hooks::decompose(self.shaper, ab) else {
            return 0;
        };
        let b = match b {
            Some(b) => match self.nominal(b) {
                Some(glyph) => Some((b, glyph)),
                None => return 0,
            },
            None => None,
        };
        let a_glyph = self.nominal(a);
        if let (true, Some(a_glyph)) = (shortest, a_glyph) {
            return output_pair(cur, (a, a_glyph), b, out);
        }
        let ret = self.decompose(cur, shortest, a, out);
        if ret > 0 {
            if let Some((b, glyph)) = b {
                out.push(output_char(cur, b, glyph));
                return ret + 1;
            }
            return ret;
        }
        match a_glyph {
            Some(a_glyph) => output_pair(cur, (a, a_glyph), b, out),
            None => 0,
        }
    }

    /// Second round: sorts each run of marks by modified combining
    /// class, then runs the shaper's reorder hook on it.
    fn reorder_round(&self, chars: &mut [NormChar]) {
        let count = chars.len();
        let mut i = 0;
        while i < count {
            if chars[i].mcc == 0 {
                i += 1;
                continue;
            }
            let mut end = i + 1;
            while end < count && chars[end].mcc != 0 {
                end += 1;
            }
            // An O(n^2) sort: only for short runs.
            if end - i <= MAX_COMBINING_MARKS {
                sort_by_class(chars, i, end, self.level);
                hooks::reorder_marks(self.shaper, chars, i, end, self.level);
            }
            i = end + 1;
        }
    }

    /// Third round: recomposes marks with their starter, in place.
    /// HarfBuzz copies the run to an output buffer as it goes; here
    /// `chars[..w]` is that output and `chars[i..]` its input. A
    /// composed mark is dropped, so the output falls behind the input,
    /// and the round stays linear however many marks compose.
    fn compose_round(&self, chars: &mut Vec<NormChar>) {
        let len = chars.len();
        if len == 0 {
            return;
        }
        let mut starter = 0;
        let mut w = 1;
        for i in 1..len {
            let cur = chars[i];
            // A non-mark never composes with the starter before it
            // (Hangul fonts in particular do not mix syllables and jamo).
            if cur.is_mark() {
                let unblocked = starter == w - 1 || chars[w - 1].mcc < cur.mcc;
                let composed = unblocked
                    .then(|| {
                        hooks::compose(self.shaper, chars[starter].ch, cur.ch, self.has_gpos_mark)
                    })
                    .flatten()
                    .and_then(|c| self.nominal(c).map(|glyph| (c, glyph)));
                if let Some((composed, glyph)) = composed {
                    self.merge_composed(chars, starter, w, i);
                    let s = &mut chars[starter];
                    s.set_char(composed);
                    s.glyph = glyph;
                    continue;
                }
            }
            chars[w] = cur;
            if cur.mcc == 0 {
                starter = w;
            }
            w += 1;
        }
        chars.truncate(w);
    }

    /// HarfBuzz's `merge_out_clusters (starter, out_len)` for a mark at
    /// `chars[i]` that composes with the output `chars[starter..w]`:
    /// [`merge_clusters`] over the output from `starter` on and the
    /// mark, as if the two were adjacent. Like the merge, it spreads
    /// back over output characters that shared the starter's cluster
    /// and forward over input characters that shared the mark's.
    fn merge_composed(&self, chars: &mut [NormChar], starter: usize, w: usize, i: usize) {
        if !self.level.is_monotone() || starter >= w || w > i {
            return;
        }
        let mark = chars[i].cluster;
        let Some(cluster) = chars[starter..w]
            .iter()
            .map(|c| c.cluster)
            .chain([mark])
            .min()
        else {
            return;
        };
        let mut end = i + 1;
        if cluster != mark {
            while end < chars.len() && chars[end - 1].cluster == chars[end].cluster {
                end += 1;
            }
        }
        let mut start = starter;
        if cluster != chars[starter].cluster {
            while start > 0 && chars[start - 1].cluster == chars[start].cluster {
                start -= 1;
            }
        }
        let (output, input) = chars.split_at_mut(i);
        for c in output[start..w].iter_mut().chain(&mut input[..end - i]) {
            glyph_flags::set_cluster(c, cluster, GlyphFlags::empty());
        }
    }
}

/// HarfBuzz's `hb_ot_deal_with_variation_selectors`, run after
/// positioning and before the ignorables are hidden: with a not-found
/// variation selector glyph set
/// ([`crate::Buffer::set_not_found_variation_selector_glyph`]), every
/// variation selector the font could not resolve after its base becomes
/// that glyph, with no advance and no offset.
pub(super) fn show_variation_selectors(glyphs: &mut [Glyph], not_found: Option<u32>) {
    let Some(glyph_id) = not_found else {
        return;
    };
    for g in glyphs
        .iter_mut()
        .filter(|g| g.char_class & char_class::UNRESOLVED_SELECTOR != 0)
    {
        g.glyph_id = glyph_id;
        g.x_advance = 0;
        g.y_advance = 0;
        g.x_offset = 0;
        g.y_offset = 0;
    }
}

/// HarfBuzz's COMBINING GRAPHEME JOINER check after reordering: a CGJ
/// that blocked no reordering (the marks around it were in order
/// anyway) stops being hidden, so GSUB can skip it.
fn unhide_cgjs(chars: &mut [NormChar]) {
    for i in 1..chars.len().saturating_sub(1) {
        if chars[i].ch == CGJ {
            let (last, next) = (chars[i - 1].mcc, chars[i + 1].mcc);
            if next == 0 || last <= next {
                chars[i].unhidden = true;
            }
        }
    }
}

/// `output_char`: `cur` replaced by `ch`, mapped to `glyph`, keeping
/// `cur`'s cluster.
fn output_char(cur: NormChar, ch: char, glyph: u32) -> NormChar {
    let mut c = cur.with_glyph(glyph);
    c.set_char(ch);
    c
}

/// Outputs `a` and, when present, `b`; returns how many.
fn output_pair(
    cur: NormChar,
    (a, a_glyph): (char, u32),
    b: Option<(char, u32)>,
    out: &mut Vec<NormChar>,
) -> usize {
    out.push(output_char(cur, a, a_glyph));
    match b {
        Some((b, b_glyph)) => {
            out.push(output_char(cur, b, b_glyph));
            2
        }
        None => 1,
    }
}

/// `hb_buffer_t::sort` by modified combining class: a stable insertion
/// sort that merges the clusters an item moves across (at the levels
/// where HarfBuzz merges).
fn sort_by_class(chars: &mut [NormChar], start: usize, end: usize, level: ClusterLevel) {
    for i in start + 1..end {
        let mut j = i;
        while j > start && chars[j - 1].mcc > chars[i].mcc {
            j -= 1;
        }
        if i == j {
            continue;
        }
        merge_clusters(chars, j, i + 1, level);
        chars[j..=i].rotate_right(1);
    }
}

/// VARIATION SELECTOR-1 to 16 and 17 to 256
/// (`hb_unicode_funcs_t::is_variation_selector`; the Mongolian free
/// variation selectors are the Mongolian shaper's business).
const fn is_variation_selector(ch: char) -> bool {
    matches!(ch as u32, 0xFE00..=0xFE0F | 0xE0100..=0xE01EF)
}

#[cfg(test)]
mod tests;
