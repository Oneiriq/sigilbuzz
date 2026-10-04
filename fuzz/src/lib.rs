//! Shared helpers for the sigilbuzz fuzz targets.
//!
//! The helpers never panic on their own, so any crash a target reports comes
//! from sigilbuzz itself.

use sigilbuzz::{
    shape, BidiParagraph, Buffer, BufferFlags, ClusterLevel, Direction, Face, Feature, Font,
};

/// Text samples that reach every shaper: Latin ligatures, Arabic joining,
/// Hebrew marks, the Indic family, USE scripts, Mongolian, Tibetan, CJK,
/// emoji sequences, and bidi controls.
pub const SAMPLE_TEXTS: &[&str] = &[
    "Hello, world! office fi ffl AV To",
    "\u{0645}\u{0631}\u{062d}\u{0628}\u{0627} \u{0627}\u{0644}\u{0644}\u{0647}",
    "\u{05e9}\u{05c1}\u{05b8}\u{05dc}\u{05d5}\u{05b9}\u{05dd} \u{05e2}\u{05d5}\u{05b9}\u{05dc}\u{05b8}\u{05dd}",
    "\u{0928}\u{092e}\u{0938}\u{094d}\u{0924}\u{0947} \u{0915}\u{094d}\u{0937}\u{0924}\u{094d}\u{0930}\u{093f}\u{092f} \u{0930}\u{094d}\u{0915}",
    "\u{1796}\u{17d2}\u{179a}\u{17c7}\u{179a}\u{17b6}\u{1787}\u{17b6}",
    "\u{1019}\u{1004}\u{103a}\u{1039}\u{1002}\u{101c}\u{102c}",
    "\u{0e2a}\u{0e27}\u{0e31}\u{0e2a}\u{0e14}\u{0e35}\u{0e0d}\u{0e33}",
    "\u{182e}\u{1823}\u{1829}\u{182d}\u{1823}\u{182f} \u{180b}\u{180e}",
    "\u{0f56}\u{0f7c}\u{0f51}\u{0f0b}\u{0f61}\u{0f72}\u{0f42}",
    "\u{6f22}\u{5b57}\u{304b}\u{306a}\u{30ab}\u{30ca}\u{d55c}\u{ad6d}\u{c5b4}\u{1100}\u{1161}\u{11a8}",
    "a\u{0301}\u{0327}\u{200d}\u{fe0f} \u{1f642}\u{1f44d}\u{1f3fd} \u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}",
    "abc \u{202e}xyz\u{202c} (\u{05d0}[\u{05d1}]) \u{2067}12\u{2069} \u{0661}\u{0662}",
];

/// Common feature tags, turned on or off by the fuzzer.
pub const FEATURE_TAGS: &[[u8; 4]] = &[
    *b"liga", *b"kern", *b"calt", *b"smcp", *b"c2sc", *b"onum", *b"tnum", *b"frac", *b"ss01",
    *b"vert", *b"vrt2", *b"rlig", *b"ccmp", *b"mark", *b"mkmk", *b"zzzz", *b"rvrn", *b"palt",
];

/// A small reader over the fuzzer's control bytes. It hands out zeros once
/// the bytes run out.
pub struct Knobs<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Knobs<'a> {
    /// Wraps the given control bytes.
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    /// Next byte, or 0 when the bytes are used up.
    pub fn byte(&mut self) -> u8 {
        let b = self.bytes.get(self.pos).copied().unwrap_or(0);
        self.pos = self.pos.saturating_add(1);
        b
    }

    /// Next two bytes as a big-endian u16.
    pub fn u16(&mut self) -> u16 {
        u16::from_be_bytes([self.byte(), self.byte()])
    }

    /// A coordinate in roughly -1.5..1.5, with occasional non-finite values.
    pub fn coord(&mut self) -> f32 {
        match self.byte() {
            250 => f32::NAN,
            251 => f32::INFINITY,
            252 => f32::NEG_INFINITY,
            253 => f32::MAX,
            b => (f32::from(b) - 125.0) / 83.0,
        }
    }
}

/// Splits fuzz input into a control prefix and the remaining payload.
pub fn split_control(data: &[u8], control_len: usize) -> (&[u8], &[u8]) {
    let n = control_len.min(data.len());
    data.split_at(n)
}

/// The number of glyphs the face declares, or zero.
pub fn num_glyphs(face: &Face<'_>) -> u16 {
    face.maxp().map(|m| m.num_glyphs).unwrap_or(0)
}

/// A bounded set of glyph ids to exercise: the first few real glyphs, some
/// chosen by the fuzzer (possibly out of range), and the maximum id.
pub fn glyph_ids(face: &Face<'_>, knobs: &mut Knobs<'_>) -> Vec<u16> {
    let mut ids: Vec<u16> = (0..num_glyphs(face).min(16)).collect();
    for _ in 0..4 {
        ids.push(knobs.u16());
    }
    ids.push(u16::MAX);
    ids
}

/// One coordinate per variation axis, plus a few extra so length
/// mismatches get exercised too.
pub fn axis_coords(face: &Face<'_>, knobs: &mut Knobs<'_>) -> Vec<f32> {
    let axes = match face.fvar() {
        Ok(Some(fvar)) => fvar.axes().len(),
        _ => 0,
    };
    let extra = usize::from(knobs.byte() % 3);
    (0..axes.min(16) + extra).map(|_| knobs.coord()).collect()
}

/// Shapes every sample text (plus `extra`) against `face` with the given
/// knobs choosing direction, features, size, and coordinates.
pub fn shape_samples(face: Face<'_>, knobs: &mut Knobs<'_>, extra: Option<&str>) {
    let coords = axis_coords(&face, knobs);
    let size = f32::from(knobs.byte()) + 1.0;
    // Half the time the instance comes from user-space values.
    let font = if knobs.byte() & 1 == 1 {
        let variations: Vec<([u8; 4], f32)> = match face.fvar() {
            Ok(Some(fvar)) => fvar
                .axes()
                .iter()
                .map(|a| (a.tag, a.default_value + knobs.coord() * 500.0))
                .collect(),
            _ => Vec::new(),
        };
        Font::new(face, size).with_variations(&variations)
    } else {
        Font::new(face, size).with_coords(&coords)
    };
    let mut features = Vec::new();
    for _ in 0..(knobs.byte() % 6) {
        let i = usize::from(knobs.byte()) % FEATURE_TAGS.len();
        features.push(Feature { tag: FEATURE_TAGS[i], value: u32::from(knobs.byte() % 3) });
    }
    let mode = knobs.byte();
    let flags = BufferFlags::from_bits_truncate(u32::from(knobs.byte()));
    let level = match (mode >> 3) % 4 {
        0 => ClusterLevel::MonotoneGraphemes,
        1 => ClusterLevel::MonotoneCharacters,
        2 => ClusterLevel::Characters,
        _ => ClusterLevel::Graphemes,
    };
    let not_found_selector = (mode & 0x80 != 0).then(|| u32::from(knobs.byte()));
    let texts = SAMPLE_TEXTS.iter().copied().chain(extra);
    for text in texts {
        let mut buffer = Buffer::new();
        buffer.set_flags(flags);
        buffer.set_cluster_level(level);
        buffer.set_not_found_variation_selector_glyph(not_found_selector);
        match mode % 5 {
            0 => buffer.push_str(text),
            1 => buffer.set_text(text),
            _ => {
                // Bidi text shapes run by run, each run in its own
                // direction, with `buffer` as the settings template.
                let paragraph = BidiParagraph::new(text, None);
                let _ = paragraph.shape(&font, &buffer, &features);
                buffer.set_text(text);
            }
        }
        buffer.set_direction(match (mode >> 5) % 4 {
            0 => Direction::Ltr,
            1 => Direction::Rtl,
            2 => Direction::Ttb,
            _ => Direction::Btt,
        });
        let _ = buffer.script_runs();
        let _ = shape(&font, &buffer, &features);
    }
}
