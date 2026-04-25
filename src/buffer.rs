//! Shaping input and output types.
//!
//! A [`Buffer`] is fed one text run at a time, then passed to
//! [`crate::shape`] along with a [`crate::Font`]. On a successful
//! shape it yields a vector of [`Glyph`]s — each carrying the glyph
//! index the renderer should emit plus the position of that glyph
//! relative to the pen.
//!
//! The API mirrors `HarfBuzz`'s `hb_buffer_t` deliberately, so a
//! consumer who already knows `HarfBuzz` can reach for sigilbuzz without
//! relearning concepts.

use alloc::string::String;
use alloc::vec::Vec;
use core::ops::Range;

use crate::unicode::{script_of, Script};

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
/// In addition to the rendered fields, `Glyph` carries two
/// shaper-internal scratch fields — `unicode_props` and
/// `indic_position` — that the Indic / complex-script shapers use
/// to track per-glyph state across GSUB passes. Renderers and
/// most callers can ignore them; they are public so the shaper
/// modules inside this crate can round-trip state through `Vec<Glyph>`
/// without stashing a parallel array. Stable bits of `unicode_props`
/// are set once during buffer preparation (default-ignorable,
/// joiner, …); `indic_position` is an [`IndicPosition`] value that
/// survives ligature substitutions (the surviving glyph inherits
/// the first-component position).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Glyph {
    /// Glyph index within the font. After shaping, this is the index
    /// in the SFNT glyph table, *not* a Unicode codepoint.
    pub glyph_id: u32,
    /// Cluster tag linking this glyph back to the input codepoints.
    /// Multiple glyphs with the same cluster came from the same input
    /// grapheme (e.g. a ligature, or a base + combining mark).
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
    /// "was this glyph's source a joiner / default-ignorable / …?"
    /// without re-deriving from the cluster. See [`unicode_prop`].
    pub unicode_props: u16,
    /// Shaper-internal Indic positional role, set during Indic
    /// syllable segmentation and consulted by the final-reorder
    /// pass. Zero (`IndicPosition::Start`) for non-Indic glyphs and
    /// for Indic glyphs whose role has not been resolved yet.
    pub indic_position: u8,
}

/// Bits packed into [`Glyph::unicode_props`]. Laid out to leave room
/// for future expansion without shifting existing meanings.
pub mod unicode_prop {
    /// The glyph's source codepoint is a Unicode default-ignorable
    /// format character (ZWJ, ZWNJ, LRM, RLM, …).
    pub const DEFAULT_IGNORABLE: u16 = 1 << 0;
    /// The glyph's source codepoint is a joiner (ZWJ).
    pub const JOINER: u16 = 1 << 1;
    /// The glyph's source codepoint is a non-joiner (ZWNJ).
    pub const NON_JOINER: u16 = 1 << 2;
}

/// Indic positional role — stored in [`Glyph::indic_position`] as
/// `u8`. Mirrors HarfBuzz's `ot_position_t` so that a future port
/// of the richer Indic reorder (pref, below-form resolution, …) can
/// drop the constants in without a rename. Only the slots
/// sigilbuzz currently uses are documented; reserved intermediate
/// values keep parity with HarfBuzz so the enum's integer layout
/// does not shift.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // reserved slots mirror HarfBuzz's ot_position_t
pub enum IndicPosition {
    /// Default / unresolved — also used for non-Indic glyphs.
    Start = 0,
    /// A leading `ra` that is a reph candidate. Set on the glyph
    /// carrying the reph before GSUB runs; the reph glyph inherits
    /// the position through ligature substitution.
    RaToBecomeReph = 1,
    /// Pre-base matra (before the base consonant visually).
    PreM = 2,
    /// Pre-base consonant — reserved.
    PreC = 3,
    /// The base consonant of a syllable.
    BaseC = 4,
    /// After the main consonant — reserved.
    AfterMain = 5,
    /// Above-base glyph — reserved.
    AboveC = 6,
    /// Before sub-joined form — reserved.
    BeforeSub = 7,
    /// Below-base glyph — reserved.
    BelowC = 8,
    /// After sub-joined form — reserved.
    AfterSub = 9,
    /// Before post-base position. Target slot for Devanagari reph.
    BeforePost = 10,
    /// Post-base glyph — reserved.
    PostC = 11,
    /// After post-base position — reserved.
    AfterPost = 12,
    /// Syllable modifier / vedic — reserved.
    Smvd = 13,
    /// End-of-syllable sentinel — reserved.
    End = 14,
}

impl Glyph {
    /// Minimal constructor used by the shaper pipeline — everything
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
            indic_position: IndicPosition::Start as u8,
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
    /// When `true`, `shape()` composes the input text via
    /// [`crate::unicode::normalize::compose_str`] before glyph
    /// lookup. Matches HarfBuzz's implicit NFC pass for the
    /// ranges sigilbuzz has curated tables for.
    pub(crate) normalize_nfc: bool,
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
    pub fn set_text(&mut self, text: &str) {
        self.text.clear();
        self.text.push_str(text);
    }

    /// Current text view.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Current writing direction.
    #[must_use]
    pub const fn direction(&self) -> Direction {
        self.direction
    }

    /// Sets the writing direction for the next shaping call.
    pub fn set_direction(&mut self, direction: Direction) {
        self.direction = direction;
    }

    /// True when [`Buffer::set_normalize_nfc`] has been enabled.
    #[must_use]
    pub const fn normalize_nfc(&self) -> bool {
        self.normalize_nfc
    }

    /// Enables or disables the implicit NFC composition pass that
    /// runs before glyph lookup. Off by default. Turn this on to
    /// match HarfBuzz's behaviour, where `e + U+0301` renders the
    /// same as the precomposed `é`.
    pub fn set_normalize_nfc(&mut self, enabled: bool) {
        self.normalize_nfc = enabled;
    }

    /// Clears the text and resets direction to LTR. Other future
    /// state (script, language, user data) will reset here too.
    pub fn clear(&mut self) {
        self.text.clear();
        self.direction = Direction::Ltr;
        self.normalize_nfc = false;
    }

    /// True when no text has been pushed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Splits the buffer's text into maximal script runs and yields
    /// one [`ScriptRun`] per run. Consecutive codepoints sharing the
    /// same resolved script collapse into a single run; `COMMON`
    /// (digits, punctuation, ASCII space, ZWJ/ZWNJ/bidi marks) and
    /// `INHERITED` (combining marks) codepoints extend whichever real
    /// script ran before them, matching HarfBuzz's
    /// `select_shaper_for_script` segmentation.
    ///
    /// A leading `COMMON`/`INHERITED` span before the first real
    /// script codepoint takes `Script::Other` with the default `DFLT`
    /// priority — same treatment HarfBuzz gives a pure-digits or
    /// pure-punctuation run.
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
            // characters — ZWJ/ZWNJ/LRM/RLM/ALM — plus any other char
            // that `script_of` could not classify. Everything with a
            // real script bucket attaches normally through the
            // script-equality test below.
            let resolved = if is_common_or_inherited(ch) {
                current.map_or(raw, |(s, _)| s)
            } else {
                raw
            };
            match current {
                Some((s, start)) if s == resolved => {
                    // Extend the active run.
                    let _ = start;
                }
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
    /// Script-tag priority list (e.g. `&[b"arab", b"DFLT"]`) — what
    /// the OpenType dispatcher walks to locate this run's features.
    pub script_priority: &'static [[u8; 4]],
}

/// DFLT-only priority for Latin / Greek / Cyrillic / Han / unknown
/// scripts. Interned as a static so `script_priority_for` can return
/// a `'static` reference.
const DFLT_ONLY: &[[u8; 4]] = &[*b"DFLT"];
/// Arabic script-tag priority: `arab` then DFLT fallback.
const ARAB_PRIORITY: &[[u8; 4]] = &[*b"arab", *b"DFLT"];
/// Hebrew script-tag priority: `hebr` then DFLT fallback.
const HEBR_PRIORITY: &[[u8; 4]] = &[*b"hebr", *b"DFLT"];

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
        // Latin / Greek / Cyrillic / Han / Other — DFLT is where Latin
        // shipped features live and where anything we do not have
        // specialised dispatch for falls back.
        _ => DFLT_ONLY,
    }
}

/// True for codepoints HarfBuzz treats as `COMMON` or `INHERITED`
/// for segmentation purposes — they should extend the adjacent
/// real-script run rather than carve their own segment.
///
/// Covers:
/// - ASCII controls, whitespace, and punctuation (U+0000..U+002F,
///   U+003A..U+0040, U+005B..U+0060, U+007B..U+007E) including the
///   ASCII digits so `"Price: 100 شلوم"` keeps the Arabic tail from
///   detaching on the digits.
/// - Latin-1 punctuation / symbols (U+00A0..U+00BF).
/// - The Unicode format-character block sigilbuzz already recognises
///   (ZWJ / ZWNJ / LRM / RLM / ALM).
/// - Unicode `INHERITED` combining-mark blocks — Combining
///   Diacritical Marks (U+0300..U+036F), the Supplement
///   (U+1DC0..U+1DFF), Combining Diacritical Marks for Symbols
///   (U+20D0..U+20FF), and Combining Half Marks (U+FE20..U+FE2F).
///   Without these, `"e\u{0301}"` segments into Latin + Other
///   because `script_of` has no rule for U+0300 and drops the
///   mark into `Script::Other` — breaking `ccmp` dispatch and
///   any cross-mark GSUB context.
///
/// Everything else resolves via [`script_of`]; runs of the same
/// real script collapse through the normal equality check.
const fn is_common_or_inherited(ch: char) -> bool {
    let cp = ch as u32;
    matches!(
        cp,
        // ASCII controls + SPACE + !"#$%&'()*+,-./
        0x0000..=0x002F
        // ASCII digits + :;<=>?@
        | 0x0030..=0x0040
        // ASCII [\]^_`
        | 0x005B..=0x0060
        // ASCII {|}~ + DEL
        | 0x007B..=0x007F
        // Latin-1 punctuation / symbols block
        | 0x00A0..=0x00BF
        // Unicode format characters the shaper recognises.
        | 0x200C | 0x200D | 0x200E | 0x200F | 0x061C
        // INHERITED combining-mark blocks.
        | 0x0300..=0x036F
        | 0x1DC0..=0x1DFF
        | 0x20D0..=0x20FF
        | 0xFE20..=0xFE2F
    )
}

/// The result of a shaping call: the glyphs, in visual order.
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
        assert_eq!(runs[0].script_priority, &[*b"DFLT"]);
    }

    #[test]
    fn script_runs_splits_latin_then_hebrew() {
        let mut b = Buffer::new();
        b.push_str("Hi \u{05E9}\u{05DC}\u{05D5}\u{05DD}");
        let runs = b.script_runs();
        assert_eq!(runs.len(), 2);
        // "Hi " — space is Latin in our classifier, so it stays on
        // the first run.
        assert_eq!(runs[0].script, Script::Latin);
        assert_eq!(runs[0].byte_range, 0..3);
        assert_eq!(runs[1].script, Script::Hebrew);
        // Hebrew letters are 2 UTF-8 bytes each; 4 chars × 2 = 8
        // bytes starting at offset 3.
        assert_eq!(runs[1].byte_range, 3..11);
        assert_eq!(runs[1].script_priority, &[*b"hebr", *b"DFLT"]);
    }

    #[test]
    fn script_runs_common_digits_stick_to_preceding_script() {
        // "Price: ₪100 שלום" — digits land in Script::Latin bucket
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
    fn script_runs_three_scripts_emits_three_segments() {
        // Latin SPACE Arabic SPACE Hebrew. The spaces are COMMON and
        // attach to the preceding real script, so transitioning
        // Latin → Arabic → Hebrew produces exactly three segments.
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
        // Same input twice — segmentation must agree byte-for-byte.
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
        assert_eq!(script_priority_for(Script::Latin), &[*b"DFLT"]);
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
        // must resolve to the preceding Latin run — otherwise the mark
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
    fn script_runs_arabic_with_quranic_mark_stays_one_segment() {
        // U+06D6 ARABIC SMALL HIGH LIGATURE SAD is an Arabic-script
        // combining mark — its Script property is Arabic, not
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
