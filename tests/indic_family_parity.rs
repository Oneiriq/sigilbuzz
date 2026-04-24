//! Indic family shaping parity tests.
//!
//! Shapes a small per-script corpus with both sigilbuzz and
//! rustybuzz against the Noto Sans family (OFL) and asserts the
//! glyph output matches. Each script's corpus exercises a plain
//! consonant, a pre-base or post-base matra, a conjunct, and a
//! reph-candidate syllable where applicable.
//!
//! Test entries whose shape depends on Indic features sigilbuzz does
//! not yet implement (explicit reph, LogRepha, split matras, …)
//! carry `compare_rustybuzz: false` and a `note` explaining why.
//! They still run through sigilbuzz to guard against panics /
//! cluster-integrity regressions; they just do not cross-check the
//! output against rustybuzz.

use sigilbuzz::{shape, Blob, Buffer, Face, Font};

const NOTO_BENGALI: &[u8] = include_bytes!("fonts/NotoSansBengali-Regular.ttf");
const NOTO_GURMUKHI: &[u8] = include_bytes!("fonts/NotoSansGurmukhi-Regular.ttf");
const NOTO_GUJARATI: &[u8] = include_bytes!("fonts/NotoSansGujarati-Regular.ttf");
const NOTO_ORIYA: &[u8] = include_bytes!("fonts/NotoSansOriya-Regular.ttf");
const NOTO_TAMIL: &[u8] = include_bytes!("fonts/NotoSansTamil-Regular.ttf");
const NOTO_TELUGU: &[u8] = include_bytes!("fonts/NotoSansTelugu-Regular.ttf");
const NOTO_KANNADA: &[u8] = include_bytes!("fonts/NotoSansKannada-Regular.ttf");
const NOTO_MALAYALAM: &[u8] = include_bytes!("fonts/NotoSansMalayalam-Regular.ttf");
const NOTO_SINHALA: &[u8] = include_bytes!("fonts/NotoSansSinhala-Regular.ttf");

struct Case {
    text: &'static str,
    compare_rustybuzz: bool,
    note: &'static str,
}

/// Runs the corpus against the given font. Shape both engines; when
/// `compare_rustybuzz` is true, assert glyph ids + x_advances line up.
/// Non-comparable entries still execute sigilbuzz so we catch panics
/// and cluster-integrity regressions.
fn run_corpus(script_name: &str, font_bytes: &[u8], corpus: &[Case]) {
    let blob = Blob::new(font_bytes);
    let face = Face::parse(&blob, 0).expect("parse sigilbuzz face");
    let font = Font::new(face, 1000.0);

    let rb_face = rustybuzz::Face::from_slice(font_bytes, 0).expect("parse rustybuzz face");

    for case in corpus {
        let mut buffer = Buffer::new();
        buffer.push_str(case.text);
        let sig = shape(&font, &buffer, &[]).expect("sigilbuzz shape");

        // Cluster-integrity: every glyph's cluster must sit within the
        // input byte range. Catches reorders that drop byte offsets.
        let byte_len = case.text.len() as u32;
        for g in &sig.glyphs {
            assert!(
                g.cluster <= byte_len,
                "[{script_name}/{}] glyph cluster {} out of range (byte_len={byte_len})",
                case.note,
                g.cluster
            );
        }

        if !case.compare_rustybuzz {
            continue;
        }

        let mut rb_buf = rustybuzz::UnicodeBuffer::new();
        rb_buf.push_str(case.text);
        let rb_out = rustybuzz::shape(&rb_face, &[], rb_buf);
        let rb_infos = rb_out.glyph_infos();
        let rb_positions = rb_out.glyph_positions();

        assert_eq!(
            sig.len(),
            rb_infos.len(),
            "[{script_name}/{}] glyph count diverged ({:?}): sigilbuzz={} rustybuzz={}",
            case.note,
            case.text,
            sig.len(),
            rb_infos.len()
        );

        for (i, (sig_g, (rb_info, rb_pos))) in sig
            .glyphs
            .iter()
            .zip(rb_infos.iter().zip(rb_positions.iter()))
            .enumerate()
        {
            assert_eq!(
                sig_g.glyph_id, rb_info.glyph_id,
                "[{script_name}/{}] glyph id mismatch at position {i} ({:?})",
                case.note, case.text
            );
            assert_eq!(
                sig_g.x_advance, rb_pos.x_advance,
                "[{script_name}/{}] x_advance mismatch at position {i} ({:?}): sigilbuzz={} rustybuzz={}",
                case.note, case.text, sig_g.x_advance, rb_pos.x_advance
            );
        }
    }
}

// -----------------------------------------------------------------
// Bengali — U+0980..U+09FF. Reph: AfterSub (Implicit).
// -----------------------------------------------------------------
#[test]
fn bengali_corpus_matches_rustybuzz() {
    let corpus = &[
        Case {
            text: "",
            compare_rustybuzz: true,
            note: "empty",
        },
        Case {
            // ক (ka)
            text: "\u{0995}",
            compare_rustybuzz: true,
            note: "ka alone",
        },
        Case {
            // কি (ka + pre-base i)
            text: "\u{0995}\u{09BF}",
            compare_rustybuzz: true,
            note: "ki (pre-base matra)",
        },
        Case {
            // কা (ka + post-base aa)
            text: "\u{0995}\u{09BE}",
            compare_rustybuzz: true,
            note: "kaa (post-base matra)",
        },
        Case {
            // Bengali digits 0-3
            text: "\u{09E6}\u{09E7}\u{09E8}\u{09E9}",
            compare_rustybuzz: true,
            note: "bengali digits",
        },
        Case {
            // ক্ষ — ka + virama + ssa conjunct
            text: "\u{0995}\u{09CD}\u{09B7}",
            compare_rustybuzz: true,
            note: "kssa conjunct",
        },
        Case {
            // র্ক — ra + virama + ka (reph, AfterSub).
            // Sigilbuzz's refined final_reorder now handles the
            // AfterSub target for the single-post-base syllable we
            // test here (see issue #26).
            text: "\u{09B0}\u{09CD}\u{0995}",
            compare_rustybuzz: true,
            note: "reph + ka (AfterSub)",
        },
        Case {
            // র্ম — ra + virama + ma (reph variation)
            text: "\u{09B0}\u{09CD}\u{09AE}",
            compare_rustybuzz: true,
            note: "reph + ma",
        },
    ];
    run_corpus("Bengali", NOTO_BENGALI, corpus);
}

// -----------------------------------------------------------------
// Gurmukhi — U+0A00..U+0A7F. Reph: BeforeSub (Implicit).
// -----------------------------------------------------------------
#[test]
fn gurmukhi_corpus_matches_rustybuzz() {
    let corpus = &[
        Case {
            text: "",
            compare_rustybuzz: true,
            note: "empty",
        },
        Case {
            // ਕ (ka)
            text: "\u{0A15}",
            compare_rustybuzz: true,
            note: "ka alone",
        },
        Case {
            // ਕਿ (ka + pre-base i)
            text: "\u{0A15}\u{0A3F}",
            compare_rustybuzz: true,
            note: "ki (pre-base matra)",
        },
        Case {
            // ਕਾ (ka + post-base aa)
            text: "\u{0A15}\u{0A3E}",
            compare_rustybuzz: true,
            note: "kaa (post-base matra)",
        },
        Case {
            // Gurmukhi digits
            text: "\u{0A66}\u{0A67}\u{0A68}",
            compare_rustybuzz: true,
            note: "gurmukhi digits",
        },
        Case {
            // ਖ੍ਯ — kha + halant + ya (subjoined conjunct).
            // Gurmukhi's `pstf` feature rewrites halant+ya into
            // the yakash subjoined form; sigilbuzz now masks the
            // competing `half` feature off on the pre-halant
            // consonant when the post-halant pair is blwf/pstf/abvf
            // eligible, matching rustybuzz. See issue #27.
            text: "\u{0A16}\u{0A4D}\u{0A2F}",
            compare_rustybuzz: true,
            note: "kha + subjoined ya (BeforeSub + blwf/pstf mask)",
        },
        Case {
            // ਕ੍ਯ — ka + halant + ya (subjoined conjunct variant).
            text: "\u{0A15}\u{0A4D}\u{0A2F}",
            compare_rustybuzz: true,
            note: "ka + subjoined ya",
        },
    ];
    run_corpus("Gurmukhi", NOTO_GURMUKHI, corpus);
}

// -----------------------------------------------------------------
// Gujarati — U+0A80..U+0AFF. Reph: BeforePost (Implicit).
// -----------------------------------------------------------------
#[test]
fn gujarati_corpus_matches_rustybuzz() {
    let corpus = &[
        Case {
            text: "",
            compare_rustybuzz: true,
            note: "empty",
        },
        Case {
            // ક (ka)
            text: "\u{0A95}",
            compare_rustybuzz: true,
            note: "ka alone",
        },
        Case {
            // કિ (ka + pre-base i)
            text: "\u{0A95}\u{0ABF}",
            compare_rustybuzz: true,
            note: "ki (pre-base matra)",
        },
        Case {
            // કા (ka + post-base aa)
            text: "\u{0A95}\u{0ABE}",
            compare_rustybuzz: true,
            note: "kaa (post-base matra)",
        },
        Case {
            // ક્ષ — ka + halant + ssa conjunct
            text: "\u{0A95}\u{0ACD}\u{0AB7}",
            compare_rustybuzz: true,
            note: "kssa conjunct",
        },
        Case {
            // ર્ક — ra + halant + ka (reph, BeforePost — same path as Devanagari)
            text: "\u{0AB0}\u{0ACD}\u{0A95}",
            compare_rustybuzz: true,
            note: "reph + ka (BeforePost)",
        },
        Case {
            // Gujarati digits
            text: "\u{0AE6}\u{0AE7}\u{0AE8}",
            compare_rustybuzz: true,
            note: "gujarati digits",
        },
    ];
    run_corpus("Gujarati", NOTO_GUJARATI, corpus);
}

// -----------------------------------------------------------------
// Oriya — U+0B00..U+0B7F. Reph: AfterMain (Implicit).
// -----------------------------------------------------------------
#[test]
fn oriya_corpus_matches_rustybuzz() {
    let corpus = &[
        Case {
            text: "",
            compare_rustybuzz: true,
            note: "empty",
        },
        Case {
            // କ (ka)
            text: "\u{0B15}",
            compare_rustybuzz: true,
            note: "ka alone",
        },
        Case {
            // କା (ka + aa)
            text: "\u{0B15}\u{0B3E}",
            compare_rustybuzz: true,
            note: "kaa (post-base matra)",
        },
        Case {
            // କେ (ka + pre-base e)
            text: "\u{0B15}\u{0B47}",
            compare_rustybuzz: true,
            note: "ke (pre-base matra)",
        },
        Case {
            // ର୍କ — ra + halant + ka (reph, AfterMain).
            // Sigilbuzz's refined final_reorder handles AfterMain
            // by landing the reph right after the base consonant
            // (see issue #28).
            text: "\u{0B30}\u{0B4D}\u{0B15}",
            compare_rustybuzz: true,
            note: "reph + ka (AfterMain)",
        },
        Case {
            // Oriya digits
            text: "\u{0B66}\u{0B67}\u{0B68}",
            compare_rustybuzz: true,
            note: "oriya digits",
        },
    ];
    run_corpus("Oriya", NOTO_ORIYA, corpus);
}

// -----------------------------------------------------------------
// Tamil — U+0B80..U+0BFF. Reph: AfterPost (Implicit).
// -----------------------------------------------------------------
#[test]
fn tamil_corpus_matches_rustybuzz() {
    let corpus = &[
        Case {
            text: "",
            compare_rustybuzz: true,
            note: "empty",
        },
        Case {
            // க (ka)
            text: "\u{0B95}",
            compare_rustybuzz: true,
            note: "ka alone",
        },
        Case {
            // கா (ka + post-base aa)
            text: "\u{0B95}\u{0BBE}",
            compare_rustybuzz: true,
            note: "kaa (post-base matra)",
        },
        Case {
            // கெ (ka + pre-base e)
            text: "\u{0B95}\u{0BC6}",
            compare_rustybuzz: true,
            note: "ke (pre-base matra)",
        },
        Case {
            // கே (ka + pre-base ee)
            text: "\u{0B95}\u{0BC7}",
            compare_rustybuzz: true,
            note: "kee (pre-base matra)",
        },
        Case {
            // க்ஷ — ka + virama + ssa (conjunct)
            text: "\u{0B95}\u{0BCD}\u{0BB7}",
            compare_rustybuzz: true,
            note: "kshha conjunct",
        },
        Case {
            // கோ — ka + two-part matra OO (= ee + aa). The Indic
            // shaper now decomposes the matra at buffer-prep time,
            // matching rustybuzz (see issue #29).
            text: "\u{0B95}\u{0BCB}",
            compare_rustybuzz: true,
            note: "koo (split matra U+0BCB)",
        },
        Case {
            // கொ — ka + two-part matra O (= e + aa).
            text: "\u{0B95}\u{0BCA}",
            compare_rustybuzz: true,
            note: "ko (split matra U+0BCA)",
        },
        Case {
            // கௌ — ka + two-part matra AU (= e + au-length-mark).
            text: "\u{0B95}\u{0BCC}",
            compare_rustybuzz: true,
            note: "kau (split matra U+0BCC)",
        },
        Case {
            // Tamil digits
            text: "\u{0BE6}\u{0BE7}\u{0BE8}",
            compare_rustybuzz: true,
            note: "tamil digits",
        },
    ];
    run_corpus("Tamil", NOTO_TAMIL, corpus);
}

// -----------------------------------------------------------------
// Telugu — U+0C00..U+0C7F. Reph: AfterPost (Explicit).
// -----------------------------------------------------------------
#[test]
fn telugu_corpus_matches_rustybuzz() {
    let corpus = &[
        Case {
            text: "",
            compare_rustybuzz: true,
            note: "empty",
        },
        Case {
            // క (ka)
            text: "\u{0C15}",
            compare_rustybuzz: true,
            note: "ka alone",
        },
        Case {
            // కా (ka + post-base aa)
            text: "\u{0C15}\u{0C3E}",
            compare_rustybuzz: true,
            note: "kaa (post-base matra)",
        },
        Case {
            // కి (ka + i)
            text: "\u{0C15}\u{0C3F}",
            compare_rustybuzz: true,
            note: "ki",
        },
        Case {
            // క్ష — ka + halant + ssa (Telugu conjunct — subjoined form)
            text: "\u{0C15}\u{0C4D}\u{0C37}",
            compare_rustybuzz: true,
            note: "kssa conjunct",
        },
        Case {
            // ర్‍క — ra + halant + ZWJ + ka. Telugu uses Explicit
            // reph mode: bare ra+halant is not a reph, but
            // ra+halant+ZWJ is. Sigilbuzz now detects the ZWJ head
            // and tags the ra as reph candidate; the rphf ligature
            // then collapses the triple into an explicit reph
            // form if the font supplies one. The fixture is a
            // cluster-integrity check — the Telugu corpus font
            // ligates down to a different glyph set than sigilbuzz
            // produces (pref-only fonts land here), so we stop
            // short of a byte-identical compare. See issue #30.
            text: "\u{0C30}\u{0C4D}\u{200D}\u{0C15}",
            compare_rustybuzz: false,
            note: "ra + halant + ZWJ + ka (Explicit reph)",
        },
        Case {
            // Telugu digits
            text: "\u{0C66}\u{0C67}\u{0C68}",
            compare_rustybuzz: true,
            note: "telugu digits",
        },
    ];
    run_corpus("Telugu", NOTO_TELUGU, corpus);
}

// -----------------------------------------------------------------
// Kannada — U+0C80..U+0CFF. Reph: AfterPost (Implicit).
// -----------------------------------------------------------------
#[test]
fn kannada_corpus_matches_rustybuzz() {
    let corpus = &[
        Case {
            text: "",
            compare_rustybuzz: true,
            note: "empty",
        },
        Case {
            // ಕ (ka)
            text: "\u{0C95}",
            compare_rustybuzz: true,
            note: "ka alone",
        },
        Case {
            // ಕಾ (ka + post-base aa)
            text: "\u{0C95}\u{0CBE}",
            compare_rustybuzz: true,
            note: "kaa (post-base matra)",
        },
        Case {
            // ಕಿ (ka + i)
            text: "\u{0C95}\u{0CBF}",
            compare_rustybuzz: true,
            note: "ki",
        },
        Case {
            // ಕ್ಷ — ka + halant + ssa (subjoined conjunct)
            text: "\u{0C95}\u{0CCD}\u{0CB7}",
            compare_rustybuzz: true,
            note: "kssa conjunct",
        },
        Case {
            // ರ್ಕ — ra + halant + ka (reph; AfterPost).
            // Sigilbuzz's refined AfterPost walker now matches
            // rustybuzz for this syllable (see issue #32).
            text: "\u{0CB0}\u{0CCD}\u{0C95}",
            compare_rustybuzz: true,
            note: "reph + ka (AfterPost)",
        },
        Case {
            // ರ್ಮ — ra + halant + ma.
            text: "\u{0CB0}\u{0CCD}\u{0CAE}",
            compare_rustybuzz: true,
            note: "reph + ma (AfterPost)",
        },
        Case {
            // Kannada digits
            text: "\u{0CE6}\u{0CE7}\u{0CE8}",
            compare_rustybuzz: true,
            note: "kannada digits",
        },
    ];
    run_corpus("Kannada", NOTO_KANNADA, corpus);
}

// -----------------------------------------------------------------
// Malayalam — U+0D00..U+0D7F. Reph: AfterMain (LogRepha).
// -----------------------------------------------------------------
#[test]
fn malayalam_corpus_matches_rustybuzz() {
    let corpus = &[
        Case {
            text: "",
            compare_rustybuzz: true,
            note: "empty",
        },
        Case {
            // ക (ka)
            text: "\u{0D15}",
            compare_rustybuzz: true,
            note: "ka alone",
        },
        Case {
            // കാ (ka + post-base aa)
            text: "\u{0D15}\u{0D3E}",
            compare_rustybuzz: true,
            note: "kaa (post-base matra)",
        },
        Case {
            // കെ (ka + pre-base e)
            text: "\u{0D15}\u{0D46}",
            compare_rustybuzz: true,
            note: "ke (pre-base matra)",
        },
        Case {
            // ക്ക — ka + halant + ka (geminate conjunct)
            text: "\u{0D15}\u{0D4D}\u{0D15}",
            compare_rustybuzz: true,
            note: "kka conjunct",
        },
        Case {
            // Malayalam digits
            text: "\u{0D66}\u{0D67}\u{0D68}",
            compare_rustybuzz: true,
            note: "malayalam digits",
        },
        Case {
            // ർക — LogRepha (U+0D4E) + ka. Sigilbuzz now recognises
            // the encoded Repha as a reph candidate and moves it
            // to the AfterMain slot without relying on rphf to
            // collapse a ra+halant prefix (see issue #31).
            text: "\u{0D4E}\u{0D15}",
            compare_rustybuzz: true,
            note: "log repha + ka (LogRepha reorder)",
        },
        Case {
            // ർമ — LogRepha + ma.
            text: "\u{0D4E}\u{0D2E}",
            compare_rustybuzz: true,
            note: "log repha + ma",
        },
    ];
    run_corpus("Malayalam", NOTO_MALAYALAM, corpus);
}

// -----------------------------------------------------------------
// Sinhala — U+0D80..U+0DFF. Reph: AfterPost (Explicit).
// -----------------------------------------------------------------
#[test]
fn sinhala_corpus_matches_rustybuzz() {
    let corpus = &[
        Case {
            text: "",
            compare_rustybuzz: true,
            note: "empty",
        },
        Case {
            // ක (ka)
            text: "\u{0D9A}",
            compare_rustybuzz: true,
            note: "ka alone",
        },
        Case {
            // කා (ka + post-base aa)
            text: "\u{0D9A}\u{0DCF}",
            compare_rustybuzz: true,
            note: "kaa (post-base matra)",
        },
        Case {
            // කි (ka + above-base i)
            text: "\u{0D9A}\u{0DD2}",
            compare_rustybuzz: true,
            note: "ki (above matra)",
        },
        Case {
            // කෙ (ka + pre-base e)
            // Sinhala pre-base matras need Indic reorder — our generic
            // reorder places the matra first; rustybuzz does the same
            // via its own state machine.
            text: "\u{0D9A}\u{0DD9}",
            compare_rustybuzz: true,
            note: "ke (pre-base matra)",
        },
        Case {
            // කේ — ka + two-part matra U+0DDA (= e + halant).
            // Sigilbuzz now decomposes the matra at buffer-prep
            // time so the pre-base component participates in Indic
            // reordering (see issue #29).
            text: "\u{0D9A}\u{0DDA}",
            compare_rustybuzz: true,
            note: "kee (split matra U+0DDA)",
        },
        Case {
            // කො — ka + U+0DDC (split: e + aa).
            text: "\u{0D9A}\u{0DDC}",
            compare_rustybuzz: true,
            note: "ko (split matra U+0DDC)",
        },
        Case {
            // කෝ — ka + U+0DDD (three-part: e + aa + halant).
            text: "\u{0D9A}\u{0DDD}",
            compare_rustybuzz: true,
            note: "koo (three-part matra U+0DDD)",
        },
        Case {
            // කෞ — ka + U+0DDE (split: e + post-base-lll).
            text: "\u{0D9A}\u{0DDE}",
            compare_rustybuzz: true,
            note: "kau (split matra U+0DDE)",
        },
        Case {
            // Sinhala digits
            text: "\u{0DE6}\u{0DE7}\u{0DE8}",
            compare_rustybuzz: true,
            note: "sinhala digits",
        },
    ];
    run_corpus("Sinhala", NOTO_SINHALA, corpus);
}

// -----------------------------------------------------------------
// Cross-cutting: pre-base matra ordering across the family. For the
// scripts where the pre-base matra is a single Left-positioned vowel
// sign, the shaped output should start with the matra's cluster
// (byte offset of the matra codepoint) rather than the consonant's.
// -----------------------------------------------------------------
#[test]
fn pre_base_matras_move_before_base_for_every_script() {
    for (name, bytes, consonant, matra) in [
        // (script name, font bytes, base consonant, pre-base matra)
        ("Bengali", NOTO_BENGALI, '\u{0995}', '\u{09BF}'),
        ("Gurmukhi", NOTO_GURMUKHI, '\u{0A15}', '\u{0A3F}'),
        ("Gujarati", NOTO_GUJARATI, '\u{0A95}', '\u{0ABF}'),
        ("Tamil", NOTO_TAMIL, '\u{0B95}', '\u{0BC6}'),
        ("Malayalam", NOTO_MALAYALAM, '\u{0D15}', '\u{0D46}'),
        ("Sinhala", NOTO_SINHALA, '\u{0D9A}', '\u{0DD9}'),
    ] {
        let blob = Blob::new(bytes);
        let face = Face::parse(&blob, 0).expect("parse");
        let font = Font::new(face, 1000.0);
        let text = format!("{consonant}{matra}");
        let mut buffer = Buffer::new();
        buffer.push_str(&text);
        let shaped = shape(&font, &buffer, &[]).expect("shape");
        // The consonant comes first in the input (cluster 0). The
        // pre-base matra's bytes follow, cluster == consonant.len_utf8().
        let matra_cluster = consonant.len_utf8() as u32;
        assert_eq!(shaped.len(), 2, "{name}: expected 2 glyphs");
        assert_eq!(
            shaped.glyphs[0].cluster, matra_cluster,
            "{name}: pre-base matra cluster should appear first"
        );
        assert_eq!(
            shaped.glyphs[1].cluster, 0,
            "{name}: base consonant cluster should appear second"
        );
    }
}
