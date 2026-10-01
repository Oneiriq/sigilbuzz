//! HarfBuzz's Hangul preprocessing (`preprocess_text_hangul` in
//! `hb-ot-shaper-hangul.cc`), run on the characters of a buffer the
//! Hangul shaper shapes, after grapheme clusters form and before
//! normalization.
//!
//! - `<L,V>` and `<L,V,T>` jamo sequences compose into the precomposed
//!   syllable when every part is a modern jamo and the font has the
//!   syllable. An `<LV,T>` pair composes the same way.
//! - A sequence that does not compose stays as jamo, and each jamo gets
//!   its `ljmo`, `vjmo`, or `tjmo` feature. A precomposed syllable the
//!   font lacks, or an `<LV>` followed by a trailing jamo that cannot
//!   join it, decomposes into jamo with those features when the font
//!   has them. The jamo of one syllable then share a cluster at the
//!   grapheme levels.
//! - A tone mark (U+302E, U+302F) right after a syllable moves in front
//!   of it, and the two share a cluster at the monotone levels, unless
//!   the font draws the tone mark with no advance (it is then made to
//!   overstrike). A tone mark after anything else gets a dotted circle
//!   to sit on: after it, or before a zero-advance one.

use alloc::vec::Vec;

use super::cluster::Clustered;
use super::glyph_flags;
use crate::buffer::{char_class, ClusterLevel, Glyph, GlyphFlags};
pub(super) use crate::ot::hangul::jamo_features;
use crate::ot::hangul::{is_l, is_t, is_v, jamo};
use crate::unicode::normalize::modified_combining_class;

const L_BASE: u32 = 0x1100;
const V_BASE: u32 = 0x1161;
const T_BASE: u32 = 0x11A7;
const L_COUNT: u32 = 19;
const V_COUNT: u32 = 21;
const T_COUNT: u32 = 28;
const S_BASE: u32 = 0xAC00;
const N_COUNT: u32 = V_COUNT * T_COUNT;
const S_COUNT: u32 = L_COUNT * N_COUNT;
const DOTTED_CIRCLE: char = '\u{25CC}';

const fn is_combining_l(u: u32) -> bool {
    L_BASE <= u && u < L_BASE + L_COUNT
}

const fn is_combining_v(u: u32) -> bool {
    V_BASE <= u && u < V_BASE + V_COUNT
}

const fn is_combining_t(u: u32) -> bool {
    T_BASE < u && u < T_BASE + T_COUNT
}

const fn is_combined_s(u: u32) -> bool {
    S_BASE <= u && u < S_BASE + S_COUNT
}

/// A Hangul tone mark (`isHangulTone`).
const fn is_tone(ch: char) -> bool {
    matches!(ch as u32, 0x302E..=0x302F)
}

/// What the preprocessing asks of the font.
pub(super) struct HangulFont<'a> {
    /// Whether the font maps a character.
    pub(super) has_glyph: &'a dyn Fn(char) -> bool,
    /// Whether the font maps a character to a glyph with no advance
    /// (`is_zero_width_char`).
    pub(super) zero_width: &'a dyn Fn(char) -> bool,
    /// Whether broken tone marks get a dotted circle: the font has one
    /// and the buffer allows it.
    pub(super) dotted_circle: bool,
}

/// One character on its way through the preprocessing.
#[derive(Clone, Copy)]
struct Entry {
    ch: char,
    glyph: Glyph,
    mirrored: bool,
    feature: u8,
}

impl Clustered for Entry {
    fn cluster(&self) -> u32 {
        self.glyph.cluster
    }

    fn set_cluster(&mut self, cluster: u32) {
        self.glyph.cluster = cluster;
    }

    fn flags(&self) -> GlyphFlags {
        self.glyph.flags
    }

    fn set_flags(&mut self, flags: GlyphFlags) {
        self.glyph.flags = flags;
    }
}

/// The output so far and the input still to read, as HarfBuzz's buffer
/// keeps them while it rewrites itself. Cluster merges see the output
/// followed by the unread input as one run, as HarfBuzz's merges do.
struct Rewrite {
    out: Vec<Entry>,
    input: Vec<Entry>,
    read: usize,
}

impl Rewrite {
    fn cur(&self, offset: usize) -> Option<char> {
        self.input.get(self.read + offset).map(|e| e.ch)
    }

    /// Copies the next input character to the output.
    fn next_glyph(&mut self) {
        if let Some(&e) = self.input.get(self.read) {
            self.out.push(e);
            self.read += 1;
        }
    }

    /// Replaces the next `num_in` input characters with `chars`, each a
    /// copy of the first one after its cluster merged over the replaced
    /// ones at the monotone `level`s (`replace_glyphs`). At the other
    /// levels the replaced characters are unsafe to break instead, as
    /// HarfBuzz's `merge_clusters` makes them.
    fn replace(&mut self, num_in: usize, chars: &[char], level: ClusterLevel) {
        if level.is_monotone() {
            self.merge(self.out.len(), self.out.len() + num_in);
        } else {
            self.unsafe_to_break_input(num_in, level);
        }
        let Some(&orig) = self.input.get(self.read) else {
            return;
        };
        self.out.extend(chars.iter().map(|&ch| Entry {
            ch,
            feature: jamo::NONE,
            ..orig
        }));
        self.read += num_in;
    }

    /// HarfBuzz's `unsafe_to_break(idx, idx + n)`: the next `n` input
    /// characters.
    fn unsafe_to_break_input(&mut self, n: usize, level: ClusterLevel) {
        if let Some(rest) = self.input.get_mut(self.read..) {
            glyph_flags::unsafe_to_break(rest, 0, n, level);
        }
    }

    /// HarfBuzz's `unsafe_to_break_from_outbuffer(start, idx)`: the
    /// output from `start` on.
    fn unsafe_to_break_output(&mut self, start: usize, level: ClusterLevel) {
        let end = self.out.len();
        glyph_flags::unsafe_to_break(&mut self.out, start, end, level);
    }

    /// The length of the run the merges see.
    fn len(&self) -> usize {
        self.out.len() + self.input.len().saturating_sub(self.read)
    }

    /// The entry at `k` of the run the merges see.
    fn at(&mut self, k: usize) -> Option<&mut Entry> {
        let out = self.out.len();
        if k < out {
            self.out.get_mut(k)
        } else {
            self.input.get_mut(self.read + (k - out))
        }
    }

    fn cluster(&mut self, k: usize) -> u32 {
        self.at(k).map_or(0, |e| e.glyph.cluster)
    }

    /// HarfBuzz's `merge_clusters_impl` over `[start, end)` of the run:
    /// the range takes its smallest cluster, extended over neighbors
    /// that share a cluster with an end whose cluster changes. A
    /// character whose cluster changes loses its glyph flags
    /// (`set_cluster`).
    fn merge(&mut self, mut start: usize, mut end: usize) {
        let limit = self.len();
        if end > limit || end <= start + 1 {
            return;
        }
        let cluster = (start..end).map(|k| self.cluster(k)).min().unwrap_or(0);
        if cluster != self.cluster(end - 1) {
            while end < limit && self.cluster(end - 1) == self.cluster(end) {
                end += 1;
            }
        }
        if cluster != self.cluster(start) {
            while start > 0 && self.cluster(start - 1) == self.cluster(start) {
                start -= 1;
            }
        }
        for k in start..end {
            if let Some(e) = self.at(k) {
                glyph_flags::set_cluster(&mut e.glyph, cluster, GlyphFlags::empty());
            }
        }
    }
}

/// Runs the Hangul preprocessing over one buffer's characters
/// (`cps`, `glyphs`, and `mirrored` are one to one) and returns each
/// resulting character's jamo feature (see [`jamo`]).
pub(super) fn preprocess(
    cps: &mut Vec<char>,
    glyphs: &mut Vec<Glyph>,
    mirrored: &mut Vec<bool>,
    font: &HangulFont<'_>,
    level: ClusterLevel,
) -> Vec<u8> {
    if cps.len() != glyphs.len() || cps.len() != mirrored.len() {
        return alloc::vec![jamo::NONE; cps.len()];
    }
    let entries: Vec<Entry> = cps
        .iter()
        .zip(glyphs.iter())
        .zip(mirrored.iter())
        .map(|((&ch, &glyph), &mirrored)| Entry {
            ch,
            glyph,
            mirrored,
            feature: jamo::NONE,
        })
        .collect();
    let mut buf = Rewrite {
        out: Vec::with_capacity(entries.len()),
        input: entries,
        read: 0,
    };
    // The most recent syllable, `out[start..end]`, valid when
    // `start < end`.
    let (mut start, mut end) = (0usize, 0usize);
    let has = font.has_glyph;
    let ch = |u: u32| char::from_u32(u).unwrap_or('\u{FFFD}');
    let graphemes = level.is_graphemes();

    while let Some(u) = buf.cur(0) {
        if is_tone(u) {
            if start < end && end == buf.out.len() {
                // The tone mark follows a syllable: move it in front,
                // unless it has no advance.
                buf.unsafe_to_break_output(start, level);
                buf.next_glyph();
                if !(font.zero_width)(u) {
                    if level.is_monotone() {
                        buf.merge(start, end + 1);
                    }
                    if let Some(span) = buf.out.get_mut(start..=end) {
                        span.rotate_right(1);
                    }
                }
            } else if font.dotted_circle {
                // No syllable to sit on: add a dotted circle. HarfBuzz
                // copies the tone mark's glyph info to it, so it sorts
                // with the marks in normalization as the tone mark
                // does (see `normalize_segments`).
                let chars = if (font.zero_width)(u) {
                    [DOTTED_CIRCLE, u]
                } else {
                    [u, DOTTED_CIRCLE]
                };
                buf.replace(1, &chars, level);
                let n = buf.out.len();
                if let Some(circle) = buf.out.get_mut(n.saturating_sub(2)..n) {
                    for e in circle.iter_mut().filter(|e| e.ch == DOTTED_CIRCLE) {
                        e.glyph.char_class = char_class::MARK;
                        e.glyph.combining_class = modified_combining_class(u);
                    }
                }
            } else {
                buf.next_glyph();
            }
            start = buf.out.len();
            end = buf.out.len();
            continue;
        }

        start = buf.out.len();
        let next = buf.cur(1);
        if is_l(u) && next.is_some() {
            let v = next.unwrap_or_default();
            if is_v(v) {
                let t = buf.cur(2).filter(|&t| is_t(t));
                let len = if t.is_some() { 3 } else { 2 };
                buf.unsafe_to_break_input(len, level);
                let (lu, vu, tu) = (u as u32, v as u32, t.map(|t| t as u32));
                if is_combining_l(lu) && is_combining_v(vu) && tu.map_or(true, is_combining_t) {
                    let tindex = tu.map_or(0, |t| t - T_BASE);
                    let s = ch(S_BASE + (lu - L_BASE) * N_COUNT + (vu - V_BASE) * T_COUNT + tindex);
                    if has(s) {
                        buf.replace(len, &[s], level);
                        end = start + 1;
                        continue;
                    }
                }
                // No composition: the jamo take their features.
                for feature in [jamo::LJMO, jamo::VJMO, jamo::TJMO].into_iter().take(len) {
                    if let Some(e) = buf.input.get_mut(buf.read) {
                        e.feature = feature;
                    }
                    buf.next_glyph();
                }
                end = start + len;
                if graphemes {
                    buf.merge(start, end);
                }
                continue;
            }
        } else if is_combined_s(u as u32) {
            let s = u as u32;
            let has_s = has(u);
            let lindex = (s - S_BASE) / N_COUNT;
            let nindex = (s - S_BASE) % N_COUNT;
            let vindex = nindex / T_COUNT;
            let tindex = nindex % T_COUNT;
            let next_t = next.filter(|&n| is_t(n));
            if tindex == 0 {
                if let Some(t) = next.filter(|&n| is_combining_t(n as u32)) {
                    // `<LV,T>`: compose if the font has the syllable.
                    let new_s = ch(s + (t as u32 - T_BASE));
                    if has(new_s) {
                        buf.replace(2, &[new_s], level);
                        end = start + 1;
                        continue;
                    }
                    // Unsafe between the LV and the T.
                    buf.unsafe_to_break_input(2, level);
                }
            }
            if !has_s || (tindex == 0 && next_t.is_some()) {
                let decomposed = [
                    ch(L_BASE + lindex),
                    ch(V_BASE + vindex),
                    ch(T_BASE + tindex),
                ];
                if has(decomposed[0]) && has(decomposed[1]) && (tindex == 0 || has(decomposed[2])) {
                    let mut s_len = if tindex == 0 { 2 } else { 3 };
                    buf.replace(1, &decomposed[..s_len], level);
                    // An `<LV>` decomposed for a trailing jamo that
                    // cannot join it takes that jamo along.
                    if has_s && tindex == 0 {
                        buf.next_glyph();
                        s_len += 1;
                    }
                    end = start + s_len;
                    let features = [jamo::LJMO, jamo::VJMO, jamo::TJMO];
                    let decomposed_out = buf.out.get_mut(start..end).unwrap_or_default();
                    for (e, &f) in decomposed_out.iter_mut().zip(&features) {
                        e.feature = f;
                    }
                    if graphemes {
                        buf.merge(start, end);
                    }
                    continue;
                }
                if tindex == 0 && next_t.is_some() {
                    // Unsafe between the LV and the T.
                    buf.unsafe_to_break_input(2, level);
                }
            }
            if has_s {
                end = start + 1;
            }
        }
        // Not a syllable start: leave `end` at or before `start`, which
        // keeps a following tone mark from moving.
        buf.next_glyph();
    }

    let entries = buf.out;
    *cps = entries.iter().map(|e| e.ch).collect();
    *glyphs = entries.iter().map(|e| e.glyph).collect();
    *mirrored = entries.iter().map(|e| e.mirrored).collect();
    entries.iter().map(|e| e.feature).collect()
}

#[cfg(test)]
mod tests;
