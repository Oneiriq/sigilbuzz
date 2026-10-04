//! The stretch of the Arabic shaper's `stch` feature (`apply_stch` in
//! `hb-ot-shaper-arabic.cc`), which runs after positioning.
//!
//! A font's `stch` feature splits a stretching mark, such as U+070F
//! SYRIAC ABBREVIATION MARK, into tiles with a multiple substitution:
//! the odd ones repeat and the even ones keep their place (see
//! `record_stch` in the `arabic_joining` module). After positioning,
//! each run of tiles stretches over the rest of its word: the repeating
//! tiles repeat as often as the word's width needs, overlapping a
//! little to fit, and every tile is drawn with a zero advance at an
//! offset across the word.
//!
//! HarfBuzz counts a glyph into the word when it is default ignorable
//! or its General_Category is one of `HB_ARABIC_GENERAL_CATEGORY_IS_WORD`
//! (`Lm`, `Lo`, the marks, the numbers, the symbols, and unassigned and
//! private-use code points). sigilbuzz knows a glyph from a mark or a
//! default ignorable by its own properties, and reads the category of
//! the other glyphs off the first character of their cluster.

use alloc::vec::Vec;

use super::arabic_joining::{STCH_FIXED, STCH_REPEATING};
use crate::buffer::{ClusterLevel, Glyph};
use crate::unicode::general_category::{general_category_class, GeneralCategoryClass};

/// HarfBuzz's `STCH_MAX_GLYPHS`: the most glyphs one stretch takes.
const MAX_GLYPHS: usize = 256;

/// True for a stretch tile.
fn is_tile(g: &Glyph) -> bool {
    matches!(g.indic_position, STCH_FIXED | STCH_REPEATING)
}

/// Whether glyph `g`, whose cluster starts with `first`, belongs to the
/// word a stretch fills: a mark, a default ignorable, or a character of
/// `HB_ARABIC_GENERAL_CATEGORY_IS_WORD`, as far as sigilbuzz knows the
/// categories (see the module docs).
fn is_word(g: &Glyph, first: Option<char>) -> bool {
    let props = crate::buffer::unicode_prop::DEFAULT_IGNORABLE;
    if g.char_class & crate::buffer::char_class::MARK != 0 || g.unicode_props & props != 0 {
        return true;
    }
    first.is_some_and(is_word_char)
}

/// `HB_ARABIC_GENERAL_CATEGORY_IS_WORD` for `ch`, as far as sigilbuzz
/// knows the categories: the marks, the numbers, the symbols (`Sc`,
/// `Sk`, `Sm`, `So`), the letters without case (`Lm`, `Lo`), and the
/// unassigned and private-use code points.
fn is_word_char(ch: char) -> bool {
    if crate::unicode::is_default_ignorable(ch) || ch.is_numeric() {
        return true;
    }
    match general_category_class(ch) {
        Some(
            GeneralCategoryClass::Mark
            | GeneralCategoryClass::DecimalNumber
            | GeneralCategoryClass::Symbol,
        ) => true,
        // Lm and Lo: the letters without case.
        Some(GeneralCategoryClass::Letter) => !ch.is_uppercase() && !ch.is_lowercase(),
        // Unassigned and private-use code points have no script.
        _ => crate::unicode::script_code(ch) == *b"Zzzz",
    }
}

/// What the stretch needs besides the glyphs.
pub(super) struct Stretch<'a> {
    /// True for right-to-left text.
    pub(super) rtl: bool,
    /// The font's horizontal advance of a glyph id.
    pub(super) advance: &'a dyn Fn(u32) -> i32,
    /// The text whose byte offsets the clusters are.
    pub(super) text: &'a str,
    /// The buffer's cluster level.
    pub(super) level: ClusterLevel,
    /// The most glyphs the run may grow to.
    pub(super) max_len: usize,
}

/// One run of tiles: `start..end` in `glyphs`, the word before them
/// starting at `context`, and how often its repeating tiles repeat.
struct Run {
    context: usize,
    start: usize,
    end: usize,
    copies: usize,
    overlap: i32,
    remaining: i32,
}

/// Measures the run of tiles that ends at `end`.
fn measure(glyphs: &[Glyph], end: usize, cx: &Stretch<'_>) -> Run {
    let (mut w_fixed, mut w_repeating) = (0i32, 0i32);
    let (mut n_fixed, mut n_repeating) = (0usize, 0usize);
    let mut start = end;
    while let Some(g) = start.checked_sub(1).and_then(|i| glyphs.get(i)) {
        if !is_tile(g) {
            break;
        }
        start -= 1;
        let width = (cx.advance)(g.glyph_id);
        if g.indic_position == STCH_FIXED {
            w_fixed = w_fixed.saturating_add(width);
            n_fixed += 1;
        } else {
            w_repeating = w_repeating.saturating_add(width);
            n_repeating += 1;
        }
    }
    let mut w_total = 0i32;
    let mut context = start;
    while let Some(g) = context.checked_sub(1).and_then(|i| glyphs.get(i)) {
        let first = usize::try_from(g.cluster)
            .ok()
            .and_then(|c| cx.text.get(c..))
            .and_then(|t| t.chars().next());
        if is_tile(g) || !is_word(g, first) {
            break;
        }
        context -= 1;
        w_total = w_total.saturating_add(g.x_advance);
    }
    let remaining_signed = i64::from(w_total) - i64::from(w_fixed);
    let repeating = i64::from(w_repeating);
    let mut copies: i64 = 0;
    if remaining_signed > repeating && repeating > 0 {
        copies = remaining_signed / repeating - 1;
    }
    let mut remaining = w_total.saturating_sub(w_fixed);
    let mut overlap = 0i32;
    let shortfall = remaining_signed - repeating * (copies + 1);
    if shortfall > 0 && n_repeating > 0 {
        copies += 1;
        let excess = (copies + 1) * repeating - remaining_signed;
        if excess > 0 {
            let divisor = copies * n_repeating as i64;
            let fit = excess.checked_div(divisor).unwrap_or(0);
            overlap = fit.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
            remaining = 0;
        }
    }
    let max_copies = match n_repeating {
        0 => 0,
        n => MAX_GLYPHS.saturating_sub(n_fixed + n) / n,
    };
    let copies = usize::try_from(copies).unwrap_or(0).min(max_copies);
    Run {
        context,
        start,
        end,
        copies,
        overlap,
        remaining,
    }
}

/// Stretches every run of tiles in `glyphs`, which are in visual order,
/// and clears the tile codes. Each run grows by its repeated tiles,
/// unless the whole text would grow past `max_len`, in which case
/// nothing stretches, as in HarfBuzz.
pub(super) fn apply_stch(glyphs: &mut Vec<Glyph>, cx: &Stretch<'_>) {
    if !glyphs.iter().any(is_tile) {
        return;
    }
    if !cx.rtl {
        glyphs.reverse();
    }
    // Measure every run, from the end of the glyphs back, as HarfBuzz's
    // first pass does.
    let mut runs: Vec<Run> = Vec::new();
    let mut extra = 0usize;
    let mut i = glyphs.len();
    while i > 0 {
        if glyphs.get(i - 1).is_some_and(is_tile) {
            let run = measure(glyphs, i, cx);
            extra = extra.saturating_add(
                run.copies.saturating_mul(
                    glyphs[run.start..run.end]
                        .iter()
                        .filter(|g| g.indic_position == STCH_REPEATING)
                        .count(),
                ),
            );
            i = run.start;
            runs.push(run);
        } else {
            i -= 1;
        }
    }
    if glyphs.len().saturating_add(extra) <= cx.max_len {
        cut(glyphs, &runs, cx, extra);
    }
    for g in glyphs.iter_mut() {
        g.indic_position = 0;
    }
    if !cx.rtl {
        glyphs.reverse();
    }
}

/// HarfBuzz's second pass: rebuilds `glyphs` with each run's tiles
/// repeated and offset across its word. `runs` go from the end of the
/// glyphs back.
fn cut(glyphs: &mut Vec<Glyph>, runs: &[Run], cx: &Stretch<'_>, extra: usize) {
    for run in runs {
        super::glyph_flags::unsafe_to_break(glyphs, run.context, run.end, cx.level);
    }
    // Built back to front, then reversed.
    let mut out: Vec<Glyph> = Vec::with_capacity(glyphs.len() + extra);
    let mut runs = runs.iter().peekable();
    let mut i = glyphs.len();
    while i > 0 {
        let Some(run) = runs.next_if(|r| r.end == i) else {
            i -= 1;
            if let Some(&g) = glyphs.get(i) {
                out.push(g);
            }
            continue;
        };
        let mut x_offset = run.remaining / 2;
        for k in (run.start..run.end).rev() {
            let Some(&tile) = glyphs.get(k) else {
                continue;
            };
            let width = (cx.advance)(tile.glyph_id);
            let repeat = if tile.indic_position == STCH_REPEATING {
                1 + run.copies
            } else {
                1
            };
            let mut g = tile;
            g.x_advance = 0;
            for n in 0..repeat {
                if cx.rtl {
                    x_offset = x_offset.saturating_sub(width);
                    if n > 0 {
                        x_offset = x_offset.saturating_add(run.overlap);
                    }
                }
                g.x_offset = x_offset;
                out.push(g);
                if !cx.rtl {
                    x_offset = x_offset.saturating_add(width);
                    if n > 0 {
                        x_offset = x_offset.saturating_sub(run.overlap);
                    }
                }
            }
        }
        i = run.start;
    }
    out.reverse();
    *glyphs = out;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tile(id: u32, code: u8, cluster: u32) -> Glyph {
        let mut g = Glyph::new(id, cluster);
        g.indic_position = code;
        g
    }

    fn letter(cluster: u32, advance: i32) -> Glyph {
        let mut g = Glyph::new(50, cluster);
        g.x_advance = advance;
        g
    }

    #[test]
    fn repeating_tiles_fill_the_rest_of_the_word() {
        // Visual right-to-left order: two letters (the word after the
        // mark, logically), then the tiles of the mark at cluster 0.
        // Text: the mark, then two Syriac letters (beth, gamal).
        let text = "\u{070F}\u{0712}\u{0713}";
        let mut glyphs = alloc::vec![
            letter(4, 600),
            letter(2, 600),
            tile(1, STCH_FIXED, 0),
            tile(2, STCH_REPEATING, 0),
            tile(3, STCH_FIXED, 0),
        ];
        let advance = |id: u32| if id == 2 { 200 } else { 100 };
        let cx = Stretch {
            rtl: true,
            advance: &advance,
            text,
            level: ClusterLevel::MonotoneGraphemes,
            max_len: 1000,
        };
        apply_stch(&mut glyphs, &cx);
        // 1200 to fill, 200 fixed: the 200-wide tile repeats 5 times,
        // and the tiles start half the leftover width (500) in.
        let ids: Vec<u32> = glyphs.iter().map(|g| g.glyph_id).collect();
        assert_eq!(ids, [50, 50, 1, 2, 2, 2, 2, 2, 3]);
        assert!(glyphs[2..].iter().all(|g| g.x_advance == 0));
        assert!(glyphs.iter().all(|g| g.indic_position == 0));
        let offsets: Vec<i32> = glyphs[2..].iter().map(|g| g.x_offset).collect();
        assert_eq!(offsets, [-700, -600, -400, -200, 0, 200, 400]);
    }

    #[test]
    fn symbols_count_into_the_word() {
        // The mark, beth, then `middle`, then gamal, in visual
        // right-to-left order. HarfBuzz counts a symbol (Sc, Sk, Sm,
        // So) into the word the tiles fill, and a punctuation mark
        // ends it.
        let stretched = |middle: char| {
            let text: alloc::string::String = ['\u{070F}', '\u{0712}', middle, '\u{0713}']
                .iter()
                .collect();
            let gamal = u32::try_from(4 + middle.len_utf8()).unwrap();
            let mut glyphs = alloc::vec![
                letter(gamal, 600),
                letter(4, 600),
                letter(2, 600),
                tile(1, STCH_FIXED, 0),
                tile(2, STCH_REPEATING, 0),
                tile(3, STCH_FIXED, 0),
            ];
            let advance = |id: u32| if id == 2 { 200 } else { 100 };
            let cx = Stretch {
                rtl: true,
                advance: &advance,
                text: &text,
                level: ClusterLevel::MonotoneGraphemes,
                max_len: 1000,
            };
            apply_stch(&mut glyphs, &cx);
            glyphs.iter().filter(|g| g.glyph_id == 2).count()
        };
        // 1800 to fill, 200 fixed: the 200-wide tile repeats 8 times.
        for symbol in ['+', '\u{20AC}', '^', '\u{00A9}'] {
            assert_eq!(stretched(symbol), 8, "{symbol:?}");
        }
        // The word is beth alone: 600 to fill, so 2 tiles.
        assert_eq!(stretched('!'), 2);
        assert_eq!(stretched('A'), 2);
    }

    #[test]
    fn nothing_stretches_past_the_length_limit() {
        let text = "\u{070F}\u{0712}";
        let mut glyphs = alloc::vec![letter(2, 2000), tile(2, STCH_REPEATING, 0)];
        let advance = |_: u32| 100;
        let cx = Stretch {
            rtl: true,
            advance: &advance,
            text,
            level: ClusterLevel::MonotoneGraphemes,
            max_len: 4,
        };
        apply_stch(&mut glyphs, &cx);
        assert_eq!(glyphs.len(), 2);
        assert_eq!(glyphs[1].indic_position, 0);
    }
}
