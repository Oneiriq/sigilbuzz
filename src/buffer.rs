//! Shaping input and output types.
//!
//! A [`Buffer`] is fed one text run at a time, then passed to
//! [`crate::shape`] along with a [`crate::Font`]. On a successful
//! shape it yields a vector of [`Glyph`]s, each carrying the glyph
//! index the renderer should emit plus the position of that glyph
//! relative to the pen.
//!
//! The API mirrors `HarfBuzz`'s `hb_buffer_t`, so a
//! consumer who already knows `HarfBuzz` can reach for sigilbuzz without
//! relearning concepts.

use alloc::string::String;
use alloc::vec::Vec;
use core::ops::Range;

use crate::unicode::{is_common_or_inherited, is_hangul_tone_mark, script_of, Script};

pub mod char_class;
mod flags;
mod glyph_flags;
pub use flags::{BufferFlags, ClusterLevel};
pub use glyph_flags::GlyphFlags;

/// Writing direction of a text run.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Horizontal, left-to-right. Default for Latin, CJK in modern use.
    #[default]
    Ltr,
    /// Horizontal, right-to-left. Arabic, Hebrew.
    Rtl,
    /// Vertical, top-to-bottom. Traditional CJK.
    Ttb,
    /// Vertical, bottom-to-top. Rare, used for some Mongolian display.
    Btt,
}

impl Direction {
    /// True for horizontal directions.
    #[must_use]
    pub const fn is_horizontal(self) -> bool {
        matches!(self, Self::Ltr | Self::Rtl)
    }

    /// True for directions that advance "forward" in natural order.
    #[must_use]
    pub const fn is_forward(self) -> bool {
        matches!(self, Self::Ltr | Self::Ttb)
    }
}

/// One positioned glyph in the shaped output.
///
/// Positions are in font-design units that have been scaled by the
/// font's size. Advances and offsets are signed because shaping can
/// produce negative displacements (contextual kerning, backtracking
/// combining marks).
///
/// In addition to the rendered fields, `Glyph` carries shaper-internal
/// scratch fields (`unicode_props`, `indic_position`, `char_class`,
/// `combining_class`, and `syllable`) that the shaping stages use to track per-glyph
/// state across GSUB passes. Renderers and
/// most callers can ignore them; they are public so the shaper
/// modules inside this crate can round-trip state through `Vec<Glyph>`
/// without stashing a parallel array. Stable bits of `unicode_props`
/// are set once during buffer preparation (default-ignorable,
/// joiner, ...). `indic_position` is a scratch byte that survives
/// ligature substitutions (the surviving glyph inherits the first
/// component's).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Glyph {
    /// Glyph index within the font. After shaping, this is the index
    /// in the SFNT glyph table, *not* a Unicode codepoint.
    pub glyph_id: u32,
    /// Cluster tag linking this glyph back to the input codepoints.
    /// Glyphs sharing a cluster came from characters the buffer's
    /// [`ClusterLevel`] groups (a ligature, a base and its marks, ...).
    pub cluster: u32,
    /// Horizontal advance applied after drawing this glyph.
    pub x_advance: i32,
    /// Vertical advance applied after drawing this glyph.
    pub y_advance: i32,
    /// Horizontal offset applied to the glyph origin before drawing.
    pub x_offset: i32,
    /// Vertical offset applied to the glyph origin before drawing.
    pub y_offset: i32,
    /// Shaper-internal Unicode property bits. Set once during buffer
    /// preparation and carried across GSUB so later passes can query
    /// "was this glyph's source a joiner / default-ignorable / ...?"
    /// without re-deriving from the cluster. See `unicode_prop`.
    pub unicode_props: u16,
    /// Shaper-internal byte the syllable-based shapers borrow while one
    /// of their GSUB stages runs, to keep each glyph's shaper state
    /// aligned with it through the substitutions. Zero otherwise.
    pub indic_position: u8,
    /// Shaper-internal `char_class` bits of the glyph's source
    /// character, set by normalization and carried through GSUB like
    /// [`Self::unicode_props`] (a ligature keeps its first component's).
    pub char_class: u8,
    /// Shaper-internal combining class of the glyph's source character
    /// when it is a mark: HarfBuzz's modified combining class (see
    /// `unicode::normalize::modified_combining_class`), as the mark
    /// reordering and fallback positioning adjust it. Zero for every
    /// other glyph.
    pub combining_class: u8,
    /// Shaper-internal syllable of the glyph, HarfBuzz's `syllable()`
    /// byte: the Indic, Khmer, Myanmar, and USE shapers number their
    /// syllables (a serial in the high four bits, the syllable type in
    /// the low four) and every glyph a syllable produces carries its
    /// number through GSUB. The features HarfBuzz registers with
    /// `F_PER_SYLLABLE` only match within one syllable. Zero for a
    /// glyph in no syllable.
    pub syllable: u8,
    /// Glyph flags, HarfBuzz's `hb_glyph_info_get_glyph_flags`: whether
    /// the text may be broken or joined at this glyph's cluster without
    /// reshaping (see [`GlyphFlags`]). Every glyph of a cluster carries
    /// the same flags.
    pub flags: GlyphFlags,
}

/// Bits packed into [`Glyph::unicode_props`].
///
/// This is the one map of all sixteen bits; the constants live where
/// their users are:
///
/// | Bits | Meaning | Constant |
/// |------|---------|----------|
/// | 0 | unsubstituted default ignorable | [`DEFAULT_IGNORABLE`] |
/// | 1 | ZWJ | [`JOINER`] |
/// | 2 | ZWNJ | [`NON_JOINER`] |
/// | 3 | hidden ignorable (CGJ, Mongolian FVS, tags) | `match_prop::HIDDEN` |
/// | 4, 5 | synthesized glyph class | `match_prop::SYNTHESIZED_CLASS` |
/// | 6 | output of a ligature substitution | `match_prop::LIGATED` |
/// | 7 | output of a multiple substitution | `match_prop::MULTIPLIED` |
/// | 8 to 15 | HarfBuzz's `lig_props` byte | `match_prop::LIG_PROPS_SHIFT` |
///
/// The `match_prop` constants are in
/// [`crate::tables::layout::skip_iter::match_prop`], which the lookup
/// matching rules read. The `lig_props` byte holds the ligature id in
/// its top three bits, the "is the ligature glyph" flag in bit 4, and
/// the component index in the low four; GSUB records it for GPOS mark
/// attachment. Bits 3 to 15 are the shaper's own: callers building
/// glyphs by hand should leave them zero.
///
/// [`DEFAULT_IGNORABLE`]: crate::buffer::unicode_prop::DEFAULT_IGNORABLE
/// [`JOINER`]: crate::buffer::unicode_prop::JOINER
/// [`NON_JOINER`]: crate::buffer::unicode_prop::NON_JOINER
pub mod unicode_prop {
    /// The glyph's source codepoint is default ignorable in HarfBuzz's
    /// sense (ZWJ, ZWNJ, bidi controls, variation selectors, soft
    /// hyphen, ...) and GSUB has not substituted it. After positioning,
    /// shaping gives glyphs that still carry this bit a zero advance
    /// and swaps in the space glyph; any GSUB substitution clears it,
    /// as in HarfBuzz.
    pub const DEFAULT_IGNORABLE: u16 = 1 << 0;
    /// The glyph's source codepoint is a joiner (ZWJ).
    pub const JOINER: u16 = 1 << 1;
    /// The glyph's source codepoint is a non-joiner (ZWNJ).
    pub const NON_JOINER: u16 = 1 << 2;
}

impl Glyph {
    /// Minimal constructor used by the shaper pipeline: everything
    /// but the glyph id and cluster starts at zero. Exists so the
    /// hot spots in `shape()` stay short even as we grow more
    /// scratch fields.
    #[must_use]
    pub const fn new(glyph_id: u32, cluster: u32) -> Self {
        Self {
            glyph_id,
            cluster,
            x_advance: 0,
            y_advance: 0,
            x_offset: 0,
            y_offset: 0,
            unicode_props: 0,
            indic_position: 0,
            char_class: 0,
            combining_class: 0,
            syllable: 0,
            flags: GlyphFlags::empty(),
        }
    }
}

/// Shaping input: the text run, plus state flags the shaper consults.
///
/// Buffers are reusable. After calling [`crate::shape`] and consuming
/// the output, call [`Buffer::clear`] and push the next run.
#[derive(Debug, Default, Clone)]
pub struct Buffer {
    /// The text being shaped. Stored as `String` so the shaper sees
    /// validated UTF-8 without re-checking.
    pub(crate) text: String,
    /// Writing direction. Defaults to [`Direction::Ltr`].
    pub(crate) direction: Direction,
    /// `true` once a caller picked the direction with
    /// [`Buffer::set_direction`]. While `false`, `direction` is only
    /// the LTR default and `shape()` may choose vertical layout for
    /// Mongolian-dominant text. [`Buffer::clear`] resets it.
    pub(crate) direction_explicit: bool,
    /// Script the whole buffer shapes as, set by
    /// [`Buffer::set_script`]. `None` segments the text into script
    /// runs. Accessors live in `buffer_props.rs`.
    pub(crate) script: Option<Script>,
    /// BCP 47 language selecting the OpenType language system, set by
    /// [`Buffer::set_language`].
    pub(crate) language: Option<crate::language::Language>,
    /// Up to [`Buffer::CONTEXT_LENGTH`] characters that precede the
    /// text in the source, set by [`Buffer::set_pre_context`].
    pub(crate) pre_context: String,
    /// Up to [`Buffer::CONTEXT_LENGTH`] characters that follow the
    /// text in the source, set by [`Buffer::set_post_context`].
    pub(crate) post_context: String,
    /// HarfBuzz's buffer flags, set by [`Buffer::set_flags`].
    pub(crate) flags: BufferFlags,
    /// How clusters form and merge, set by [`Buffer::set_cluster_level`].
    pub(crate) cluster_level: ClusterLevel,
    /// The glyph a variation selector the font cannot resolve becomes,
    /// set by [`Buffer::set_not_found_variation_selector_glyph`].
    pub(crate) not_found_variation_selector: Option<u32>,
}

impl Buffer {
    /// Creates an empty buffer with default direction.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends `text` to the buffer.
    pub fn push_str(&mut self, text: &str) {
        self.text.push_str(text);
    }

    /// Replaces the buffer contents with `text`.
    ///
    /// A buffer holds one run in one direction, as in HarfBuzz. For text
    /// that mixes directions, use [`crate::BidiParagraph`], which shapes
    /// each run of the paragraph in its own direction.
    pub fn set_text(&mut self, text: &str) {
        self.text.clear();
        self.text.push_str(text);
    }

    /// Current text view.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Current writing direction: [`Direction::Ltr`] until one is set
    /// (see [`Self::has_explicit_direction`]).
    #[must_use]
    pub const fn direction(&self) -> Direction {
        self.direction
    }

    /// Sets the writing direction for the next shaping call.
    ///
    /// The direction decides the output order of [`crate::shape`]:
    /// forward directions ([`Direction::Ltr`], [`Direction::Ttb`])
    /// return glyphs in logical order, backward ones
    /// ([`Direction::Rtl`], [`Direction::Btt`]) in reversed (visual)
    /// order, exactly like HarfBuzz. Calling this marks the direction
    /// as explicit (see [`Self::has_explicit_direction`]), even when
    /// `direction` is the LTR default.
    pub fn set_direction(&mut self, direction: Direction) {
        self.direction = direction;
        self.direction_explicit = true;
    }

    /// Forgets the direction the caller chose, the way HarfBuzz's
    /// `hb_buffer_set_direction(buffer, HB_DIRECTION_INVALID)` does.
    ///
    /// [`Self::direction`] goes back to the [`Direction::Ltr`] default
    /// and [`Self::has_explicit_direction`] to `false`, so
    /// [`crate::shape`] once again picks the layout itself (vertical for
    /// Mongolian-dominant text). The text, script, language, and
    /// context are kept.
    ///
    /// ```
    /// use sigilbuzz::{Buffer, Direction};
    ///
    /// let mut buffer = Buffer::new();
    /// buffer.push_str("abc");
    /// buffer.set_direction(Direction::Rtl);
    /// buffer.unset_direction();
    /// assert_eq!(buffer.direction(), Direction::Ltr);
    /// assert!(!buffer.has_explicit_direction());
    /// assert_eq!(buffer.text(), "abc");
    /// ```
    pub fn unset_direction(&mut self) {
        self.direction = Direction::Ltr;
        self.direction_explicit = false;
    }

    /// True when the direction was chosen by the caller through
    /// [`Self::set_direction`], false while
    /// [`Self::direction`] only reports the LTR default.
    ///
    /// [`crate::shape`] lays out Mongolian-dominant text vertically
    /// (top to bottom) only while no direction is explicit; an
    /// explicit [`Direction::Ltr`] keeps it horizontal.
    ///
    /// ```
    /// use sigilbuzz::{Buffer, Direction};
    ///
    /// let mut buffer = Buffer::new();
    /// assert!(!buffer.has_explicit_direction());
    /// buffer.set_direction(Direction::Ltr);
    /// assert!(buffer.has_explicit_direction());
    /// buffer.clear();
    /// assert!(!buffer.has_explicit_direction());
    /// ```
    #[must_use]
    pub const fn has_explicit_direction(&self) -> bool {
        self.direction_explicit
    }

    /// Clears the text and resets direction to the unset LTR default.
    /// Like HarfBuzz's `hb_buffer_clear_contents`, this also forgets
    /// the script, language, and pre- and post-context.
    pub fn clear(&mut self) {
        self.text.clear();
        self.direction = Direction::Ltr;
        self.direction_explicit = false;
        self.script = None;
        self.language = None;
        self.pre_context.clear();
        self.post_context.clear();
    }

    /// True when no text has been pushed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Splits the buffer's text into maximal script runs and yields
    /// one `ScriptRun` per run. Consecutive codepoints sharing the
    /// same resolved script collapse into a single run; `COMMON`
    /// (digits, punctuation, ASCII space, ZWJ/ZWNJ/bidi marks, the
    /// tatweel, the dandas) and `INHERITED` (combining marks)
    /// codepoints, as the Unicode Script property gives them, extend
    /// whichever real script ran before them, matching HarfBuzz's
    /// `select_shaper_for_script` segmentation. A Hangul tone mark
    /// (U+302E, U+302F) extends the run before it too.
    ///
    /// A leading `COMMON`/`INHERITED` span before the first real
    /// script codepoint joins that script's run, the way HarfBuzz
    /// gives a buffer the script of its first non-`COMMON` character.
    /// Text with no real script at all is one `Script::Other` run
    /// with the default `DFLT` priority, same treatment HarfBuzz gives
    /// a pure-digits or pure-punctuation run.
    ///
    /// The returned vector is empty for an empty buffer. Callers walk
    /// it left-to-right: segment boundaries are deterministic, so the
    /// same input always yields the same segmentation.
    #[must_use]
    pub fn script_runs(&self) -> Vec<ScriptRun> {
        let mut runs: Vec<ScriptRun> = Vec::new();
        if self.text.is_empty() {
            return runs;
        }
        let leading = self
            .text
            .chars()
            .find(|&c| !is_common_or_inherited(c))
            .map_or(Script::Other, script_of);
        let mut current: Option<(Script, usize)> = None;
        for (byte, ch) in self.text.char_indices() {
            let raw = script_of(ch);
            // `COMMON` / `INHERITED` extend the previous real-script
            // run if one exists. In sigilbuzz the only explicit bucket
            // we keep for these characters is `Script::Other` (ASCII
            // digits / punctuation land in `Script::Latin`; combining
            // marks inherit their cluster base's script via the
            // `script_of` range table). So the only codepoints we
            // still need to actively extend are the Unicode format
            // characters (ZWJ/ZWNJ/LRM/RLM/ALM) plus any other char
            // that `script_of` could not classify. Everything with a
            // real script bucket attaches normally through the
            // script-equality test below.
            let resolved = if is_common_or_inherited(ch) {
                current.map_or(leading, |(s, _)| s)
            } else if is_hangul_tone_mark(ch) {
                // A Hangul tone mark is a combining mark: it stays
                // with the character before it, as in `shape`.
                current.map_or(raw, |(s, _)| s)
            } else {
                raw
            };
            match current {
                // Extend the active run.
                Some((s, _)) if s == resolved => {}
                Some((s, start)) => {
                    runs.push(ScriptRun {
                        byte_range: start..byte,
                        script: s,
                        script_priority: script_priority_for(s),
                    });
                    current = Some((resolved, byte));
                }
                None => {
                    current = Some((resolved, byte));
                }
            }
        }
        if let Some((s, start)) = current {
            runs.push(ScriptRun {
                byte_range: start..self.text.len(),
                script: s,
                script_priority: script_priority_for(s),
            });
        }
        runs
    }
}

/// One maximal script run carved out of a [`Buffer`]'s text.
///
/// Returned by [`Buffer::script_runs`]. The byte range is expressed
/// against the buffer's current text; the script tag priority is the
/// one `shape()` should use when dispatching GSUB/GPOS lookups for
/// this segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptRun {
    /// Half-open byte range into [`Buffer::text`].
    pub byte_range: Range<usize>,
    /// Resolved script for the run. `Script::Other` for unknown /
    /// pure-COMMON runs with no real-script codepoint.
    pub script: Script,
    /// Script-tag priority list (e.g. `&[b"arab", b"DFLT"]`): what
    /// the OpenType dispatcher walks to locate this run's features.
    pub script_priority: &'static [[u8; 4]],
}

/// DFLT-only priority for text with no recognized script.
/// Interned as a static so `script_priority_for` can return a
/// `'static` reference.
const DFLT_ONLY: &[[u8; 4]] = &[*b"DFLT"];
/// Arabic script-tag priority: `arab` then DFLT fallback.
const ARAB_PRIORITY: &[[u8; 4]] = &[*b"arab", *b"DFLT"];
/// Hebrew script-tag priority: `hebr` then DFLT fallback.
const HEBR_PRIORITY: &[[u8; 4]] = &[*b"hebr", *b"DFLT"];
/// Latin script-tag priority. Language systems (Turkish, Romanian,
/// ...) live under `latn`, so it must come before DFLT for
/// [`Buffer::set_language`] to reach them.
const LATN_PRIORITY: &[[u8; 4]] = &[*b"latn", *b"DFLT"];
/// Cyrillic script-tag priority (`cyrl` holds SRB / MKD / BGR).
const CYRL_PRIORITY: &[[u8; 4]] = &[*b"cyrl", *b"DFLT"];
/// Greek script-tag priority.
const GREK_PRIORITY: &[[u8; 4]] = &[*b"grek", *b"DFLT"];
/// Han script-tag priority (`hani` holds the ZHS / ZHT / JAN / KOR
/// language systems).
const HANI_PRIORITY: &[[u8; 4]] = &[*b"hani", *b"DFLT"];

/// Returns the GSUB/GPOS script-tag priority list for a coarse
/// [`Script`]. Mirrors what `shape()` used to compute inline and what
/// the Indic / USE shapers keep as constants.
///
/// Every priority ends in `DFLT` so mixed-script runs fall back to
/// the default LangSys on fonts that only ship lookups under DFLT.
#[must_use]
pub fn script_priority_for(script: Script) -> &'static [[u8; 4]] {
    use crate::ot::indic::{
        BENG_SCRIPT_PRIORITY, DEVA_SCRIPT_PRIORITY, GUJR_SCRIPT_PRIORITY, GURU_SCRIPT_PRIORITY,
        KNDA_SCRIPT_PRIORITY, MLYM_SCRIPT_PRIORITY, ORYA_SCRIPT_PRIORITY, SINH_SCRIPT_PRIORITY,
        TAML_SCRIPT_PRIORITY, TELU_SCRIPT_PRIORITY,
    };
    use crate::ot::mongolian::MONG_SCRIPT_PRIORITY;
    use crate::ot::tibetan::TIBT_SCRIPT_PRIORITY;
    use crate::ot::use_shaper::{
        BALINESE_SCRIPT_PRIORITY, BRAHMI_SCRIPT_PRIORITY, BUGINESE_SCRIPT_PRIORITY,
        CHAM_SCRIPT_PRIORITY, HANGUL_SCRIPT_PRIORITY, KHMER_SCRIPT_PRIORITY,
        KHOJKI_SCRIPT_PRIORITY, LAO_SCRIPT_PRIORITY, LEPCHA_SCRIPT_PRIORITY, LIMBU_SCRIPT_PRIORITY,
        MODI_SCRIPT_PRIORITY, MYANMAR_SCRIPT_PRIORITY, NKO_SCRIPT_PRIORITY,
        SHARADA_SCRIPT_PRIORITY, SUNDANESE_SCRIPT_PRIORITY, TAI_THAM_SCRIPT_PRIORITY,
        THAI_SCRIPT_PRIORITY, TIRHUTA_SCRIPT_PRIORITY,
    };
    match script {
        Script::Arabic => ARAB_PRIORITY,
        Script::Hebrew => HEBR_PRIORITY,
        Script::Devanagari => DEVA_SCRIPT_PRIORITY,
        Script::Bengali => BENG_SCRIPT_PRIORITY,
        Script::Gurmukhi => GURU_SCRIPT_PRIORITY,
        Script::Gujarati => GUJR_SCRIPT_PRIORITY,
        Script::Oriya => ORYA_SCRIPT_PRIORITY,
        Script::Tamil => TAML_SCRIPT_PRIORITY,
        Script::Telugu => TELU_SCRIPT_PRIORITY,
        Script::Kannada => KNDA_SCRIPT_PRIORITY,
        Script::Malayalam => MLYM_SCRIPT_PRIORITY,
        Script::Sinhala => SINH_SCRIPT_PRIORITY,
        Script::Khmer => KHMER_SCRIPT_PRIORITY,
        Script::Myanmar => MYANMAR_SCRIPT_PRIORITY,
        Script::Thai => THAI_SCRIPT_PRIORITY,
        Script::Lao => LAO_SCRIPT_PRIORITY,
        Script::Hangul => HANGUL_SCRIPT_PRIORITY,
        Script::Tibetan => TIBT_SCRIPT_PRIORITY,
        Script::Mongolian => MONG_SCRIPT_PRIORITY,
        Script::NKo => NKO_SCRIPT_PRIORITY,
        Script::Buginese => BUGINESE_SCRIPT_PRIORITY,
        Script::TaiTham => TAI_THAM_SCRIPT_PRIORITY,
        Script::Balinese => BALINESE_SCRIPT_PRIORITY,
        Script::Sundanese => SUNDANESE_SCRIPT_PRIORITY,
        Script::Lepcha => LEPCHA_SCRIPT_PRIORITY,
        Script::Limbu => LIMBU_SCRIPT_PRIORITY,
        Script::Cham => CHAM_SCRIPT_PRIORITY,
        Script::Brahmi => BRAHMI_SCRIPT_PRIORITY,
        Script::Sharada => SHARADA_SCRIPT_PRIORITY,
        Script::Khojki => KHOJKI_SCRIPT_PRIORITY,
        Script::Tirhuta => TIRHUTA_SCRIPT_PRIORITY,
        Script::Modi => MODI_SCRIPT_PRIORITY,
        Script::Latin => LATN_PRIORITY,
        Script::Cyrillic => CYRL_PRIORITY,
        Script::Greek => GREK_PRIORITY,
        Script::Han => HANI_PRIORITY,
        // The buckets that take their code points from the Unicode
        // Script property try their own tag. Scripts sigilbuzz has no
        // bucket for fall back to DFLT.
        other => other.table_script_priority().unwrap_or(DFLT_ONLY),
    }
}

/// The result of a shaping call: the glyphs, in visual order.
///
/// The order follows the buffer's [`Direction`], the same contract as
/// HarfBuzz's `hb_shape`:
///
/// - [`Direction::Ltr`] and [`Direction::Ttb`] (forward): glyphs come
///   out in logical order, which is also their visual order along the
///   pen's direction of travel.
/// - [`Direction::Rtl`] and [`Direction::Btt`] (backward): shaping runs
///   in logical order, then the glyph vector is reversed. For RTL,
///   `glyphs[0]` is the leftmost glyph (the logically last one) and
///   the pen moves left to right. For BTT, `glyphs[0]` is the topmost
///   glyph and the pen moves down, as in TTB. Clusters keep their
///   logical byte offsets, so they decrease along a backward run.
///
/// In every direction a renderer draws `glyphs` in vector order,
/// placing each glyph at the current pen position plus its
/// `(x_offset, y_offset)` and then adding `(x_advance, y_advance)` to
/// the pen. Vertical runs report negative `y_advance` values (the pen
/// moves down) for both TTB and BTT, and their offsets place each
/// glyph's horizontal origin: like HarfBuzz, shaping moves every
/// glyph from its vertical origin (centered horizontally, at the
/// `VORG` height or the top of its box plus the `vmtx` top side
/// bearing) to its horizontal one.
///
/// One known difference remains: when the requested horizontal
/// direction is not the script's native one (LTR Hebrew, RTL Latin),
/// or for BTT, HarfBuzz reverses the grapheme clusters before shaping
/// and shapes in the opposite direction. sigilbuzz shapes those runs
/// in logical order as asked, so marks inside a cluster and
/// contextual lookups can come out differently there.
#[derive(Debug, Default, Clone)]
pub struct ShapedRun {
    /// Positioned glyphs, ready to draw.
    pub glyphs: Vec<Glyph>,
}

impl ShapedRun {
    /// Number of glyphs produced.
    #[must_use]
    pub fn len(&self) -> usize {
        self.glyphs.len()
    }

    /// True if shaping produced no glyphs (empty input, or pre-shape).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.glyphs.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer_round_trips_text_and_direction() {
        let mut b = Buffer::new();
        b.push_str("hello");
        b.set_direction(Direction::Rtl);
        assert_eq!(b.text(), "hello");
        assert_eq!(b.direction(), Direction::Rtl);
        assert!(!b.is_empty());
    }

    #[test]
    fn clear_resets_everything() {
        let mut b = Buffer::new();
        b.push_str("x");
        b.set_direction(Direction::Ttb);
        b.clear();
        assert!(b.is_empty());
        assert_eq!(b.direction(), Direction::Ltr);
    }

    #[test]
    fn set_text_replaces_rather_than_appends() {
        let mut b = Buffer::new();
        b.set_text("one");
        b.set_text("two");
        assert_eq!(b.text(), "two");
    }

    #[test]
    fn direction_starts_implicit_and_set_direction_makes_it_explicit() {
        let mut b = Buffer::new();
        assert!(!b.has_explicit_direction());
        assert_eq!(b.direction(), Direction::Ltr);
        // Setting the default value still counts as a caller choice.
        b.set_direction(Direction::Ltr);
        assert!(b.has_explicit_direction());
        b.set_direction(Direction::Btt);
        assert!(b.has_explicit_direction());
        assert_eq!(b.direction(), Direction::Btt);
    }

    #[test]
    fn unset_direction_returns_to_the_implicit_default() {
        let mut b = Buffer::new();
        b.push_str("abc");
        b.set_direction(Direction::Ttb);
        b.unset_direction();
        assert!(!b.has_explicit_direction());
        assert_eq!(b.direction(), Direction::Ltr);
        assert_eq!(b.text(), "abc");
        // An explicit LTR is also forgotten.
        b.set_direction(Direction::Ltr);
        b.unset_direction();
        assert!(!b.has_explicit_direction());
    }

    #[test]
    fn text_mutations_keep_the_explicit_direction() {
        let mut b = Buffer::new();
        b.set_direction(Direction::Rtl);
        b.push_str("abc");
        b.set_text("def");
        assert!(b.has_explicit_direction());
        assert_eq!(b.direction(), Direction::Rtl);
    }

    #[test]
    fn clear_resets_the_explicit_direction_flag() {
        let mut b = Buffer::new();
        b.set_direction(Direction::Rtl);
        b.clear();
        assert!(!b.has_explicit_direction());
        assert_eq!(b.direction(), Direction::Ltr);
    }

    #[test]
    fn direction_classifies_axes_and_order() {
        assert!(Direction::Ltr.is_horizontal());
        assert!(Direction::Rtl.is_horizontal());
        assert!(!Direction::Ttb.is_horizontal());
        assert!(Direction::Ltr.is_forward());
        assert!(!Direction::Rtl.is_forward());
    }

    #[test]
    fn script_runs_empty_buffer_yields_nothing() {
        let b = Buffer::new();
        assert!(b.script_runs().is_empty());
    }

    #[test]
    fn script_runs_single_script_latin_is_one_run() {
        let mut b = Buffer::new();
        b.push_str("hello");
        let runs = b.script_runs();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].script, Script::Latin);
        assert_eq!(runs[0].byte_range, 0..5);
        assert_eq!(runs[0].script_priority, &[*b"latn", *b"DFLT"]);
    }

    #[test]
    fn script_runs_splits_latin_then_hebrew() {
        let mut b = Buffer::new();
        b.push_str("Hi \u{05E9}\u{05DC}\u{05D5}\u{05DD}");
        let runs = b.script_runs();
        assert_eq!(runs.len(), 2);
        // "Hi ": space is Latin in our classifier, so it stays on
        // the first run.
        assert_eq!(runs[0].script, Script::Latin);
        assert_eq!(runs[0].byte_range, 0..3);
        assert_eq!(runs[1].script, Script::Hebrew);
        // Hebrew letters are 2 UTF-8 bytes each; 4 chars * 2 = 8
        // bytes starting at offset 3.
        assert_eq!(runs[1].byte_range, 3..11);
        assert_eq!(runs[1].script_priority, &[*b"hebr", *b"DFLT"]);
    }

    #[test]
    fn script_runs_common_digits_stick_to_preceding_script() {
        // "Price: ₪100 שלום": digits land in Script::Latin bucket
        // via script_of, so they extend the Latin prefix. The shekel
        // sign U+20AA falls outside our range table (Script::Other)
        // but still extends the previous run because it is a COMMON
        // codepoint in Unicode; sigilbuzz groups it with Latin here
        // because `Script::Other` matches nothing-scripted neighbors.
        let mut b = Buffer::new();
        b.push_str("Price: 100 \u{05E9}\u{05DC}\u{05D5}\u{05DD}");
        let runs = b.script_runs();
        // Expect 2 runs: Latin prefix through the space, then Hebrew.
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].script, Script::Latin);
        assert_eq!(runs[1].script, Script::Hebrew);
    }

    #[test]
    fn script_runs_zwj_between_arabic_letters_stays_one_run() {
        // kaf + ZWJ + tatweel: ZWJ is a format character that should
        // not break the Arabic segment.
        let mut b = Buffer::new();
        b.push_str("\u{0643}\u{200D}\u{0640}");
        let runs = b.script_runs();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].script, Script::Arabic);
    }

    #[test]
    fn script_runs_default_ignorables_stay_in_their_run() {
        // ZWSP, word joiner, a variation selector and a tag character
        // extend the run they sit in, as in the shaper's segmentation.
        for text in [
            "f\u{200B}i",
            "f\u{2060}i",
            "\u{0628}\u{FE0F}\u{0633}",
            "f\u{E0041}i",
        ] {
            let mut b = Buffer::new();
            b.push_str(text);
            assert_eq!(b.script_runs().len(), 1, "{text:?}");
        }
    }

    #[test]
    fn script_runs_three_scripts_emits_three_segments() {
        // Latin SPACE Arabic SPACE Hebrew. The spaces are COMMON and
        // attach to the preceding real script, so transitioning
        // Latin -> Arabic -> Hebrew produces exactly three segments.
        let mut b = Buffer::new();
        b.push_str("Read \u{0627}\u{0644}\u{0639}\u{0631}\u{0628}\u{064A}\u{0629} \u{05E9}\u{05DC}\u{05D5}\u{05DD}");
        let runs = b.script_runs();
        assert_eq!(runs.len(), 3);
        assert_eq!(runs[0].script, Script::Latin);
        assert_eq!(runs[1].script, Script::Arabic);
        assert_eq!(runs[2].script, Script::Hebrew);
    }

    #[test]
    fn script_runs_are_deterministic() {
        // Same input twice: segmentation must agree byte-for-byte.
        let mut b1 = Buffer::new();
        b1.push_str("Hi \u{05E9}\u{05DC}\u{05D5}\u{05DD} 100 \u{0627}\u{0644}");
        let mut b2 = Buffer::new();
        b2.push_str("Hi \u{05E9}\u{05DC}\u{05D5}\u{05DD} 100 \u{0627}\u{0644}");
        assert_eq!(b1.script_runs(), b2.script_runs());
    }

    #[test]
    fn script_priority_for_common_scripts() {
        assert_eq!(script_priority_for(Script::Arabic), &[*b"arab", *b"DFLT"]);
        assert_eq!(script_priority_for(Script::Hebrew), &[*b"hebr", *b"DFLT"]);
        assert_eq!(script_priority_for(Script::Latin), &[*b"latn", *b"DFLT"]);
        assert_eq!(script_priority_for(Script::Cyrillic), &[*b"cyrl", *b"DFLT"]);
        assert_eq!(script_priority_for(Script::Other), &[*b"DFLT"]);
        assert_eq!(
            script_priority_for(Script::Khmer),
            &[*b"khmr", *b"khm2", *b"DFLT"]
        );
    }

    #[test]
    fn script_runs_combining_mark_inherits_base_script() {
        // "é" as base + Unicode combining acute (U+0301). The combining
        // mark has Unicode script == INHERITED, which the segmenter
        // must resolve to the preceding Latin run. Otherwise the mark
        // gets carved into its own Script::Other segment and GSUB's
        // `ccmp` decomposition pass fires under the wrong priority.
        let mut b = Buffer::new();
        b.push_str("e\u{0301}");
        let runs = b.script_runs();
        assert_eq!(runs.len(), 1, "combining mark must extend its base");
        assert_eq!(runs[0].script, Script::Latin);
        assert_eq!(runs[0].byte_range, 0..3);
    }

    #[test]
    fn script_runs_hangul_tone_mark_stays_with_the_letter_before_it() {
        let mut b = Buffer::new();
        b.push_str("e\u{0301}\u{302E}");
        let runs = b.script_runs();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].script, Script::Latin);
        // At the start of the text the tone mark is Hangul.
        let mut b = Buffer::new();
        b.push_str("\u{302E}a");
        let runs = b.script_runs();
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].script, Script::Hangul);
        assert_eq!(runs[0].byte_range, 0..3);
    }

    #[test]
    fn script_runs_leading_punctuation_joins_the_first_script() {
        // "(123 " before Hebrew belongs to the Hebrew run.
        let mut b = Buffer::new();
        b.push_str("(123 \u{05E9}\u{05DC}\u{05D5}\u{05DD})");
        let runs = b.script_runs();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].script, Script::Hebrew);
        // No script-bearing character at all: one DFLT run.
        let mut b = Buffer::new();
        b.push_str("12:30!");
        let runs = b.script_runs();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].script, Script::Other);
        assert_eq!(runs[0].script_priority, &[*b"DFLT"]);
    }

    #[test]
    fn script_runs_arabic_with_quranic_mark_stays_one_segment() {
        // U+06D6 ARABIC SMALL HIGH LIGATURE SAD is an Arabic-script
        // combining mark: its Script property is Arabic, not
        // Inherited, so it already collapses via the equality check.
        // This test pins the Arabic baseline so the Inherited fix does
        // not accidentally widen the COMMON bucket past real-script
        // marks.
        let mut b = Buffer::new();
        b.push_str("\u{0627}\u{06D6}");
        let runs = b.script_runs();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].script, Script::Arabic);
    }
}
