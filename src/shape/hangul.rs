//! Hangul jamo composition, run over the text before the cmap lookup.

/// Pre-iterates `text` and applies Hangul NFC jamo composition in a
/// single pass: a Leading jamo (L) followed by a Vowel jamo (V) and
/// optionally a Trailing jamo (T) collapses into the matching
/// precomposed syllable in U+AC00..U+D7A3 *when* the font carries a
/// cmap entry for the precomposed codepoint. HarfBuzz / rustybuzz do
/// exactly this, so matching the behavior is mandatory for byte-
/// parity on modern Korean corpora.
///
/// Extended-B trailing jamo (U+D7CB..U+D7FB) abort composition of
/// the whole L+V+T triple so the font's `ljmo` / `vjmo` / `tjmo`
/// features can shape each jamo on its own (matches rustybuzz).
///
/// Returns a vector of `(byte_offset, char)` pairs that replaces the
/// normal `text.char_indices()` sequence in the main shaping loop.
/// The byte offset is the offset of the FIRST codepoint in the
/// composed cluster (the L for an L+V / L+V+T composition), so
/// downstream cluster tracking still maps glyphs back to the
/// original UTF-8 stream.
pub(super) fn hangul_compose(
    text: &str,
    cmap: &crate::tables::cmap::Cmap<'_>,
) -> alloc::vec::Vec<(u32, char)> {
    let mut out = alloc::vec::Vec::with_capacity(text.len());
    // Gate: HarfBuzz / rustybuzz select the Hangul shaper on a
    // per-run basis and the Hangul preprocessor (the NFC compose)
    // runs only when that shaper is active. sigilbuzz's segmenter
    // splits scripts but the Hangul preprocessor still has to see
    // the segment's L+V(+T) window to run, so we gate on "the
    // buffer is Hangul / whitespace / default-ignorable only".
    // Mixed-script buffers (e.g. "Hi " + jamo) bypass composition;
    // the jamo runs through its own segment under `hang` but stays
    // as L + V glyphs, matching rustybuzz.
    let compose_enabled = text.chars().all(|c| {
        let cp = c as u32;
        matches!(crate::unicode::script_of(c), crate::unicode::Script::Hangul)
            || c == ' '
            || (0x200B..=0x200D).contains(&cp)
            || cp == 0xFEFF
    });
    let mut it = text.char_indices().peekable();
    while let Some((byte_offset, ch)) = it.next() {
        if !compose_enabled {
            out.push((byte_offset as u32, ch));
            continue;
        }
        // L jamo range: U+1100..U+1112 (the 19 modern leading
        // consonants). Extended-A (U+A960..) do NOT compose: they
        // stay as jamo so `ljmo` picks them up.
        let l_index = if (0x1100..=0x1112).contains(&(ch as u32)) {
            Some((ch as u32) - 0x1100)
        } else {
            None
        };
        if let Some(l) = l_index {
            if let Some(&(_, next_ch)) = it.peek() {
                // V jamo range: U+1161..U+1175 (21 modern vowels).
                if (0x1161..=0x1175).contains(&(next_ch as u32)) {
                    let v = (next_ch as u32) - 0x1161;
                    // Peek past V to detect the trailing jamo, if any.
                    // HarfBuzz's rule: only compose when the whole
                    // run is in the modern range. Extended-B T
                    // (U+D7CB..U+D7FB) aborts composition entirely.
                    let mut clone = it.clone();
                    clone.next(); // skip V
                    let t_info = match clone.peek() {
                        Some(&(_, c)) if (0x11A8..=0x11C2).contains(&(c as u32)) => {
                            Some(Some((c as u32) - 0x11A7))
                        }
                        Some(&(_, c)) if (0xD7CB..=0xD7FB).contains(&(c as u32)) => Some(None),
                        _ => None,
                    };
                    if matches!(t_info, Some(None)) {
                        // Extended-B T blocks composition; emit each
                        // jamo as-is. L and V are consumed here; the
                        // T gets emitted naturally on the next
                        // iteration.
                        out.push((byte_offset as u32, ch));
                        out.push((byte_offset as u32, next_ch));
                        it.next(); // consume V
                        continue;
                    }
                    let t = t_info.and_then(|o| o).unwrap_or(0);
                    it.next(); // consume V
                    if t != 0 {
                        it.next(); // consume modern T
                    }
                    let syllable_cp = 0xAC00 + (l * 21 + v) * 28 + t;
                    if let Some(ch_composed) = core::char::from_u32(syllable_cp) {
                        if cmap.glyph_id(ch_composed).is_some() {
                            out.push((byte_offset as u32, ch_composed));
                            continue;
                        }
                    }
                    // Fallback: emit each jamo as-is.
                    out.push((byte_offset as u32, ch));
                    let v_ch = core::char::from_u32(0x1161 + v).unwrap_or(ch);
                    out.push((byte_offset as u32, v_ch));
                    if t != 0 {
                        let t_ch = core::char::from_u32(0x11A7 + t).unwrap_or(ch);
                        out.push((byte_offset as u32, t_ch));
                    }
                    continue;
                }
            }
        }
        out.push((byte_offset as u32, ch));
    }
    out
}
