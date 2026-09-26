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
//!   `General_Category`, and `Extended_Pictographic` from
//!   `emoji-data.txt`, for HarfBuzz's grapheme and native-direction
//!   rules.
//!
//! # Sources
//!
//! The committed snapshots under `tests/tools/ucd/` are the only
//! inputs. Each starts with `#` lines naming its source URL, the
//! retrieval date, and the version lines of the original file, and
//! keeps only the data the generator reads, with trailing comments
//! removed:
//!
//! - `ArabicShaping.txt`: every data line.
//! - `DerivedGeneralCategory.txt`: the Lu, Ll, Lt, Lm, Lo, Mn, Mc, Me,
//!   Nd, and Cf lines.
//! - `BidiMirroring.txt`: every data line (the commented-out list of
//!   mirrored characters without a mirror glyph is dropped).
//! - `Scripts.txt`: every data line.
//! - `PropertyValueAliases.txt`: the `sc` (Script) lines.
//! - `emoji-data.txt`: the `Extended_Pictographic` lines.
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
//! directory holding the six files as downloaded from
//! `https://www.unicode.org/Public/<version>/ucd/`
//! (`DerivedGeneralCategory.txt` is under `extracted/` there and
//! `emoji-data.txt` under `emoji/`), with `SIGILBUZZ_UCD_VERSION` (for
//! example `17.0.0`) and `SIGILBUZZ_UCD_RETRIEVED=YYYY-MM-DD` set.
//!
//! The non-ignored test in this file regenerates every table in memory
//! and fails when a committed file has drifted from the snapshots.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

const ARABIC_SHAPING: &str = "ArabicShaping.txt";
const GENERAL_CATEGORY: &str = "DerivedGeneralCategory.txt";
const BIDI_MIRRORING: &str = "BidiMirroring.txt";
const SCRIPTS: &str = "Scripts.txt";
const ALIASES: &str = "PropertyValueAliases.txt";
const EMOJI_DATA: &str = "emoji-data.txt";

const JOINING_RS: &str = "src/unicode/joining_table.rs";
const MIRRORING_RS: &str = "src/unicode/mirroring_table.rs";
const SCRIPT_RS: &str = "crates/sigilbuzz-capi/src/script_table.rs";
const CATEGORY_RS: &str = "src/unicode/general_category_table.rs";

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
    for row in &categories.rows {
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

// --- Snapshot refresh ------------------------------------------------------------

/// Reduces a downloaded UCD file to a snapshot: the provenance lines,
/// then the data lines `keep` accepts, trailing comments removed.
fn reduce(raw: &str, url: &str, retrieved: &str, keep: impl Fn(&[&str]) -> bool) -> String {
    let mut out = format!("# Source: {url}\n# Retrieved: {retrieved}\n");
    for line in raw.lines().take(2) {
        out.push_str(line.trim_end());
        out.push('\n');
    }
    for line in raw.lines() {
        let data = line.split('#').next().unwrap_or("").trim();
        if data.is_empty() {
            continue;
        }
        let fields: Vec<&str> = data.split(';').map(str::trim).collect();
        if keep(&fields) {
            out.push_str(data);
            out.push('\n');
        }
    }
    out
}

/// Which data lines of a downloaded file a snapshot keeps, by field.
type Keep = fn(&[&str]) -> bool;

fn refresh_snapshots() {
    let Ok(dir) = std::env::var("SIGILBUZZ_UCD_DIR") else {
        return;
    };
    let version = std::env::var("SIGILBUZZ_UCD_VERSION").expect("set SIGILBUZZ_UCD_VERSION");
    let retrieved =
        std::env::var("SIGILBUZZ_UCD_RETRIEVED").expect("set SIGILBUZZ_UCD_RETRIEVED=YYYY-MM-DD");
    let base = format!("https://www.unicode.org/Public/{version}/ucd");
    let jobs: [(&str, String, Keep); 6] = [
        (ARABIC_SHAPING, format!("{base}/{ARABIC_SHAPING}"), |_| true),
        (
            GENERAL_CATEGORY,
            format!("{base}/extracted/{GENERAL_CATEGORY}"),
            |f| f.get(1).is_some_and(|gc| KEPT_CATEGORIES.contains(gc)),
        ),
        (BIDI_MIRRORING, format!("{base}/{BIDI_MIRRORING}"), |_| true),
        (SCRIPTS, format!("{base}/{SCRIPTS}"), |_| true),
        (ALIASES, format!("{base}/{ALIASES}"), |f| {
            f.first() == Some(&"sc")
        }),
        (EMOJI_DATA, format!("{base}/emoji/{EMOJI_DATA}"), |f| {
            f.get(1) == Some(&"Extended_Pictographic")
        }),
    ];
    std::fs::create_dir_all(snapshot_dir()).expect("create snapshot dir");
    for (file, url, keep) in jobs {
        let raw = read(&Path::new(&dir).join(file));
        let snapshot = reduce(&raw, &url, &retrieved, keep);
        std::fs::write(snapshot_dir().join(file), snapshot).expect("write snapshot");
    }
}

fn outputs() -> [(&'static str, String); 4] {
    [
        (JOINING_RS, generate_joining()),
        (MIRRORING_RS, generate_mirroring()),
        (SCRIPT_RS, generate_scripts()),
        (CATEGORY_RS, generate_categories()),
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
