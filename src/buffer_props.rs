//! Segment properties and surrounding context for [`Buffer`].
//!
//! HarfBuzz calls a buffer's direction, script, and language its
//! segment properties. The direction accessors live with the rest of
//! [`Buffer`]; this module adds the script and language, plus the
//! pre- and post-context: text around the buffer's run that is not
//! shaped but tells the shaper how the run connects to its
//! neighbors.

use crate::buffer::{Buffer, Direction};
use crate::language::Language;
use crate::unicode::Script;

impl Buffer {
    /// Characters of context kept on each side of the text. Matches
    /// HarfBuzz's `CONTEXT_LENGTH`.
    pub const CONTEXT_LENGTH: usize = 5;

    /// The script the whole buffer shapes as, or `None` when the
    /// shaper splits the text into script runs itself (the default).
    ///
    /// # Examples
    ///
    /// ```
    /// use sigilbuzz::{Buffer, UnicodeScript};
    ///
    /// let mut buffer = Buffer::new();
    /// assert_eq!(buffer.script(), None);
    /// buffer.set_script(Some(UnicodeScript::Arabic));
    /// assert_eq!(buffer.script(), Some(UnicodeScript::Arabic));
    /// ```
    #[must_use]
    pub const fn script(&self) -> Option<Script> {
        self.script
    }

    /// Sets the script for the whole buffer.
    ///
    /// With `Some(script)`, every character shapes as that script, the
    /// way HarfBuzz shapes one buffer with one script: the script's
    /// OpenType tags choose the GSUB and GPOS features, and its complex
    /// shaper (Arabic joining, Indic reordering, and so on) runs over
    /// all of the text. With `None` (the default), [`crate::shape`]
    /// splits the text into runs of one script each and shapes every
    /// run under its own script.
    ///
    /// # Examples
    ///
    /// ```
    /// use sigilbuzz::{Buffer, UnicodeScript};
    ///
    /// let mut buffer = Buffer::new();
    /// buffer.push_str("abc");
    /// buffer.set_script(Some(UnicodeScript::Latin));
    /// buffer.set_script(None);
    /// assert_eq!(buffer.script(), None);
    /// ```
    pub fn set_script(&mut self, script: Option<Script>) {
        self.script = script;
    }

    /// Fills in the segment properties the caller left unset, as
    /// HarfBuzz's `hb_buffer_guess_segment_properties` does. The script
    /// becomes that of the first character that is not Common or
    /// Inherited, and the direction becomes that script's horizontal
    /// direction ([`Script::horizontal_direction`]), or left to right
    /// when the text has no such character. Properties the caller set
    /// stay as they are. HarfBuzz also takes an unset language from the
    /// process locale. sigilbuzz leaves it unset, so the output does
    /// not depend on the environment.
    ///
    /// With the script set, [`crate::shape`] shapes the whole buffer
    /// with that script's shaper, as HarfBuzz does, even when the text
    /// mixes scripts. Code that calls `hb_buffer_guess_segment_properties`
    /// before `hb_shape` gets the same output by calling this before
    /// [`crate::shape`]. Without the call, [`crate::shape`] shapes each
    /// script run with its own shaper.
    ///
    /// # Examples
    ///
    /// ```
    /// use sigilbuzz::{Buffer, Direction, UnicodeScript};
    ///
    /// let mut buffer = Buffer::new();
    /// buffer.push_str("abc \u{05D0}\u{05D1}");
    /// buffer.guess_segment_properties();
    /// assert_eq!(buffer.script(), Some(UnicodeScript::Latin));
    /// assert_eq!(buffer.direction(), Direction::Ltr);
    ///
    /// let mut buffer = Buffer::new();
    /// buffer.push_str("12 \u{05D0}\u{05D1} abc");
    /// buffer.guess_segment_properties();
    /// assert_eq!(buffer.script(), Some(UnicodeScript::Hebrew));
    /// assert_eq!(buffer.direction(), Direction::Rtl);
    ///
    /// // A direction the caller chose stays.
    /// let mut buffer = Buffer::new();
    /// buffer.push_str("\u{05D0}");
    /// buffer.set_direction(Direction::Ltr);
    /// buffer.guess_segment_properties();
    /// assert_eq!(buffer.direction(), Direction::Ltr);
    /// ```
    pub fn guess_segment_properties(&mut self) {
        if self.script.is_none() {
            self.script = crate::shape::guess_script(self.text.chars());
        }
        if !self.direction_explicit {
            let direction = self
                .script
                .map_or(Direction::Ltr, Script::horizontal_direction);
            self.set_direction(direction);
        }
    }

    /// The language set with [`Self::set_language`].
    ///
    /// # Examples
    ///
    /// ```
    /// use sigilbuzz::{Buffer, Language};
    ///
    /// let mut buffer = Buffer::new();
    /// buffer.set_language(Language::new("tr"));
    /// assert_eq!(buffer.language().map(Language::as_str), Some("tr"));
    /// ```
    #[must_use]
    pub const fn language(&self) -> Option<&Language> {
        self.language.as_ref()
    }

    /// Sets the language of the text.
    ///
    /// The language picks the OpenType language system: for each
    /// script tag the shaper tries, it looks for a language system
    /// matching [`Language::ot_language_tags`] in order, then falls
    /// back to the script's default. That is how a font's Turkish,
    /// Serbian, or Urdu `locl` forms get selected. `None` (the default)
    /// always uses the default language system.
    ///
    /// # Examples
    ///
    /// ```
    /// use sigilbuzz::{Buffer, Language};
    ///
    /// let mut buffer = Buffer::new();
    /// buffer.set_language(Language::new("sr-Cyrl"));
    /// assert_eq!(buffer.language().map(Language::ot_language_tags), Some(&[*b"SRB "][..]));
    /// ```
    pub fn set_language(&mut self, language: Option<Language>) {
        self.language = language;
    }

    /// The pre-context: up to [`Self::CONTEXT_LENGTH`] characters that
    /// come before the buffer's text in the source.
    ///
    /// # Examples
    ///
    /// ```
    /// use sigilbuzz::Buffer;
    ///
    /// let mut buffer = Buffer::new();
    /// buffer.set_pre_context("abcdefg");
    /// assert_eq!(buffer.pre_context(), "cdefg");
    /// ```
    #[must_use]
    pub fn pre_context(&self) -> &str {
        &self.pre_context
    }

    /// Records the text that precedes the buffer's run in the source,
    /// keeping its last [`Self::CONTEXT_LENGTH`] characters.
    ///
    /// Context is never shaped and never produces glyphs. Cursive
    /// scripts consult it so a run that starts in the middle of a word
    /// still gets the right joining form: an Arabic letter whose
    /// pre-context ends in a dual-joining letter takes its final or
    /// medial form. GSUB lookups themselves do not see context
    /// characters, the same as in HarfBuzz. Text mutators such as
    /// [`Self::push_str`] leave the context alone; [`Self::clear`]
    /// resets it.
    ///
    /// # Examples
    ///
    /// ```
    /// use sigilbuzz::Buffer;
    ///
    /// let mut buffer = Buffer::new();
    /// buffer.set_pre_context("\u{0628}");
    /// buffer.push_str("\u{0628}");
    /// assert_eq!(buffer.pre_context(), "\u{0628}");
    /// ```
    pub fn set_pre_context(&mut self, text: &str) {
        let start = text
            .char_indices()
            .rev()
            .nth(Self::CONTEXT_LENGTH - 1)
            .map_or(0, |(i, _)| i);
        self.pre_context.clear();
        self.pre_context.push_str(&text[start..]);
    }

    /// The post-context: up to [`Self::CONTEXT_LENGTH`] characters
    /// that follow the buffer's text in the source.
    ///
    /// # Examples
    ///
    /// ```
    /// use sigilbuzz::Buffer;
    ///
    /// let mut buffer = Buffer::new();
    /// buffer.set_post_context("abcdefg");
    /// assert_eq!(buffer.post_context(), "abcde");
    /// ```
    #[must_use]
    pub fn post_context(&self) -> &str {
        &self.post_context
    }

    /// Records the text that follows the buffer's run in the source,
    /// keeping its first [`Self::CONTEXT_LENGTH`] characters. See
    /// [`Self::set_pre_context`] for how context is used.
    ///
    /// # Examples
    ///
    /// ```
    /// use sigilbuzz::Buffer;
    ///
    /// let mut buffer = Buffer::new();
    /// buffer.set_post_context("\u{0628}\u{0627}");
    /// assert_eq!(buffer.post_context(), "\u{0628}\u{0627}");
    /// ```
    pub fn set_post_context(&mut self, text: &str) {
        let end = text
            .char_indices()
            .nth(Self::CONTEXT_LENGTH)
            .map_or(text.len(), |(i, _)| i);
        self.post_context.clear();
        self.post_context.push_str(&text[..end]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_unset() {
        let b = Buffer::new();
        assert_eq!(b.script(), None);
        assert!(b.language().is_none());
        assert_eq!(b.pre_context(), "");
        assert_eq!(b.post_context(), "");
    }

    #[test]
    fn pre_context_keeps_the_last_five_chars() {
        let mut b = Buffer::new();
        b.set_pre_context("\u{0627}\u{0644}\u{0639}\u{0631}\u{0628}\u{064A}\u{0629}");
        assert_eq!(b.pre_context(), "\u{0639}\u{0631}\u{0628}\u{064A}\u{0629}");
        b.set_pre_context("ab");
        assert_eq!(b.pre_context(), "ab");
        b.set_pre_context("");
        assert_eq!(b.pre_context(), "");
    }

    #[test]
    fn post_context_keeps_the_first_five_chars() {
        let mut b = Buffer::new();
        b.set_post_context("\u{0627}\u{0644}\u{0639}\u{0631}\u{0628}\u{064A}\u{0629}");
        assert_eq!(b.post_context(), "\u{0627}\u{0644}\u{0639}\u{0631}\u{0628}");
        b.set_post_context("xyz");
        assert_eq!(b.post_context(), "xyz");
    }

    #[test]
    fn context_counts_chars_not_bytes() {
        let mut b = Buffer::new();
        // Five four-byte characters plus one more.
        b.set_pre_context("\u{1F600}\u{1F601}\u{1F602}\u{1F603}\u{1F604}\u{1F605}");
        assert_eq!(b.pre_context().chars().count(), 5);
        assert!(b.pre_context().starts_with('\u{1F601}'));
        b.set_post_context("\u{1F600}\u{1F601}\u{1F602}\u{1F603}\u{1F604}\u{1F605}");
        assert!(b.post_context().ends_with('\u{1F604}'));
    }

    #[test]
    fn text_mutators_keep_context_and_properties() {
        let mut b = Buffer::new();
        b.set_script(Some(Script::Arabic));
        b.set_language(Language::new("ur"));
        b.set_pre_context("a");
        b.set_post_context("z");
        b.push_str("x");
        b.set_text("y");
        b.set_direction(crate::Direction::Rtl);
        assert_eq!(b.script(), Some(Script::Arabic));
        assert_eq!(b.language().map(Language::as_str), Some("ur"));
        assert_eq!(b.pre_context(), "a");
        assert_eq!(b.post_context(), "z");
    }

    #[test]
    fn clear_resets_script_language_and_context() {
        let mut b = Buffer::new();
        b.set_script(Some(Script::Hebrew));
        b.set_language(Language::new("he"));
        b.set_pre_context("a");
        b.set_post_context("z");
        b.push_str("x");
        b.clear();
        assert_eq!(b.script(), None);
        assert!(b.language().is_none());
        assert_eq!(b.pre_context(), "");
        assert_eq!(b.post_context(), "");
    }
}
