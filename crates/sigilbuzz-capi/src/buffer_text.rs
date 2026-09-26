//! Text ingest, cluster mapping, and segment properties for
//! `hb_buffer_t`.
//!
//! HarfBuzz reports a glyph's cluster as the index, in the caller's
//! own code units, of the character it came from: UTF-8 bytes for
//! `hb_buffer_add_utf8`, UTF-16 code units for `hb_buffer_add_utf16`,
//! counted from the start of the array passed to that call (so
//! `item_offset` is included). The core [`Buffer`] stores UTF-8 and
//! reports byte offsets into its own text, so every add records, per
//! character, the cluster HarfBuzz would report; `hb_shape_full` maps
//! the core clusters back through that table.
//!
//! The add functions also fill the buffer's pre- and post-context from
//! the text around the item, as HarfBuzz does, and decode malformed
//! input the way `hb-utf.hh` does: each malformed sequence becomes
//! U+FFFD and consumes one code unit.

use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::CStr;

use sigilbuzz::{Buffer, Direction, Language, UnicodeScript};

use crate::{
    hb_direction_t, hb_language_t, hb_script_t, lang_und, BufferState, HB_DIRECTION_BTT,
    HB_DIRECTION_INVALID, HB_DIRECTION_LTR, HB_DIRECTION_RTL, HB_DIRECTION_TTB, HB_SCRIPT_INVALID,
};

/// HarfBuzz's default replacement character for malformed input.
const REPLACEMENT: char = '\u{FFFD}';

/// Per-character cluster table for the text added so far.
#[derive(Debug, Default)]
pub(crate) struct ClusterTable {
    /// `(byte offset of a character in the core buffer text, cluster
    /// HarfBuzz reports for it)`, in text order.
    entries: Vec<(u32, u32)>,
    /// Length of the core text the table describes. A mismatch means
    /// the text changed behind the table's back; mapping then falls
    /// back to the core byte offsets.
    text_len: usize,
}

impl ClusterTable {
    /// The cluster to report for a glyph whose core cluster is
    /// `core` (a byte offset into a text of `text_len` bytes).
    pub(crate) fn map(&self, core: u32, text_len: usize) -> u32 {
        if self.text_len != text_len {
            return core;
        }
        self.entries
            .binary_search_by_key(&core, |&(offset, _)| offset)
            .map_or(core, |i| self.entries[i].1)
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.text_len = 0;
    }
}

/// A code-unit encoding accepted by `hb_buffer_add_*`, decoded exactly
/// like HarfBuzz's `hb_utf8_t` / `hb_utf16_t`.
pub(crate) trait Encoding {
    /// One code unit.
    type Unit: Copy;

    /// Decodes the character starting at `pos`, reading no further than
    /// `end`. Returns it and the position after it.
    fn next(text: &[Self::Unit], pos: usize, end: usize) -> (char, usize);

    /// Decodes the character ending just before `pos`, reading no
    /// further back than `start`. Returns it and its first position.
    fn prev(text: &[Self::Unit], start: usize, pos: usize) -> (char, usize);
}

/// UTF-8 code units.
pub(crate) enum Utf8 {}

impl Encoding for Utf8 {
    type Unit = u8;

    fn next(text: &[u8], pos: usize, end: usize) -> (char, usize) {
        let lead = u32::from(text[pos]);
        let after = pos + 1;
        if lead <= 0x7F {
            return (char::from(text[pos]), after);
        }
        let cont = |i: usize| -> Option<u32> {
            let b = u32::from(*text.get(i).filter(|_| i < end)?).wrapping_sub(0x80);
            (b <= 0x3F).then_some(b)
        };
        let decoded = match lead {
            0xC2..=0xDF => cont(after).map(|t1| (((lead & 0x1F) << 6) | t1, 1)),
            0xE0..=0xEF => cont(after).zip(cont(after + 1)).and_then(|(t1, t2)| {
                let c = ((lead & 0x0F) << 12) | (t1 << 6) | t2;
                (c >= 0x800 && !(0xD800..=0xDFFF).contains(&c)).then_some((c, 2))
            }),
            0xF0..=0xF4 => cont(after)
                .zip(cont(after + 1))
                .zip(cont(after + 2))
                .and_then(|((t1, t2), t3)| {
                    let c = ((lead & 0x07) << 18) | (t1 << 12) | (t2 << 6) | t3;
                    (0x10000..=0x10FFFF).contains(&c).then_some((c, 3))
                }),
            _ => None,
        };
        match decoded.and_then(|(c, n)| char::from_u32(c).map(|ch| (ch, n))) {
            Some((ch, n)) => (ch, after + n),
            None => (REPLACEMENT, after),
        }
    }

    fn prev(text: &[u8], start: usize, pos: usize) -> (char, usize) {
        let end = pos;
        let mut at = pos - 1;
        while start < at && (text[at] & 0xC0) == 0x80 && end - at < 4 {
            at -= 1;
        }
        match Self::next(text, at, end) {
            (ch, after) if after == end => (ch, at),
            _ => (REPLACEMENT, end - 1),
        }
    }
}

/// UTF-16 code units.
pub(crate) enum Utf16 {}

impl Utf16 {
    fn combine(high: u32, low: u32) -> char {
        char::from_u32((high << 10) + low - ((0xD800 << 10) - 0x10000 + 0xDC00))
            .unwrap_or(REPLACEMENT)
    }
}

impl Encoding for Utf16 {
    type Unit = u16;

    fn next(text: &[u16], pos: usize, end: usize) -> (char, usize) {
        let c = u32::from(text[pos]);
        let after = pos + 1;
        if !(0xD800..=0xDFFF).contains(&c) {
            return (char::from_u32(c).unwrap_or(REPLACEMENT), after);
        }
        if c <= 0xDBFF && after < end {
            let low = u32::from(text[after]);
            if (0xDC00..=0xDFFF).contains(&low) {
                return (Self::combine(c, low), after + 1);
            }
        }
        (REPLACEMENT, after)
    }

    fn prev(text: &[u16], start: usize, pos: usize) -> (char, usize) {
        let at = pos - 1;
        let c = u32::from(text[at]);
        if !(0xD800..=0xDFFF).contains(&c) {
            return (char::from_u32(c).unwrap_or(REPLACEMENT), at);
        }
        if c >= 0xDC00 && start < at {
            let high = u32::from(text[at - 1]);
            if (0xD800..=0xDBFF).contains(&high) {
                return (Self::combine(high, c), at - 1);
            }
        }
        (REPLACEMENT, at)
    }
}

fn to_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// How far an `hb_buffer_add_*` item extends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ItemLength {
    /// `-1`: to the end of the text.
    ToEnd,
    /// An explicit number of code units.
    Units(usize),
}

impl ItemLength {
    /// Interprets the C argument. Negative values other than `-1` are
    /// rejected (`None`), as in HarfBuzz.
    pub(crate) fn from_c(raw: core::ffi::c_int) -> Option<Self> {
        if raw == -1 {
            Some(Self::ToEnd)
        } else {
            usize::try_from(raw).ok().map(Self::Units)
        }
    }
}

/// Adds `text[item_offset..item_offset + item_length]` to the buffer,
/// following HarfBuzz's `hb_buffer_add_utf`:
///
/// - When the buffer is empty and `item_offset > 0`, the (up to five)
///   characters before the item become the pre-context.
/// - Each character's cluster is its offset in `text`, in code units.
/// - The (up to five) characters after the item become the
///   post-context, replacing any earlier one.
///
/// An item that starts past the end of `text` adds nothing; one that
/// runs past it is clamped.
pub(crate) fn add<E: Encoding>(
    state: &mut BufferState,
    text: &[E::Unit],
    item_offset: usize,
    item_length: ItemLength,
) {
    let start = item_offset;
    if start > text.len() {
        return;
    }
    let end = match item_length {
        ItemLength::ToEnd => text.len(),
        ItemLength::Units(len) => start.saturating_add(len).min(text.len()),
    };
    if state.buffer.is_empty() {
        state.clusters.clear();
        if start > 0 {
            let mut before: Vec<char> = Vec::with_capacity(Buffer::CONTEXT_LENGTH);
            let mut at = start;
            while at > 0 && before.len() < Buffer::CONTEXT_LENGTH {
                let (ch, prev) = E::prev(text, 0, at);
                before.push(ch);
                at = prev;
            }
            let pre: String = before.iter().rev().collect();
            state.buffer.set_pre_context(&pre);
        }
    }

    let mut item = String::with_capacity(end - start);
    let base = state.buffer.text().len();
    let mut at = start;
    while at < end {
        let (ch, after) = E::next(text, at, end);
        state
            .clusters
            .entries
            .push((to_u32(base + item.len()), to_u32(at)));
        item.push(ch);
        at = after;
    }
    state.buffer.push_str(&item);
    state.clusters.text_len = state.buffer.text().len();

    let mut post = String::new();
    let mut count = 0;
    while at < text.len() && count < Buffer::CONTEXT_LENGTH {
        let (ch, after) = E::next(text, at, text.len());
        post.push(ch);
        at = after;
        count += 1;
    }
    state.buffer.set_post_context(&post);
}

/// `hb_buffer_clear_contents`: HarfBuzz's `clear()` drops the text,
/// the output, the segment properties (direction, script, language),
/// and the context.
pub(crate) fn clear_contents(state: &mut BufferState) {
    state.buffer.clear();
    state.clusters.clear();
    state.direction = HB_DIRECTION_INVALID;
    state.script = HB_SCRIPT_INVALID;
    state.language = core::ptr::null();
    state.glyph_infos.clear();
    state.glyph_positions.clear();
    state.props_set = false;
}

/// The core script for an `hb_script_t`: a script sigilbuzz has a
/// bucket for shapes the whole buffer as that script. Anything else
/// (invalid, Common, Inherited, Unknown, or a script without a bucket)
/// leaves the core free to split the text into script runs.
pub(crate) fn core_script(script: hb_script_t) -> Option<UnicodeScript> {
    if script == HB_SCRIPT_INVALID {
        return None;
    }
    UnicodeScript::from_iso15924_tag(script.to_be_bytes())
}

/// The core language for an `hb_language_t` (a NUL-terminated tag
/// string, or null for none).
///
/// # Safety
///
/// `language` must be null or point to a NUL-terminated string, as
/// returned by `hb_language_from_string`.
pub(crate) unsafe fn core_language(language: hb_language_t) -> Option<Language> {
    if language.is_null() {
        return None;
    }
    // SAFETY: the caller guarantees a NUL-terminated string.
    let tag = unsafe { CStr::from_ptr(language) };
    tag.to_str().ok().and_then(Language::new)
}

fn direction_out(direction: Direction) -> hb_direction_t {
    match direction {
        Direction::Ltr => HB_DIRECTION_LTR,
        Direction::Rtl => HB_DIRECTION_RTL,
        Direction::Ttb => HB_DIRECTION_TTB,
        Direction::Btt => HB_DIRECTION_BTT,
    }
}

/// `hb_buffer_guess_segment_properties`, in HarfBuzz's order: the
/// script from the first character that has one, then the direction
/// from the script (right to left for Arabic, Hebrew, and the other
/// RTL scripts, left to right when the script has no preference),
/// then the language.
pub(crate) fn guess_segment_properties(state: &mut BufferState) {
    if state.script == HB_SCRIPT_INVALID {
        // script_runs() folds leading digits and punctuation into the
        // first real script, so its first run is HarfBuzz's guess.
        // Text with no script-bearing character keeps an invalid
        // script, as in HarfBuzz.
        let first = state.buffer.script_runs().first().map(|run| run.script);
        if let Some(tag) = first.and_then(UnicodeScript::iso15924_tag) {
            state.script = u32::from_be_bytes(tag);
            state.buffer.set_script(core_script(state.script));
        }
    }
    if state.direction == HB_DIRECTION_INVALID {
        let direction =
            Direction::horizontal_for_script(state.script.to_be_bytes()).unwrap_or(Direction::Ltr);
        state.direction = direction_out(direction);
        state.buffer.set_direction(direction);
    }
    if state.language.is_null() {
        // HarfBuzz uses the process locale here; pick "und" as a safe
        // default that lookups always have to fall back through.
        state.language = lang_und();
        // SAFETY: lang_und() is a static NUL-terminated string.
        state
            .buffer
            .set_language(unsafe { core_language(state.language) });
    }
    state.props_set = true;
}

#[cfg(test)]
mod tests;
