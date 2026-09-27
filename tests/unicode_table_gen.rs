//! Generator for the tables sigilbuzz derives from the Unicode
//! Character Database:
//!
//! - `src/unicode/joining_table.rs`: joining types, from
//!   `ArabicShaping.txt` plus `General_Category` (HarfBuzz's
//!   `get_joining_type`: a code point the file does not list is
//!   Transparent when it is Mn, Me, or Cf, and Non_Joining otherwise).
//! - `src/unicode/mirroring_table.rs`: `Bidi_Mirroring_Glyph`, from
//!   `BidiMirroring.txt`.
//! - `crates/sigilbuzz-capi/src/script_table.rs`: the `Script`
//!   property as ISO 15924 codes, from `Scripts.txt` and the `sc`
//!   rows of `PropertyValueAliases.txt`, for
//!   `hb_buffer_guess_segment_properties`.
//! - `src/unicode/general_category_table.rs`: the letter (L*), mark
//!   (Mn, Mc, Me), and decimal number (Nd) ranges of
//!   `General_Category`, the nonspacing mark (Mn) ranges on their own,
//!   and `Extended_Pictographic` from `emoji-data.txt`, for HarfBuzz's
//!   grapheme and native-direction rules, its synthesized glyph
//!   classes, and its fallback mark positioning.
//! - `src/unicode/normalize/decompose_table.rs`: the canonical
//!   `Decomposition_Mapping` of every character, from
//!   `UnicodeData.txt`.
//! - `src/unicode/normalize/compose_table.rs`: the primary composites,
//!   every two-character canonical decomposition that is not a full
//!   composition exclusion: not listed in `CompositionExclusions.txt`,
//!   and neither the composite nor the first character of its full
//!   decomposition has a nonzero `Canonical_Combining_Class` (UAX #15
//!   "non-starter decompositions"; singletons never compose).
//! - `src/unicode/normalize/combining_class_table.rs`: the nonzero
//!   `Canonical_Combining_Class` ranges, from
//!   `DerivedCombiningClass.txt`.
//! - `src/unicode/bidi_class_table.rs`: `Bidi_Class`, from
//!   `DerivedBidiClass.txt`, with the defaults its `@missing` lines give
//!   the code points it does not list (R, AL, or ET in the blocks set
//!   aside for them, L everywhere else).
//! - `src/unicode/bidi_brackets_table.rs`: `Bidi_Paired_Bracket` and
//!   `Bidi_Paired_Bracket_Type`, from `BidiBrackets.txt`.
//!
//! # Sources
//!
//! The committed snapshots under `tests/tools/ucd/` are the only
//! inputs. Each starts with `#` lines naming its source URL, the
//! retrieval date, and the version lines of the original file (a
//! synthesized version line for `UnicodeData.txt`, which has none),
//! and keeps only the data the generator reads, with trailing
//! comments removed:
//!
//! - `ArabicShaping.txt`: every data line.
//! - `DerivedGeneralCategory.txt`: the Lu, Ll, Lt, Lm, Lo, Mn, Mc, Me,
//!   Nd, and Cf lines.
//! - `BidiMirroring.txt`: every data line (the commented-out list of
//!   mirrored characters without a mirror glyph is dropped).
//! - `Scripts.txt`: every data line.
//! - `PropertyValueAliases.txt`: the `sc` (Script) lines.
//! - `emoji-data.txt`: the `Extended_Pictographic` lines.
//! - `UnicodeData.txt`: `code point;Decomposition_Mapping` for every
//!   character with a canonical (untagged) decomposition.
//! - `DerivedCombiningClass.txt`: the lines with a nonzero class.
//! - `CompositionExclusions.txt`: every data line.
//! - `DerivedBidiClass.txt`: every data line, and the `@missing` lines
//!   with their leading `# ` removed, so they read as data.
//! - `BidiBrackets.txt`: every data line.
//!
//! # Commands
//!
//! Regenerate the tables from the snapshots:
//!
//! ```text
//! cargo test --test unicode_table_gen -- --ignored
//! ```
//!
//! Refresh the snapshots first by pointing `SIGILBUZZ_UCD_DIR` at a
//! directory holding the eleven files as downloaded from
//! `https://www.unicode.org/Public/<version>/ucd/`
//! (`DerivedGeneralCategory.txt`, `DerivedCombiningClass.txt`, and
//! `DerivedBidiClass.txt` are under `extracted/` there and
//! `emoji-data.txt` under `emoji/`), with
//! `SIGILBUZZ_UCD_VERSION` (for example `17.0.0`) and
//! `SIGILBUZZ_UCD_RETRIEVED=YYYY-MM-DD` set.
//!
//! The non-ignored test in this file regenerates every table in memory
//! and fails when a committed file has drifted from the snapshots.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

const ARABIC_SHAPING: &str = "ArabicShaping.txt";
const GENERAL_CATEGORY: &str = "DerivedGeneralCategory.txt";
const BIDI_MIRRORING: &str = "BidiMirroring.txt";
const SCRIPTS: &str = "Scripts.txt";
const ALIASES: &str = "PropertyValueAliases.txt";
const EMOJI_DATA: &str = "emoji-data.txt";
const UNICODE_DATA: &str = "UnicodeData.txt";
const COMBINING_CLASS: &str = "DerivedCombiningClass.txt";
const COMPOSITION_EXCLUSIONS: &str = "CompositionExclusions.txt";
const BIDI_CLASS: &str = "DerivedBidiClass.txt";
const BIDI_BRACKETS: &str = "BidiBrackets.txt";

const JOINING_RS: &str = "src/unicode/joining_table.rs";
const MIRRORING_RS: &str = "src/unicode/mirroring_table.rs";
const SCRIPT_RS: &str = "crates/sigilbuzz-capi/src/script_table.rs";
const CATEGORY_RS: &str = "src/unicode/general_category_table.rs";
const DECOMPOSE_RS: &str = "src/unicode/normalize/decompose_table.rs";
const COMPOSE_RS: &str = "src/unicode/normalize/compose_table.rs";
const COMBINING_CLASS_RS: &str = "src/unicode/normalize/combining_class_table.rs";
const BIDI_CLASS_RS: &str = "src/unicode/bidi_class_table.rs";
const BIDI_BRACKETS_RS: &str = "src/unicode/bidi_brackets_table.rs";

/// The General_Category values the snapshot keeps.
const KEPT_CATEGORIES: &[&str] = &["Lu", "Ll", "Lt", "Lm", "Lo", "Mn", "Mc", "Me", "Nd", "Cf"];

/// Maximum emitted line width, matching the crate's rustfmt setting.
const MAX_WIDTH: usize = 100;

/// One past the last code point.
const CODE_SPACE: usize = 0x11_0000;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn snapshot_dir() -> PathBuf {
    root().join("tests").join("tools").join("ucd")
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
        .replace("\r\n", "\n")
}

/// A snapshot: its provenance header and its data lines.
struct Snapshot {
    header: Vec<String>,
    rows: Vec<Vec<String>>,
}

fn load(file: &str) -> Snapshot {
    let text = read(&snapshot_dir().join(file));
    let mut header = Vec::new();
    let mut rows = Vec::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix('#') {
            header.push(rest.trim().to_owned());
        } else if !line.trim().is_empty() {
            rows.push(line.split(';').map(|f| f.trim().to_owned()).collect());
        }
    }
    Snapshot { header, rows }
}

/// Parses `XXXX` or `XXXX..YYYY`.
fn parse_range(field: &str) -> (u32, u32) {
    let hex = |s: &str| u32::from_str_radix(s, 16).unwrap_or_else(|e| panic!("{s:?}: {e}"));
    match field.split_once("..") {
        Some((a, b)) => (hex(a), hex(b)),
        None => (hex(field), hex(field)),
    }
}

/// Appends `items` as a comma-separated list wrapped to `MAX_WIDTH`
/// columns with a four-space indent.
fn emit_wrapped(out: &mut String, items: &[String]) {
    let mut line = String::from("   ");
    for item in items {
        if line.len() + 1 + item.len() + 1 > MAX_WIDTH {
            out.push_str(&line);
            out.push('\n');
            line = String::from("   ");
        }
        line.push(' ');
        line.push_str(item);
        line.push(',');
    }
    if !line.trim().is_empty() {
        out.push_str(&line);
        out.push('\n');
    }
}

fn file_header(out: &mut String, sources: &[&Snapshot]) {
    out.push_str("// Generated by `cargo test --test unicode_table_gen -- --ignored`.\n");
    out.push_str("// Do not edit by hand; see `tests/unicode_table_gen.rs`.\n");
    for source in sources {
        out.push_str("//\n");
        for line in &source.header {
            let _ = writeln!(out, "// {line}");
        }
    }
    out.push('\n');
}

/// Collapses a per-code-point property into `(start, end, value)`
/// runs, skipping runs of `skip`.
fn runs<T: Copy + PartialEq>(values: &[T], skip: T) -> Vec<(u32, u32, T)> {
    let mut out: Vec<(u32, u32, T)> = Vec::new();
    for (cp, &value) in values.iter().enumerate() {
        let cp = cp as u32;
        match out.last_mut() {
            Some(last) if last.2 == value && last.1 + 1 == cp => last.1 = cp,
            _ if value == skip => {}
            _ => out.push((cp, cp, value)),
        }
    }
    out
}

// --- Joining types -----------------------------------------------------------

fn generate_joining() -> String {
    let shaping = load(ARABIC_SHAPING);
    let categories = load(GENERAL_CATEGORY);
    let mut types = vec!['U'; CODE_SPACE];
    for row in &categories.rows {
        if !matches!(row[1].as_str(), "Mn" | "Me" | "Cf") {
            continue;
        }
        let (start, end) = parse_range(&row[0]);
        for cp in start..=end {
            types[cp as usize] = 'T';
        }
    }
    for row in &shaping.rows {
        let (cp, _) = parse_range(&row[0]);
        // HarfBuzz gives the ALAPH and DALATH RISH joining groups their
        // own Syriac states; sigilbuzz has no Syriac shaper, so they
        // keep their joining type (R).
        let jt = row[2].chars().next().expect("joining type");
        assert!("UTRDCL".contains(jt), "{row:?}");
        types[cp as usize] = jt;
    }

    let mut out = String::new();
    file_header(&mut out, &[&shaping, &categories]);
    out.push_str("// Code point ranges read best in hex without digit separators.\n");
    out.push_str("#![allow(clippy::unreadable_literal)]\n\n");
    out.push_str("use super::joining::JoiningType::{self, C, D, L, R, T};\n\n");
    out.push_str("/// Every code point whose joining type is not U (Non_Joining):\n");
    out.push_str("/// the ArabicShaping.txt entries, and T (Transparent) for the other\n");
    out.push_str("/// Mn, Me, and Cf code points. Sorted, non-overlapping, inclusive.\n");
    out.push_str("pub(super) static JOINING_TYPES: &[(u32, u32, JoiningType)] = &[\n");
    let items: Vec<String> = runs(&types, 'U')
        .iter()
        .map(|(s, e, t)| format!("(0x{s:04X}, 0x{e:04X}, {t})"))
        .collect();
    emit_wrapped(&mut out, &items);
    out.push_str("];\n");
    out
}

// --- Bidi mirroring ----------------------------------------------------------

fn generate_mirroring() -> String {
    let mirroring = load(BIDI_MIRRORING);
    let mut pairs: Vec<(u32, u32)> = mirroring
        .rows
        .iter()
        .map(|row| (parse_range(&row[0]).0, parse_range(&row[1]).0))
        .collect();
    pairs.sort_unstable();
    pairs.dedup_by_key(|p| p.0);
    assert_eq!(pairs.len(), mirroring.rows.len(), "duplicate code points");

    let mut out = String::new();
    file_header(&mut out, &[&mirroring]);
    out.push_str("// Code points read best in hex without digit separators.\n");
    out.push_str("#![allow(clippy::unreadable_literal)]\n\n");
    out.push_str("/// `(code point, Bidi_Mirroring_Glyph)` for every code point that has\n");
    out.push_str("/// a mirroring glyph. Sorted by code point.\n");
    out.push_str("pub(super) static MIRRORING: &[(u32, u32)] = &[\n");
    let items: Vec<String> = pairs
        .iter()
        .map(|(a, b)| format!("(0x{a:04X}, 0x{b:04X})"))
        .collect();
    emit_wrapped(&mut out, &items);
    out.push_str("];\n");
    out
}

// --- Scripts -----------------------------------------------------------------

fn generate_scripts() -> String {
    let scripts = load(SCRIPTS);
    let aliases = load(ALIASES);
    // Long property value name to ISO 15924 code.
    let codes: BTreeMap<&str, &str> = aliases
        .rows
        .iter()
        .filter(|row| row[0] == "sc")
        .map(|row| (row[2].as_str(), row[1].as_str()))
        .collect();
    let mut tags: Vec<&str> = codes.values().copied().collect();
    tags.sort_unstable();
    tags.dedup();
    let index: BTreeMap<&str, u8> = tags
        .iter()
        .enumerate()
        .map(|(i, tag)| (*tag, u8::try_from(i).expect("fewer than 256 scripts")))
        .collect();
    let unknown = index["Zzzz"];
    let mut values = vec![unknown; CODE_SPACE];
    for row in &scripts.rows {
        let (start, end) = parse_range(&row[0]);
        let code = codes
            .get(row[1].as_str())
            .unwrap_or_else(|| panic!("no ISO 15924 code for {:?}", row[1]));
        for cp in start..=end {
            values[cp as usize] = index[code];
        }
    }

    let mut out = String::new();
    file_header(&mut out, &[&scripts, &aliases]);
    out.push_str("// Code point ranges read best in hex without digit separators.\n");
    out.push_str("#![allow(clippy::unreadable_literal)]\n\n");
    out.push_str("/// ISO 15924 codes of every script value, indexed by the third field\n");
    out.push_str("/// of [`SCRIPT_RANGES`]. Sorted.\n");
    out.push_str("pub(crate) static SCRIPT_TAGS: &[[u8; 4]] = &[\n");
    let items: Vec<String> = tags.iter().map(|t| format!("*b\"{t}\"")).collect();
    emit_wrapped(&mut out, &items);
    out.push_str("];\n\n");
    let _ = writeln!(
        out,
        "/// Index of `Zzzz` (Unknown) in [`SCRIPT_TAGS`], the script of every\n\
         /// code point [`SCRIPT_RANGES`] does not cover.\n\
         pub(crate) const UNKNOWN: u8 = {unknown};\n"
    );
    out.push_str("/// `(first, last, script)` for every code point with a known script,\n");
    out.push_str("/// `script` indexing [`SCRIPT_TAGS`]. Sorted, non-overlapping.\n");
    out.push_str("pub(crate) static SCRIPT_RANGES: &[(u32, u32, u8)] = &[\n");
    let items: Vec<String> = runs(&values, unknown)
        .iter()
        .map(|(s, e, t)| format!("(0x{s:04X}, 0x{e:04X}, {t})"))
        .collect();
    emit_wrapped(&mut out, &items);
    out.push_str("];\n");
    out
}

// --- General categories and Extended_Pictographic -------------------------------

fn generate_categories() -> String {
    let categories = load(GENERAL_CATEGORY);
    let emoji = load(EMOJI_DATA);
    let mut classes = vec![' '; CODE_SPACE];
    let mut nonspacing = vec![false; CODE_SPACE];
    for row in &categories.rows {
        if row[1] == "Mn" {
            let (start, end) = parse_range(&row[0]);
            for cp in start..=end {
                nonspacing[cp as usize] = true;
            }
        }
        let class = match row[1].as_str() {
            "Lu" | "Ll" | "Lt" | "Lm" | "Lo" => 'L',
            "Mn" | "Mc" | "Me" => 'M',
            "Nd" => 'N',
            _ => continue,
        };
        let (start, end) = parse_range(&row[0]);
        for cp in start..=end {
            classes[cp as usize] = class;
        }
    }
    let mut pictographic = vec![false; CODE_SPACE];
    for row in &emoji.rows {
        assert_eq!(row[1], "Extended_Pictographic", "{row:?}");
        let (start, end) = parse_range(&row[0]);
        for cp in start..=end {
            pictographic[cp as usize] = true;
        }
    }

    let mut out = String::new();
    file_header(&mut out, &[&categories, &emoji]);
    out.push_str("// Code point ranges read best in hex without digit separators.\n");
    out.push_str("#![allow(clippy::unreadable_literal)]\n\n");
    out.push_str("use super::general_category::GeneralCategoryClass::{self, DecimalNumber, Letter, Mark};\n\n");
    out.push_str("/// Letters (Lu, Ll, Lt, Lm, Lo), marks (Mn, Mc, Me), and decimal\n");
    out.push_str("/// numbers (Nd). Sorted, non-overlapping, inclusive.\n");
    out.push_str("pub(super) static CLASSES: &[(u32, u32, GeneralCategoryClass)] = &[\n");
    let items: Vec<String> = runs(&classes, ' ')
        .iter()
        .map(|(s, e, c)| {
            let name = match c {
                'L' => "Letter",
                'M' => "Mark",
                _ => "DecimalNumber",
            };
            format!("(0x{s:04X}, 0x{e:04X}, {name})")
        })
        .collect();
    emit_wrapped(&mut out, &items);
    out.push_str("];\n\n");
    out.push_str("/// Nonspacing marks (Mn). Sorted, non-overlapping, inclusive.\n");
    out.push_str("pub(super) static NONSPACING_MARKS: &[(u32, u32)] = &[\n");
    let items: Vec<String> = runs(&nonspacing, false)
        .iter()
        .map(|(s, e, _)| format!("(0x{s:04X}, 0x{e:04X})"))
        .collect();
    emit_wrapped(&mut out, &items);
    out.push_str("];\n\n");
    out.push_str("/// `Extended_Pictographic` ranges. Sorted, non-overlapping, inclusive.\n");
    out.push_str("pub(super) static EXTENDED_PICTOGRAPHIC: &[(u32, u32)] = &[\n");
    let items: Vec<String> = runs(&pictographic, false)
        .iter()
        .map(|(s, e, _)| format!("(0x{s:04X}, 0x{e:04X})"))
        .collect();
    emit_wrapped(&mut out, &items);
    out.push_str("];\n");
    out
}

// --- Canonical normalization -------------------------------------------------------

fn parse_hex(field: &str) -> u32 {
    u32::from_str_radix(field, 16).unwrap_or_else(|e| panic!("{field:?}: {e}"))
}

/// Every canonical `Decomposition_Mapping`: code point to its one or
/// two characters.
fn decompositions() -> BTreeMap<u32, Vec<u32>> {
    load(UNICODE_DATA)
        .rows
        .iter()
        .map(|row| {
            let mapping: Vec<u32> = row[1].split_whitespace().map(parse_hex).collect();
            assert!(matches!(mapping.len(), 1 | 2), "{row:?}");
            (parse_hex(&row[0]), mapping)
        })
        .collect()
}

/// `Canonical_Combining_Class` of every code point.
fn combining_classes() -> Vec<u8> {
    let mut classes = vec![0u8; CODE_SPACE];
    for row in &load(COMBINING_CLASS).rows {
        let (start, end) = parse_range(&row[0]);
        let class: u8 = row[1].parse().unwrap_or_else(|e| panic!("{row:?}: {e}"));
        assert_ne!(class, 0, "{row:?}");
        for cp in start..=end {
            classes[cp as usize] = class;
        }
    }
    classes
}

/// The primary composites: `(first, second, composite)` for every
/// two-character canonical decomposition that is not a full
/// composition exclusion. Sorted by the pair.
fn primary_composites() -> Vec<(u32, u32, u32)> {
    let map = decompositions();
    let classes = combining_classes();
    let mut excluded = BTreeSet::new();
    for row in &load(COMPOSITION_EXCLUSIONS).rows {
        let (start, end) = parse_range(&row[0]);
        excluded.extend(start..=end);
    }
    // The first character of a full (recursive) decomposition.
    let first = |mut cp: u32| {
        while let Some(mapping) = map.get(&cp) {
            cp = mapping[0];
        }
        cp
    };
    let mut pairs: Vec<(u32, u32, u32)> = map
        .iter()
        .filter(|(cp, m)| {
            m.len() == 2
                && !excluded.contains(*cp)
                && classes[**cp as usize] == 0
                && classes[first(m[0]) as usize] == 0
        })
        .map(|(cp, m)| (m[0], m[1], *cp))
        .collect();
    pairs.sort_unstable();
    pairs
}

fn generate_decompositions() -> String {
    let data = load(UNICODE_DATA);
    let mut out = String::new();
    file_header(&mut out, &[&data]);
    out.push_str("// Code points read best in hex without digit separators.\n");
    out.push_str("#![allow(clippy::unreadable_literal)]\n\n");
    out.push_str("/// `(code point, first, second)` for every canonical\n");
    out.push_str("/// `Decomposition_Mapping`, `second` zero for a singleton. Hangul\n");
    out.push_str("/// syllables decompose algorithmically and are not listed. Sorted.\n");
    out.push_str("pub(super) static DECOMPOSITIONS: &[(u32, u32, u32)] = &[\n");
    let items: Vec<String> = decompositions()
        .iter()
        .map(|(cp, m)| {
            let second = m.get(1).copied().unwrap_or(0);
            format!("(0x{cp:04X}, 0x{:04X}, 0x{second:04X})", m[0])
        })
        .collect();
    emit_wrapped(&mut out, &items);
    out.push_str("];\n");
    out
}

fn generate_compositions() -> String {
    let data = load(UNICODE_DATA);
    let classes = load(COMBINING_CLASS);
    let exclusions = load(COMPOSITION_EXCLUSIONS);
    let mut out = String::new();
    file_header(&mut out, &[&data, &classes, &exclusions]);
    out.push_str("// Code points read best in hex without digit separators.\n");
    out.push_str("#![allow(clippy::unreadable_literal)]\n\n");
    out.push_str("/// `(first, second, composite)` for every primary composite: a\n");
    out.push_str("/// two-character canonical decomposition that is not a full\n");
    out.push_str("/// composition exclusion. Hangul syllables compose algorithmically\n");
    out.push_str("/// and are not listed. Sorted by `(first, second)`.\n");
    out.push_str("pub(super) static COMPOSITIONS: &[(u32, u32, u32)] = &[\n");
    let items: Vec<String> = primary_composites()
        .iter()
        .map(|(a, b, c)| format!("(0x{a:04X}, 0x{b:04X}, 0x{c:04X})"))
        .collect();
    emit_wrapped(&mut out, &items);
    out.push_str("];\n");
    out
}

fn generate_combining_classes() -> String {
    let snapshot = load(COMBINING_CLASS);
    let mut out = String::new();
    file_header(&mut out, &[&snapshot]);
    out.push_str("// Code point ranges read best in hex without digit separators.\n");
    out.push_str("#![allow(clippy::unreadable_literal)]\n\n");
    out.push_str("/// `(first, last, class)` for every code point whose\n");
    out.push_str("/// `Canonical_Combining_Class` is not zero. Sorted, non-overlapping,\n");
    out.push_str("/// inclusive.\n");
    out.push_str("pub(super) static COMBINING_CLASSES: &[(u32, u32, u8)] = &[\n");
    let items: Vec<String> = runs(&combining_classes(), 0)
        .iter()
        .map(|(s, e, c)| format!("(0x{s:04X}, 0x{e:04X}, {c})"))
        .collect();
    emit_wrapped(&mut out, &items);
    out.push_str("];\n");
    out
}

// --- Bidi classes ------------------------------------------------------------------

/// The `BidiClass` variant for a Bidi_Class value, short (`AL`) or long
/// (`Arabic_Letter`).
fn bidi_variant(value: &str) -> &'static str {
    match value {
        "L" | "Left_To_Right" => "L",
        "R" | "Right_To_Left" => "R",
        "AL" | "Arabic_Letter" => "Al",
        "EN" | "European_Number" => "En",
        "ES" | "European_Separator" => "Es",
        "ET" | "European_Terminator" => "Et",
        "AN" | "Arabic_Number" => "An",
        "CS" | "Common_Separator" => "Cs",
        "NSM" | "Nonspacing_Mark" => "Nsm",
        "BN" | "Boundary_Neutral" => "Bn",
        "B" | "Paragraph_Separator" => "B",
        "S" | "Segment_Separator" => "S",
        "WS" | "White_Space" => "Ws",
        "ON" | "Other_Neutral" => "On",
        "LRE" | "Left_To_Right_Embedding" => "Lre",
        "LRO" | "Left_To_Right_Override" => "Lro",
        "RLE" | "Right_To_Left_Embedding" => "Rle",
        "RLO" | "Right_To_Left_Override" => "Rlo",
        "PDF" | "Pop_Directional_Format" => "Pdf",
        "LRI" | "Left_To_Right_Isolate" => "Lri",
        "RLI" | "Right_To_Left_Isolate" => "Rli",
        "FSI" | "First_Strong_Isolate" => "Fsi",
        "PDI" | "Pop_Directional_Isolate" => "Pdi",
        _ => panic!("unknown Bidi_Class {value:?}"),
    }
}

/// `Bidi_Class` of every code point: the `@missing` defaults in file
/// order (each overrides the ones before it, as UAX #44 says), then the
/// listed values.
fn bidi_classes() -> Vec<&'static str> {
    let snapshot = load(BIDI_CLASS);
    let mut classes = vec!["L"; CODE_SPACE];
    let (missing, listed): (Vec<_>, Vec<_>) = snapshot
        .rows
        .iter()
        .partition(|row| row[0].starts_with("@missing:"));
    assert!(!missing.is_empty(), "no @missing lines in {BIDI_CLASS}");
    for row in missing.into_iter().chain(listed) {
        let range = row[0].trim_start_matches("@missing:").trim();
        let (start, end) = parse_range(range);
        let class = bidi_variant(&row[1]);
        for cp in start..=end {
            classes[cp as usize] = class;
        }
    }
    classes
}

fn generate_bidi_classes() -> String {
    let snapshot = load(BIDI_CLASS);
    let classes = bidi_classes();
    let mut used: Vec<&str> = classes.iter().copied().filter(|&c| c != "L").collect();
    used.sort_unstable();
    used.dedup();

    let mut out = String::new();
    file_header(&mut out, &[&snapshot]);
    out.push_str("// Code point ranges read best in hex without digit separators.\n");
    out.push_str("#![allow(clippy::unreadable_literal)]\n\n");
    out.push_str("use super::bidi_class::BidiClass::{\n");
    let names: Vec<String> = ["self"]
        .into_iter()
        .chain(used)
        .map(str::to_owned)
        .collect();
    emit_wrapped(&mut out, &names);
    out.push_str("};\n\n");
    out.push_str("/// `(first, last, class)` for every code point whose `Bidi_Class`\n");
    out.push_str("/// is not L (Left_To_Right), unassigned code points included.\n");
    out.push_str("/// Sorted, non-overlapping, inclusive. A `const` so that\n");
    out.push_str("/// `bidi_class` can stay a `const fn`.\n");
    out.push_str("pub(super) const BIDI_CLASSES: &[(u32, u32, BidiClass)] = &[\n");
    let items: Vec<String> = runs(&classes, "L")
        .iter()
        .map(|(s, e, c)| format!("(0x{s:04X}, 0x{e:04X}, {c})"))
        .collect();
    emit_wrapped(&mut out, &items);
    out.push_str("];\n");
    out
}

/// `(code point, paired bracket, open)` for every `BidiBrackets.txt`
/// entry. Sorted by code point.
fn bidi_brackets() -> Vec<(u32, u32, bool)> {
    let mut pairs: Vec<(u32, u32, bool)> = load(BIDI_BRACKETS)
        .rows
        .iter()
        .map(|row| {
            let open = match row[2].as_str() {
                "o" => true,
                "c" => false,
                other => panic!("bracket type {other:?} in {row:?}"),
            };
            (parse_hex(&row[0]), parse_hex(&row[1]), open)
        })
        .collect();
    pairs.sort_unstable();
    let count = pairs.len();
    pairs.dedup_by_key(|p| p.0);
    assert_eq!(pairs.len(), count, "duplicate code points");
    pairs
}

fn generate_bidi_brackets() -> String {
    let snapshot = load(BIDI_BRACKETS);
    let mut out = String::new();
    file_header(&mut out, &[&snapshot]);
    out.push_str("// Code points read best in hex without digit separators.\n");
    out.push_str("#![allow(clippy::unreadable_literal)]\n\n");
    out.push_str("use super::bidi_brackets::BracketType::{self, Close, Open};\n\n");
    out.push_str("/// `(code point, Bidi_Paired_Bracket, Bidi_Paired_Bracket_Type)`\n");
    out.push_str("/// for every paired bracket. Sorted by code point. A `const` so\n");
    out.push_str("/// that `bracket_of` can stay a `const fn`.\n");
    out.push_str("pub(super) const BRACKETS: &[(u32, u32, BracketType)] = &[\n");
    let items: Vec<String> = bidi_brackets()
        .iter()
        .map(|&(cp, pair, open)| {
            let kind = if open { "Open" } else { "Close" };
            format!("(0x{cp:04X}, 0x{pair:04X}, {kind})")
        })
        .collect();
    emit_wrapped(&mut out, &items);
    out.push_str("];\n");
    out
}

// --- Snapshot refresh ------------------------------------------------------------

/// Reduces a downloaded UCD file to a snapshot: the provenance lines,
/// then what `keep` keeps of each data line, trailing comments
/// removed. A file without version lines (`UnicodeData.txt`) gets
/// `version_line` instead.
fn reduce(raw: &str, url: &str, retrieved: &str, version_line: &str, keep: Keep) -> String {
    let mut out = format!("# Source: {url}\n# Retrieved: {retrieved}\n");
    if raw.starts_with('#') {
        for line in raw.lines().take(2) {
            out.push_str(line.trim_end());
            out.push('\n');
        }
    } else {
        let _ = writeln!(out, "# {version_line}");
    }
    for line in raw.lines() {
        let data = line.split('#').next().unwrap_or("").trim();
        if data.is_empty() {
            continue;
        }
        let fields: Vec<&str> = data.split(';').map(str::trim).collect();
        if let Some(kept) = keep(data, &fields) {
            out.push_str(&kept);
            out.push('\n');
        }
    }
    out
}

/// What a snapshot keeps of one data line of a downloaded file, given
/// the line and its fields.
type Keep = fn(&str, &[&str]) -> Option<String>;

fn keep_all(data: &str, _: &[&str]) -> Option<String> {
    Some(data.to_owned())
}

fn refresh_snapshots() {
    let Ok(dir) = std::env::var("SIGILBUZZ_UCD_DIR") else {
        return;
    };
    let version = std::env::var("SIGILBUZZ_UCD_VERSION").expect("set SIGILBUZZ_UCD_VERSION");
    let retrieved =
        std::env::var("SIGILBUZZ_UCD_RETRIEVED").expect("set SIGILBUZZ_UCD_RETRIEVED=YYYY-MM-DD");
    let base = format!("https://www.unicode.org/Public/{version}/ucd");
    let jobs: [(&str, String, Keep); 11] = [
        (ARABIC_SHAPING, format!("{base}/{ARABIC_SHAPING}"), keep_all),
        (
            GENERAL_CATEGORY,
            format!("{base}/extracted/{GENERAL_CATEGORY}"),
            |d, f| {
                f.get(1)
                    .is_some_and(|gc| KEPT_CATEGORIES.contains(gc))
                    .then(|| d.to_owned())
            },
        ),
        (BIDI_MIRRORING, format!("{base}/{BIDI_MIRRORING}"), keep_all),
        (SCRIPTS, format!("{base}/{SCRIPTS}"), keep_all),
        (ALIASES, format!("{base}/{ALIASES}"), |d, f| {
            (f.first() == Some(&"sc")).then(|| d.to_owned())
        }),
        (EMOJI_DATA, format!("{base}/emoji/{EMOJI_DATA}"), |d, f| {
            (f.get(1) == Some(&"Extended_Pictographic")).then(|| d.to_owned())
        }),
        (UNICODE_DATA, format!("{base}/{UNICODE_DATA}"), |_, f| {
            let mapping = f.get(5)?;
            (!mapping.is_empty() && !mapping.starts_with('<'))
                .then(|| format!("{};{mapping}", f[0]))
        }),
        (
            COMBINING_CLASS,
            format!("{base}/extracted/{COMBINING_CLASS}"),
            |d, f| (f.get(1) != Some(&"0")).then(|| d.to_owned()),
        ),
        (
            COMPOSITION_EXCLUSIONS,
            format!("{base}/{COMPOSITION_EXCLUSIONS}"),
            keep_all,
        ),
        (
            BIDI_CLASS,
            format!("{base}/extracted/{BIDI_CLASS}"),
            keep_all,
        ),
        (BIDI_BRACKETS, format!("{base}/{BIDI_BRACKETS}"), keep_all),
    ];
    let version_line = format!("{UNICODE_DATA}, Unicode {version}");
    std::fs::create_dir_all(snapshot_dir()).expect("create snapshot dir");
    for (file, url, keep) in jobs {
        let mut raw = read(&Path::new(&dir).join(file));
        if file == BIDI_CLASS {
            // The defaults for unlisted code points sit in comment
            // lines. Keep them as data.
            raw = raw.replace("# @missing: ", "@missing: ");
        }
        let snapshot = reduce(&raw, &url, &retrieved, &version_line, keep);
        std::fs::write(snapshot_dir().join(file), snapshot).expect("write snapshot");
    }
}

fn outputs() -> [(&'static str, String); 9] {
    [
        (JOINING_RS, generate_joining()),
        (MIRRORING_RS, generate_mirroring()),
        (SCRIPT_RS, generate_scripts()),
        (CATEGORY_RS, generate_categories()),
        (DECOMPOSE_RS, generate_decompositions()),
        (COMPOSE_RS, generate_compositions()),
        (COMBINING_CLASS_RS, generate_combining_classes()),
        (BIDI_CLASS_RS, generate_bidi_classes()),
        (BIDI_BRACKETS_RS, generate_bidi_brackets()),
    ]
}

#[test]
#[ignore = "writes the generated UCD tables; run explicitly to regenerate"]
fn regenerate_unicode_tables() {
    refresh_snapshots();
    for (path, table) in outputs() {
        std::fs::write(root().join(path), table).expect("write table");
    }
}

#[test]
fn committed_unicode_tables_match_snapshots() {
    for (path, expected) in outputs() {
        let committed = read(&root().join(path));
        assert!(
            committed == expected,
            "{path} is stale; run `cargo test --test unicode_table_gen -- --ignored`"
        );
    }
}

#[test]
fn snapshots_parse_known_rows() {
    let shaping = load(ARABIC_SHAPING);
    let beh = shaping.rows.iter().find(|r| r[0] == "0628").expect("beh");
    assert_eq!(beh[2], "D");
    let mirroring = load(BIDI_MIRRORING);
    assert!(mirroring
        .rows
        .iter()
        .any(|r| r[0] == "0028" && r[1] == "0029"));
    let aliases = load(ALIASES);
    assert!(aliases
        .rows
        .iter()
        .any(|r| r[1] == "Arab" && r[2] == "Arabic"));
}

#[test]
fn normalization_snapshots_derive_known_mappings() {
    let map = decompositions();
    // LATIN CAPITAL LETTER A WITH GRAVE, ANGSTROM SIGN (a singleton).
    assert_eq!(map[&0x00C0], [0x0041, 0x0300]);
    assert_eq!(map[&0x212B], [0x00C5]);
    let classes = combining_classes();
    assert_eq!(classes[0x0301], 230);
    assert_eq!(classes[0x05B0], 10);
    assert_eq!(classes[0x0041], 0);
    let composites = primary_composites();
    let composes = |a: u32, b: u32| {
        composites
            .binary_search_by_key(&(a, b), |&(x, y, _)| (x, y))
            .ok()
            .map(|i| composites[i].2)
    };
    assert_eq!(composes(0x0065, 0x0301), Some(0x00E9));
    // DEVANAGARI LETTER QA is a composition exclusion; COMBINING GREEK
    // DIALYTIKA TONOS and TIBETAN VOWEL SIGN II decompose to a
    // non-starter first.
    assert_eq!(composes(0x0915, 0x093C), None);
    assert_eq!(composes(0x0308, 0x0301), None);
    assert_eq!(composes(0x0F71, 0x0F72), None);
    // Singletons never compose.
    assert!(composites.iter().all(|&(_, _, c)| c != 0x212B));
}

#[test]
fn bidi_snapshot_derives_known_classes() {
    let classes = bidi_classes();
    for (cp, class) in [
        (0x0041, "L"),
        (0x05D0, "R"),
        (0x0627, "Al"),
        (0x06F1, "En"),
        (0x0901, "Nsm"),
        (0x2029, "B"),
        (0x2067, "Rli"),
        // @missing defaults: an unassigned code point in the Hebrew, the
        // Arabic, and the Currency Symbols blocks, and outside them.
        (0x05FF, "R"),
        (0x07BF, "Al"),
        (0x20CF, "Et"),
        (0x50000, "L"),
        // Unassigned default ignorables and noncharacters are listed.
        (0x2065, "Bn"),
        (0xFDD0, "Bn"),
    ] {
        assert_eq!(classes[cp], class, "U+{cp:04X}");
    }
}

#[test]
fn bracket_snapshot_derives_known_pairs() {
    let pairs = bidi_brackets();
    assert_eq!(pairs.len(), 128);
    for (cp, pair, open) in [
        (0x0028, 0x0029, true),
        (0x0029, 0x0028, false),
        (0x2329, 0x232A, true),
        (0x3009, 0x3008, false),
        (0x0F3A, 0x0F3B, true),
        (0x2E5C, 0x2E5B, false),
    ] {
        assert!(pairs.contains(&(cp, pair, open)), "U+{cp:04X}");
    }
}
