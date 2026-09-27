//! Generator for `src/ot/syllabic/table.rs`, the character categories
//! and positions of HarfBuzz's Indic-family shapers.
//!
//! HarfBuzz derives the table in `hb-ot-shaper-indic-table.cc` with
//! `gen-indic-table.py` from `IndicSyllabicCategory.txt`,
//! `IndicPositionalCategory.txt`, and `Blocks.txt`. This file ports
//! that script's rules (the category and position maps, the per-code
//! point overrides, and the per-script matra positions) as of
//! HarfBuzz 14.5.0. The Myanmar blocks, the Myanmar-only categories,
//! and the variation selectors are left out: sigilbuzz's Myanmar
//! shaper does not read this table, and a code point the table leaves
//! out gets category `X`, which the Indic and Khmer syllable grammars
//! treat the way they treat those categories.
//!
//! # Sources
//!
//! The committed snapshots under `tests/tools/ucd/` are the only
//! inputs. Each keeps its provenance lines and the data lines of the
//! blocks the table covers, with trailing comments removed.
//!
//! # Commands
//!
//! Regenerate the table from the snapshots:
//!
//! ```text
//! cargo test --test indic_table_gen -- --ignored
//! ```
//!
//! Refresh the snapshots first by pointing `SIGILBUZZ_UCD_DIR` at a
//! directory holding the three files as downloaded from
//! `https://www.unicode.org/Public/<version>/ucd/`, with
//! `SIGILBUZZ_UCD_VERSION` (for example `17.0.0`) and
//! `SIGILBUZZ_UCD_RETRIEVED=YYYY-MM-DD` set.
//!
//! The non-ignored test regenerates the table in memory and fails when
//! the committed file has drifted from the snapshots.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

const SYLLABIC: &str = "IndicSyllabicCategory.txt";
const POSITIONAL: &str = "IndicPositionalCategory.txt";
const BLOCKS: &str = "Blocks.txt";

const TABLE_RS: &str = "src/ot/syllabic/table.rs";

/// Maximum emitted line width, matching the crate's rustfmt setting.
const MAX_WIDTH: usize = 100;

/// A gap longer than this between two listed code points starts a new
/// range in the emitted table.
const MAX_GAP: u32 = 32;

/// `ALLOWED_SINGLES` of `gen-indic-table.py`.
const ALLOWED_SINGLES: &[u32] = &[0x00A0, 0x25CC];

/// `ALLOWED_BLOCKS` of `gen-indic-table.py`, without the Myanmar
/// blocks.
const ALLOWED_BLOCKS: &[&str] = &[
    "Basic Latin",
    "Latin-1 Supplement",
    "Devanagari",
    "Bengali",
    "Gurmukhi",
    "Gujarati",
    "Oriya",
    "Tamil",
    "Telugu",
    "Kannada",
    "Malayalam",
    "Khmer",
    "Vedic Extensions",
    "General Punctuation",
    "Superscripts and Subscripts",
    "Devanagari Extended",
];

/// Blocks the snapshots keep beyond [`ALLOWED_BLOCKS`]: those of the
/// override code points outside them.
const OVERRIDE_BLOCKS: &[&str] = &["Geometric Shapes", "Grantha"];

/// `category_map` of `gen-indic-table.py`.
fn category_of(isc: &str) -> &'static str {
    match isc {
        "Other" => "X",
        "Avagraha" => "Symbol",
        "Bindu" => "SM",
        "Brahmi_Joining_Number" => "PLACEHOLDER",
        "Cantillation_Mark" => "A",
        "Consonant" => "C",
        "Consonant_Dead" => "C",
        "Consonant_Final" => "CM",
        "Consonant_Head_Letter" => "C",
        "Consonant_Initial_Postfixed" => "C",
        "Consonant_Killer" => "M",
        "Consonant_Medial" => "CM",
        "Consonant_Placeholder" => "PLACEHOLDER",
        "Consonant_Preceding_Repha" => "Repha",
        "Consonant_Prefixed" => "X",
        "Consonant_Subjoined" => "CM",
        "Consonant_Succeeding_Repha" => "CM",
        "Consonant_With_Stacker" => "CS",
        "Gemination_Mark" => "SM",
        "Invisible_Stacker" => "H",
        "Joiner" => "ZWJ",
        "Modifying_Letter" => "X",
        "Non_Joiner" => "ZWNJ",
        "Nukta" => "N",
        "Number" => "PLACEHOLDER",
        "Number_Joiner" => "PLACEHOLDER",
        "Pure_Killer" => "M",
        "Register_Shifter" => "RS",
        "Syllable_Modifier" => "SM",
        "Tone_Letter" => "X",
        "Tone_Mark" => "N",
        "Virama" => "H",
        "Visarga" => "SM",
        "Vowel" => "V",
        "Vowel_Dependent" => "M",
        "Vowel_Independent" => "V",
        other => panic!("unknown Indic_Syllabic_Category {other}"),
    }
}

/// `position_map` of `gen-indic-table.py`.
fn position_of(ipc: &str) -> &'static str {
    match ipc {
        "Not_Applicable" => "END",
        "Left" => "PRE_C",
        "Top" => "ABOVE_C",
        "Bottom" => "BELOW_C",
        "Right" => "POST_C",
        "Bottom_And_Right" => "POST_C",
        "Left_And_Right" => "POST_C",
        "Top_And_Bottom" => "BELOW_C",
        "Top_And_Bottom_And_Left" => "BELOW_C",
        "Top_And_Bottom_And_Right" => "POST_C",
        "Top_And_Left" => "ABOVE_C",
        "Top_And_Left_And_Right" => "POST_C",
        "Top_And_Right" => "POST_C",
        "Overstruck" => "AFTER_MAIN",
        "Visual_order_left" => "PRE_M",
        other => panic!("unknown Indic_Positional_Category {other}"),
    }
}

/// `category_overrides` of `gen-indic-table.py`, without the Myanmar
/// entries and the variation selectors.
const CATEGORY_OVERRIDES: &[(u32, &str)] = &[
    (0x2015, "PLACEHOLDER"),
    (0x2022, "PLACEHOLDER"),
    (0x25FB, "PLACEHOLDER"),
    (0x25FC, "PLACEHOLDER"),
    (0x25FD, "PLACEHOLDER"),
    (0x25FE, "PLACEHOLDER"),
    (0x0930, "Ra"),
    (0x09B0, "Ra"),
    (0x09F0, "Ra"),
    (0x0A30, "Ra"),
    (0x0AB0, "Ra"),
    (0x0B30, "Ra"),
    (0x0BB0, "Ra"),
    (0x0C30, "Ra"),
    (0x0CB0, "Ra"),
    (0x0D30, "Ra"),
    (0x0953, "SM"),
    (0x0954, "SM"),
    (0x0A40, "MPst"),
    (0x0A72, "C"),
    (0x0A73, "C"),
    (0x1CE2, "A"),
    (0x1CE3, "A"),
    (0x1CE4, "A"),
    (0x1CE5, "A"),
    (0x1CE6, "A"),
    (0x1CE7, "A"),
    (0x1CE8, "A"),
    (0x1CED, "A"),
    (0xA8F2, "Symbol"),
    (0xA8F3, "Symbol"),
    (0xA8F4, "Symbol"),
    (0xA8F5, "Symbol"),
    (0xA8F6, "Symbol"),
    (0xA8F7, "Symbol"),
    (0x1CE9, "Symbol"),
    (0x1CEA, "Symbol"),
    (0x1CEB, "Symbol"),
    (0x1CEC, "Symbol"),
    (0x1CEE, "Symbol"),
    (0x1CEF, "Symbol"),
    (0x1CF0, "Symbol"),
    (0x1CF1, "Symbol"),
    (0x0A51, "M"),
    (0x11301, "SM"),
    (0x11302, "SM"),
    (0x11303, "SM"),
    (0x1133B, "N"),
    (0x1133C, "N"),
    (0x0AFB, "N"),
    (0x0B55, "N"),
    (0x09FC, "PLACEHOLDER"),
    (0x0C80, "PLACEHOLDER"),
    (0x0D04, "PLACEHOLDER"),
    (0x25CC, "DOTTEDCIRCLE"),
    (0x179A, "Ra"),
    (0x17CC, "Robatic"),
    (0x17C9, "Robatic"),
    (0x17CA, "Robatic"),
    (0x17C6, "Xgroup"),
    (0x17CB, "Xgroup"),
    (0x17CD, "Xgroup"),
    (0x17CE, "Xgroup"),
    (0x17CF, "Xgroup"),
    (0x17D0, "Xgroup"),
    (0x17D1, "Xgroup"),
    (0x17C7, "Ygroup"),
    (0x17C8, "Ygroup"),
    (0x17DD, "Ygroup"),
    (0x17D3, "Ygroup"),
    (0x17D9, "PLACEHOLDER"),
];

/// `position_overrides` of `gen-indic-table.py`.
const POSITION_OVERRIDES: &[(u32, &str)] = &[(0x0A51, "BELOW_C"), (0x0B01, "BEFORE_SUB")];

/// `matra_pos_left` to `matra_pos_bottom` of `gen-indic-table.py`.
fn matra_position(u: u32, pos: &str, block: &str) -> &'static str {
    match pos {
        "PRE_C" => "PRE_M",
        "POST_C" => match block {
            "Devanagari" => "AFTER_SUB",
            "Bengali" | "Gurmukhi" | "Gujarati" | "Oriya" | "Tamil" | "Malayalam" => "AFTER_POST",
            "Telugu" if u <= 0x0C42 => "BEFORE_SUB",
            "Telugu" => "AFTER_SUB",
            "Kannada" if !(0x0CC3..=0x0CD6).contains(&u) => "BEFORE_SUB",
            "Kannada" => "AFTER_SUB",
            _ => "AFTER_SUB",
        },
        "ABOVE_C" => match block {
            "Gurmukhi" => "AFTER_POST",
            "Oriya" => "AFTER_MAIN",
            "Telugu" | "Kannada" => "BEFORE_SUB",
            _ => "AFTER_SUB",
        },
        "BELOW_C" => match block {
            "Gurmukhi" | "Gujarati" | "Tamil" | "Malayalam" => "AFTER_POST",
            "Telugu" | "Kannada" => "BEFORE_SUB",
            _ => "AFTER_SUB",
        },
        other => panic!("matra at {u:04X} with position {other}"),
    }
}

/// `position_to_category` of `gen-indic-table.py`.
fn position_category(pos: &str) -> &'static str {
    match pos {
        "PRE_C" => "VPre",
        "ABOVE_C" => "VAbv",
        "BELOW_C" => "VBlw",
        "POST_C" => "VPst",
        other => panic!("Khmer matra with position {other}"),
    }
}

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

/// A snapshot: its provenance header and its `(range, value)` rows.
struct Snapshot {
    header: Vec<String>,
    rows: Vec<((u32, u32), String)>,
}

fn parse_range(field: &str) -> (u32, u32) {
    let hex = |s: &str| u32::from_str_radix(s, 16).unwrap_or_else(|e| panic!("{s:?}: {e}"));
    match field.split_once("..") {
        Some((a, b)) => (hex(a), hex(b)),
        None => (hex(field), hex(field)),
    }
}

fn parse(text: &str) -> Snapshot {
    let mut header = Vec::new();
    let mut rows = Vec::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix('#') {
            header.push(rest.trim().to_owned());
        } else if let Some((range, value)) = line.split_once(';') {
            rows.push((parse_range(range.trim()), value.trim().to_owned()));
        }
    }
    Snapshot { header, rows }
}

fn load(file: &str) -> Snapshot {
    parse(&read(&snapshot_dir().join(file)))
}

/// Every code point of each row, with the row's value.
fn per_code_point(snapshot: &Snapshot) -> BTreeMap<u32, String> {
    let mut out = BTreeMap::new();
    for ((start, end), value) in &snapshot.rows {
        for u in *start..=*end {
            out.insert(u, value.clone());
        }
    }
    out
}

/// `(category, position)` for every code point the table lists, as
/// `gen-indic-table.py` computes `indic_data`.
fn indic_data() -> BTreeMap<u32, (&'static str, &'static str)> {
    let syllabic = per_code_point(&load(SYLLABIC));
    let positional = per_code_point(&load(POSITIONAL));
    let blocks = per_code_point(&load(BLOCKS));
    let block_of = |u: u32| blocks.get(&u).map_or("No_Block", String::as_str);

    // `combined`: the code points either category file lists, in the
    // allowed blocks or singles.
    let mut data: BTreeMap<u32, (&str, &str, String)> = BTreeMap::new();
    for &u in syllabic.keys().chain(positional.keys()) {
        let block = block_of(u);
        if !(ALLOWED_SINGLES.contains(&u) || ALLOWED_BLOCKS.contains(&block)) {
            continue;
        }
        let isc = syllabic.get(&u).map_or("Other", String::as_str);
        let ipc = positional.get(&u).map_or("Not_Applicable", String::as_str);
        let mut cat = category_of(isc);
        if cat == "SM" && ipc == "Not_Applicable" {
            cat = "SMPst";
        }
        data.insert(u, (cat, position_of(ipc), block.to_owned()));
    }
    for &(u, cat) in CATEGORY_OVERRIDES {
        let pos = data.get(&u).map_or("END", |d| d.1);
        data.insert(u, (cat, pos, block_of(u).to_owned()));
    }
    let positioned = ["CM", "SM", "RS", "H", "M", "MPst"];
    let consonants = ["C", "CS", "Ra", "CM", "V", "PLACEHOLDER", "DOTTEDCIRCLE"];
    let matras = ["M", "MPst"];
    let smvd = ["SM", "SMPst", "VD", "A", "Symbol"];
    let mut out = BTreeMap::new();
    for (&u, (cat, pos, block)) in &data {
        let (mut cat, mut pos) = (*cat, *pos);
        if !positioned.contains(&cat) {
            pos = "END";
        }
        if consonants.contains(&cat) {
            pos = "BASE_C";
        } else if matras.contains(&cat) {
            if block.starts_with("Khmer") {
                cat = position_category(pos);
            } else {
                pos = matra_position(u, pos, block);
            }
        } else if smvd.contains(&cat) {
            pos = "SMVD";
        }
        out.insert(u, (cat, pos));
    }
    for &(u, pos) in POSITION_OVERRIDES {
        let cat = out.get(&u).map_or("X", |d| d.0);
        out.insert(u, (cat, pos));
    }
    out
}

/// The generated source of `src/ot/syllabic/table.rs`.
fn generate() -> String {
    let data = indic_data();
    let mut out = String::new();
    out.push_str("// Generated by `cargo test --test indic_table_gen -- --ignored`.\n");
    out.push_str("// Do not edit by hand; see `tests/indic_table_gen.rs`.\n");
    for file in [SYLLABIC, POSITIONAL, BLOCKS] {
        out.push_str("//\n");
        for line in &load(file).header {
            let _ = writeln!(out, "// {line}");
        }
    }
    out.push('\n');
    out.push_str("// Code point ranges read best in hex without digit separators.\n");
    out.push_str("#![allow(clippy::unreadable_literal)]\n\n");
    let mut cats: Vec<String> = data.values().map(|d| d.0.to_uppercase()).collect();
    let mut positions: Vec<String> = data.values().map(|d| d.1.to_owned()).collect();
    cats.push("X".to_owned());
    positions.push("END".to_owned());
    for (module, names) in [("cat", &mut cats), ("pos", &mut positions)] {
        names.sort_unstable();
        names.dedup();
        let _ = writeln!(out, "use super::{module}::{{");
        let mut line = String::from("   ");
        for name in names.iter() {
            if line.len() + 1 + name.len() + 1 > MAX_WIDTH {
                out.push_str(&line);
                out.push('\n');
                line = String::from("   ");
            }
            line.push(' ');
            line.push_str(name);
            line.push(',');
        }
        out.push_str(&line);
        out.push_str("\n};\n");
    }
    out.push_str("use super::pack as p;\n\n");
    out.push_str("/// `(first code point, entries)`: the packed category and position of\n");
    out.push_str("/// each code point from the first on. Sorted, non-overlapping. Code\n");
    out.push_str("/// points no range covers, and the gaps inside a range, are `X` at\n");
    out.push_str("/// `END`.\n");
    out.push_str("pub(super) static RANGES: &[(u32, &[u16])] = &[\n");
    let points: Vec<u32> = data.keys().copied().collect();
    let mut groups: Vec<Vec<u32>> = Vec::new();
    for &u in &points {
        match groups.last_mut() {
            Some(g) if u - g.last().copied().unwrap_or(u) <= MAX_GAP => g.push(u),
            _ => groups.push(vec![u]),
        }
    }
    for group in groups {
        let (first, last) = (group[0], group[group.len() - 1]);
        let _ = writeln!(out, "    (\n        0x{first:04X},\n        &[");
        let items: Vec<String> = (first..=last)
            .map(|u| {
                let (cat, pos) = data.get(&u).copied().unwrap_or(("X", "END"));
                format!("p({}, {pos})", cat.to_uppercase())
            })
            .collect();
        let mut line = String::from("           ");
        for item in items {
            if line.len() + 1 + item.len() + 1 > MAX_WIDTH {
                out.push_str(&line);
                out.push('\n');
                line = String::from("           ");
            }
            line.push(' ');
            line.push_str(&item);
            line.push(',');
        }
        out.push_str(&line);
        out.push('\n');
        out.push_str("        ],\n    ),\n");
    }
    out.push_str("];\n");
    out
}

// --- Snapshot refresh --------------------------------------------------------

fn refresh_snapshots() {
    let Ok(dir) = std::env::var("SIGILBUZZ_UCD_DIR") else {
        return;
    };
    let version = std::env::var("SIGILBUZZ_UCD_VERSION").expect("set SIGILBUZZ_UCD_VERSION");
    let retrieved =
        std::env::var("SIGILBUZZ_UCD_RETRIEVED").expect("set SIGILBUZZ_UCD_RETRIEVED=YYYY-MM-DD");
    let base = format!("https://www.unicode.org/Public/{version}/ucd");
    let blocks_raw = read(&Path::new(&dir).join(BLOCKS));
    let blocks = parse(&strip_comments(&blocks_raw));
    let kept_blocks: Vec<(u32, u32)> = blocks
        .rows
        .iter()
        .filter(|(_, name)| {
            ALLOWED_BLOCKS.contains(&name.as_str()) || OVERRIDE_BLOCKS.contains(&name.as_str())
        })
        .map(|(range, _)| *range)
        .collect();
    let in_kept =
        |(start, end): (u32, u32)| kept_blocks.iter().any(|&(s, e)| start <= e && s <= end);
    for file in [SYLLABIC, POSITIONAL, BLOCKS] {
        let raw = read(&Path::new(&dir).join(file));
        let mut out = format!("# Source: {base}/{file}\n# Retrieved: {retrieved}\n");
        for line in raw.lines().take(2) {
            out.push_str(line.trim_end());
            out.push('\n');
        }
        for line in raw.lines() {
            let data = line.split('#').next().unwrap_or("").trim();
            let Some((range, value)) = data.split_once(';') else {
                continue;
            };
            let range = range.trim();
            if in_kept(parse_range(range)) {
                let _ = writeln!(out, "{range}; {}", value.trim());
            }
        }
        std::fs::write(snapshot_dir().join(file), out).expect("write snapshot");
    }
}

/// `text` without its comment lines.
fn strip_comments(text: &str) -> String {
    text.lines()
        .map(|l| l.split('#').next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
#[ignore = "writes the generated Indic table; run explicitly to regenerate"]
fn regenerate_indic_table() {
    refresh_snapshots();
    std::fs::write(root().join(TABLE_RS), generate()).expect("write table");
}

#[test]
fn committed_indic_table_matches_snapshots() {
    let committed = read(&root().join(TABLE_RS));
    assert!(
        committed == generate(),
        "{TABLE_RS} is stale; run `cargo test --test indic_table_gen -- --ignored`"
    );
}

#[test]
fn snapshots_derive_known_categories() {
    let data = indic_data();
    // DEVANAGARI LETTER KA, LETTER RA, SIGN VIRAMA, VOWEL SIGN I.
    assert_eq!(data[&0x0915], ("C", "BASE_C"));
    assert_eq!(data[&0x0930], ("Ra", "BASE_C"));
    assert_eq!(data[&0x094D], ("H", "BELOW_C"));
    assert_eq!(data[&0x093F], ("M", "PRE_M"));
    // KHMER SIGN COENG, VOWEL SIGN E, SIGN ROBAT, SIGN NIKAHIT.
    assert_eq!(data[&0x17D2], ("H", "END"));
    assert_eq!(data[&0x17C1], ("VPre", "PRE_C"));
    assert_eq!(data[&0x17CC], ("Robatic", "END"));
    assert_eq!(data[&0x17C6], ("Xgroup", "END"));
    // KANNADA VOWEL SIGN U, VOWEL SIGN VOCALIC R.
    assert_eq!(data[&0x0CC1], ("M", "BEFORE_SUB"));
    assert_eq!(data[&0x0CC3], ("M", "AFTER_SUB"));
    assert_eq!(data[&0x200D], ("ZWJ", "END"));
    assert_eq!(data[&0x25CC], ("DOTTEDCIRCLE", "BASE_C"));
}
