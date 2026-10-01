//! Generator for `src/ot/use_shaper/table.rs`, the character
//! categories of HarfBuzz's Universal Shaping Engine.
//!
//! HarfBuzz derives the table in `hb-ot-shaper-use-table.hh` with
//! `gen-use-table.py` from `IndicSyllabicCategory.txt`,
//! `IndicPositionalCategory.txt`, `ArabicShaping.txt`,
//! `DerivedCoreProperties.txt`, `UnicodeData.txt`, `Scripts.txt`, and
//! the two Microsoft override files in its `ms-use/` directory. This
//! file ports that script's rules (the overrides, the category
//! predicates, and the positional suffixes) as of HarfBuzz 14.5.0,
//! whose table reads the Unicode 18.0.0 files, except
//! `DerivedCoreProperties.txt`, which is 17.0.0 there. The snapshots
//! keep those versions, so the generated table matches HarfBuzz's for
//! every code point.
//!
//! # Sources
//!
//! The committed snapshots under `tests/tools/ucd/use/` are the only
//! inputs. Each starts with `#` lines naming its source and version
//! and keeps only the data the generator reads, as `range; value`
//! rows with trailing comments removed:
//!
//! - `IndicSyllabicCategory.txt`, `IndicPositionalCategory.txt`: every
//!   data line.
//! - `ArabicShaping.txt`: the code point and its joining type.
//! - `DerivedCoreProperties.txt`: the `Default_Ignorable_Code_Point`
//!   lines.
//! - `UnicodeData.txt`: the General_Category of each code point the
//!   other files list, in runs of consecutive code points.
//! - `Scripts.txt`: the lines of the scripts `gen-use-table.py` leaves
//!   out (Arabic, Lao, Samaritan, Syriac, Thai).
//! - `IndicSyllabicCategory-Additional.txt` and
//!   `IndicPositionalCategory-Additional.txt`: HarfBuzz's `ms-use/`
//!   files (MIT License, Copyright (c) Microsoft Corporation), their
//!   header and data lines.
//!
//! # Commands
//!
//! Regenerate the table from the snapshots:
//!
//! ```text
//! cargo test --test use_table_gen -- --ignored
//! ```
//!
//! Refresh the snapshots first by pointing `SIGILBUZZ_UCD_DIR` at a
//! directory holding the six UCD files of the versions HarfBuzz uses,
//! as downloaded from `https://www.unicode.org/Public/<version>/ucd/`,
//! and `SIGILBUZZ_MS_USE_DIR` at HarfBuzz's `src/ms-use/`, with
//! `SIGILBUZZ_UCD_VERSION` (the version of `UnicodeData.txt`, which
//! names none), `SIGILBUZZ_HARFBUZZ_VERSION`, and
//! `SIGILBUZZ_UCD_RETRIEVED=YYYY-MM-DD` set.
//!
//! The non-ignored test regenerates the table in memory and fails when
//! the committed file has drifted from the snapshots.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

const SYLLABIC: &str = "IndicSyllabicCategory.txt";
const POSITIONAL: &str = "IndicPositionalCategory.txt";
const ARABIC_SHAPING: &str = "ArabicShaping.txt";
const CORE_PROPERTIES: &str = "DerivedCoreProperties.txt";
const UNICODE_DATA: &str = "UnicodeData.txt";
const SCRIPTS: &str = "Scripts.txt";
const SYLLABIC_ADDITIONAL: &str = "IndicSyllabicCategory-Additional.txt";
const POSITIONAL_ADDITIONAL: &str = "IndicPositionalCategory-Additional.txt";

/// The snapshots, in the order the table header lists them.
const FILES: [&str; 8] = [
    SYLLABIC,
    POSITIONAL,
    ARABIC_SHAPING,
    CORE_PROPERTIES,
    UNICODE_DATA,
    SCRIPTS,
    SYLLABIC_ADDITIONAL,
    POSITIONAL_ADDITIONAL,
];

const TABLE_RS: &str = "src/ot/use_shaper/table.rs";

/// Maximum emitted line width, matching the crate's rustfmt setting.
const MAX_WIDTH: usize = 100;

/// `DISABLED_SCRIPTS` of `gen-use-table.py`.
const DISABLED_SCRIPTS: &[&str] = &["Arabic", "Lao", "Samaritan", "Syriac", "Thai"];

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn snapshot_dir() -> PathBuf {
    root().join("tests").join("tools").join("ucd").join("use")
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

/// The properties `gen-use-table.py` reads for one code point.
struct Props<'a> {
    u: u32,
    isc: &'a str,
    ipc: &'a str,
    jt: &'a str,
    di: bool,
    gc: &'a str,
}

impl Props<'_> {
    fn isc_in(&self, values: &[&str]) -> bool {
        values.contains(&self.isc)
    }

    fn is_base(&self) -> bool {
        self.isc_in(&[
            "Number",
            "Consonant",
            "Consonant_Head_Letter",
            "Tone_Letter",
            "Vowel_Independent",
        ]) || (["C", "D", "L", "R"].contains(&self.jt) && self.isc != "Joiner")
            || (self.gc == "Lo"
                && self.isc_in(&[
                    "Avagraha",
                    "Bindu",
                    "Consonant_Final",
                    "Consonant_Medial",
                    "Consonant_Subjoined",
                    "Vowel",
                    "Vowel_Dependent",
                ]))
    }

    fn is_base_other(&self) -> bool {
        self.isc == "Consonant_Placeholder"
            || [0x2015, 0x2022, 0x25FB, 0x25FC, 0x25FD, 0x25FE].contains(&self.u)
    }

    fn is_cgj(&self) -> bool {
        self.isc == "Joiner" || (self.di && ["Mc", "Me", "Mn"].contains(&self.gc))
    }

    fn is_halant_or_vowel_modifier(&self) -> bool {
        self.u == 0x0DCA
    }

    fn is_sakot(&self) -> bool {
        self.u == 0x1A60
    }

    fn is_sym_mod(&self) -> bool {
        self.isc == "Symbol_Modifier"
    }

    fn is_word_joiner(&self) -> bool {
        const NOT: [u32; 8] = [
            0x115F, 0x1160, 0x3164, 0xFFA0, 0x1BCA0, 0x1BCA1, 0x1BCA2, 0x1BCA3,
        ];
        (self.di && !NOT.contains(&self.u) && self.isc == "Other" && !self.is_cgj())
            || self.gc == "Cn"
    }

    fn is_other(&self) -> bool {
        (self.gc == "Po" || self.isc_in(&["Consonant_Dead", "Joiner", "Modifying_Letter", "Other"]))
            && !self.is_base()
            && !self.is_base_other()
            && !self.is_cgj()
            && !self.is_sym_mod()
            && !self.is_word_joiner()
    }

    /// `use_mapping` of `gen-use-table.py`: the keys whose predicate
    /// holds.
    fn categories(&self) -> Vec<&'static str> {
        let isc = self.isc;
        let not_lo = self.gc != "Lo";
        let tests: [(&'static str, bool); 29] = [
            ("B", self.is_base()),
            ("N", isc == "Brahmi_Joining_Number"),
            ("GB", self.is_base_other()),
            ("CGJ", self.is_cgj()),
            (
                "F",
                (isc == "Consonant_Final" && not_lo) || isc == "Consonant_Succeeding_Repha",
            ),
            ("FM", isc == "Syllable_Modifier"),
            (
                "M",
                (isc == "Consonant_Medial" && not_lo) || isc == "Consonant_Initial_Postfixed",
            ),
            (
                "CM",
                self.isc_in(&["Nukta", "Gemination_Mark", "Consonant_Killer"]),
            ),
            ("SUB", isc == "Consonant_Subjoined" && not_lo),
            ("CS", isc == "Consonant_With_Stacker"),
            ("H", isc == "Virama" && !self.is_halant_or_vowel_modifier()),
            ("HVM", self.is_halant_or_vowel_modifier()),
            ("HN", isc == "Number_Joiner"),
            ("IS", isc == "Invisible_Stacker" && !self.is_sakot()),
            ("G", isc == "Hieroglyph"),
            ("HM", isc == "Hieroglyph_Modifier"),
            ("HR", isc == "Hieroglyph_Mirror"),
            ("J", isc == "Hieroglyph_Joiner"),
            (
                "SB",
                self.isc_in(&["Hieroglyph_Mark_Begin", "Hieroglyph_Segment_Begin"]),
            ),
            (
                "SE",
                self.isc_in(&["Hieroglyph_Mark_End", "Hieroglyph_Segment_End"]),
            ),
            ("ZWNJ", isc == "Non_Joiner"),
            ("O", self.is_other()),
            ("RK", isc == "Reordering_Killer"),
            (
                "R",
                self.isc_in(&["Consonant_Preceding_Repha", "Consonant_Prefixed"]),
            ),
            ("Sk", self.is_sakot()),
            ("SM", self.is_sym_mod()),
            (
                "V",
                isc == "Pure_Killer" || (not_lo && self.isc_in(&["Vowel", "Vowel_Dependent"])),
            ),
            (
                "VM",
                self.isc_in(&[
                    "Tone_Mark",
                    "Cantillation_Mark",
                    "Register_Shifter",
                    "Visarga",
                ]) || (not_lo && isc == "Bindu"),
            ),
            ("WJ", self.is_word_joiner()),
        ];
        tests
            .iter()
            .filter(|(_, holds)| *holds)
            .map(|(name, _)| *name)
            .collect()
    }
}

/// `use_positions` of `gen-use-table.py`: the positional suffixes of a
/// category and the positional categories each covers, or `None` for
/// a category without positions.
fn positions(category: &str) -> Option<&'static [(&'static str, &'static [&'static str])]> {
    let table: &'static [(&'static str, &'static [&'static str])] = match category {
        "F" => &[("Abv", &["Top"]), ("Blw", &["Bottom"]), ("Pst", &["Right"])],
        "M" => &[
            ("Abv", &["Top"]),
            ("Blw", &["Bottom", "Bottom_And_Left", "Bottom_And_Right"]),
            ("Pst", &["Right"]),
            ("Pre", &["Left", "Top_And_Bottom_And_Left"]),
        ],
        "CM" => &[("Abv", &["Top"]), ("Blw", &["Bottom", "Overstruck"])],
        "V" => &[
            (
                "Abv",
                &[
                    "Top",
                    "Top_And_Bottom",
                    "Top_And_Bottom_And_Right",
                    "Top_And_Right",
                ],
            ),
            ("Blw", &["Bottom", "Overstruck", "Bottom_And_Right"]),
            ("Pst", &["Right"]),
            (
                "Pre",
                &[
                    "Left",
                    "Top_And_Left",
                    "Top_And_Left_And_Right",
                    "Left_And_Right",
                ],
            ),
        ],
        "VM" => &[
            ("Abv", &["Top"]),
            ("Blw", &["Bottom", "Overstruck"]),
            ("Pst", &["Right"]),
            ("Pre", &["Left"]),
        ],
        "SM" => &[("Abv", &["Top"]), ("Blw", &["Bottom"])],
        "FM" => &[
            ("Abv", &["Top"]),
            ("Blw", &["Bottom"]),
            ("Pst", &["Not_Applicable"]),
        ],
        _ => return None,
    };
    Some(table)
}

/// The categories `use_positions` lists, with or without positions.
const POSITIONED: &[&str] = &[
    "F", "M", "CM", "V", "VM", "SM", "H", "HM", "HR", "HVM", "IS", "B", "FM", "R", "RK", "SUB",
];

/// `map_to_use` of `gen-use-table.py` for one code point.
fn map_to_use(mut p: Props<'_>) -> String {
    // Indic_Syllabic_Category overrides.
    if (0x1CE2..=0x1CE8).contains(&p.u) {
        p.isc = "Cantillation_Mark";
    }
    if (0x0F18..=0x0F19).contains(&p.u) || (0x0F3E..=0x0F3F).contains(&p.u) {
        p.isc = "Vowel_Dependent";
    }
    if p.u == 0x1CED {
        p.isc = "Tone_Mark";
    }
    let found = p.categories();
    assert!(
        found.len() == 1,
        "{:04X} {} {} {} {}: {found:?}",
        p.u,
        p.isc,
        p.di,
        p.gc,
        p.jt
    );
    let category = found[0];
    // Indic_Positional_Category overrides.
    if [0x11302, 0x11303, 0x114C1].contains(&p.u) {
        p.ipc = "Top";
    }
    assert!(
        ["Not_Applicable", "Visual_Order_Left"].contains(&p.ipc)
            || p.u == 0x0F7F
            || POSITIONED.contains(&category),
        "{:04X} {} {category}",
        p.u,
        p.ipc
    );
    match positions(category) {
        Some(table) => {
            let suffixes: Vec<&str> = table
                .iter()
                .filter(|(_, ipcs)| ipcs.contains(&p.ipc))
                .map(|(suffix, _)| *suffix)
                .collect();
            assert!(
                suffixes.len() == 1,
                "{:04X} {} {category}: {suffixes:?}",
                p.u,
                p.ipc
            );
            format!("{category}{}", suffixes[0])
        }
        None => category.to_owned(),
    }
}

/// The Indic_Syllabic_Category of each code point: the UCD file with
/// the Microsoft overrides on top, `Consonant_Final_Modifier` read as
/// `Syllable_Modifier`.
fn syllabic_categories() -> BTreeMap<u32, String> {
    let mut isc = per_code_point(&load(SYLLABIC));
    for (u, v) in per_code_point(&load(SYLLABIC_ADDITIONAL)) {
        let v = if v == "Consonant_Final_Modifier" {
            "Syllable_Modifier".to_owned()
        } else {
            v
        };
        isc.insert(u, v);
    }
    isc
}

/// The Indic_Positional_Category of each code point: the UCD file with
/// the Microsoft overrides on top, `NA` read as `Not_Applicable`.
fn positional_categories() -> BTreeMap<u32, String> {
    let mut ipc = per_code_point(&load(POSITIONAL));
    for (u, v) in per_code_point(&load(POSITIONAL_ADDITIONAL)) {
        let v = if v == "NA" {
            "Not_Applicable".to_owned()
        } else {
            v
        };
        ipc.insert(u, v);
    }
    ipc
}

/// The code points `gen-use-table.py` classifies: those the syllabic,
/// positional, or joining type data lists, and the default
/// ignorables, without the disabled scripts.
fn listed_code_points(
    isc: &BTreeMap<u32, String>,
    ipc: &BTreeMap<u32, String>,
    jt: &BTreeMap<u32, String>,
    di: &BTreeSet<u32>,
) -> BTreeSet<u32> {
    isc.keys()
        .chain(ipc.keys())
        .chain(jt.keys())
        .chain(di.iter())
        .copied()
        .collect()
}

/// The USE category of every code point the table lists, as
/// `gen-use-table.py` computes `use_data`.
fn use_data() -> BTreeMap<u32, String> {
    let isc = syllabic_categories();
    let ipc = positional_categories();
    let jt = per_code_point(&load(ARABIC_SHAPING));
    let di: BTreeSet<u32> = per_code_point(&load(CORE_PROPERTIES))
        .into_iter()
        .filter(|(_, v)| v == "Default_Ignorable_Code_Point")
        .map(|(u, _)| u)
        .collect();
    let gc = per_code_point(&load(UNICODE_DATA));
    let scripts = per_code_point(&load(SCRIPTS));
    let mut out = BTreeMap::new();
    for u in listed_code_points(&isc, &ipc, &jt, &di) {
        if scripts
            .get(&u)
            .is_some_and(|s| DISABLED_SCRIPTS.contains(&s.as_str()))
        {
            continue;
        }
        let props = Props {
            u,
            isc: isc.get(&u).map_or("Other", String::as_str),
            ipc: ipc.get(&u).map_or("Not_Applicable", String::as_str),
            jt: jt.get(&u).map_or("X", String::as_str),
            di: di.contains(&u),
            gc: gc.get(&u).map_or("Cn", String::as_str),
        };
        out.insert(u, map_to_use(props));
    }
    out
}

/// The generated source of `src/ot/use_shaper/table.rs`.
fn generate() -> String {
    let data = use_data();
    let mut out = String::new();
    out.push_str("// Generated by `cargo test --test use_table_gen -- --ignored`.\n");
    out.push_str("// Do not edit by hand. See `tests/use_table_gen.rs`.\n");
    for file in FILES {
        out.push_str("//\n");
        for line in &load(file).header {
            let _ = writeln!(out, "// {line}");
        }
    }
    out.push('\n');
    out.push_str("// Code point ranges read best in hex without digit separators.\n");
    out.push_str("#![allow(clippy::unreadable_literal)]\n\n");
    // Runs of consecutive code points with one category. `O` is the
    // default, so its runs are left out.
    let mut runs: Vec<(u32, u32, &str)> = Vec::new();
    for (&u, category) in &data {
        match runs.last_mut() {
            Some((_, last, c)) if *last + 1 == u && *c == category.as_str() => *last = u,
            _ => runs.push((u, u, category.as_str())),
        }
    }
    runs.retain(|&(_, _, c)| c != "O");
    let mut names: Vec<String> = runs.iter().map(|&(_, _, c)| c.to_uppercase()).collect();
    names.sort_unstable();
    names.dedup();
    out.push_str("use super::category::{\n");
    let mut line = String::from("   ");
    for name in names {
        if line.len() + 1 + name.len() + 1 > MAX_WIDTH {
            out.push_str(&line);
            out.push('\n');
            line = String::from("   ");
        }
        line.push(' ');
        line.push_str(&name);
        line.push(',');
    }
    out.push_str(&line);
    out.push_str("\n};\n\n");
    out.push_str("/// `(first, last, category)`: the USE category of each code point from\n");
    out.push_str("/// `first` to `last`. Sorted, non-overlapping, inclusive. Code points no\n");
    out.push_str("/// range covers are `O`.\n");
    out.push_str("pub(super) static RANGES: &[(u32, u32, u8)] = &[\n");
    let items: Vec<String> = runs
        .iter()
        .map(|&(a, b, c)| format!("(0x{a:04X}, 0x{b:04X}, {}),", c.to_uppercase()))
        .collect();
    let mut line = String::from("   ");
    for item in items {
        if line.len() + 1 + item.len() > MAX_WIDTH {
            out.push_str(&line);
            out.push('\n');
            line = String::from("   ");
        }
        line.push(' ');
        line.push_str(&item);
    }
    out.push_str(&line);
    out.push_str("\n];\n");
    out
}

// --- Snapshot refresh --------------------------------------------------------

/// `text` without its comment lines, as `(range, fields)` rows.
fn data_rows(text: &str) -> Vec<(String, Vec<String>)> {
    text.lines()
        .filter_map(|line| {
            let data = line.split('#').next().unwrap_or("").trim();
            let fields: Vec<String> = data.split(';').map(|f| f.trim().to_owned()).collect();
            (fields.len() > 1).then(|| (fields[0].clone(), fields[1..].to_vec()))
        })
        .collect()
}

/// The `# Name-X.Y.Z.txt` and date lines a UCD file starts with.
fn version_lines(raw: &str) -> Vec<String> {
    raw.lines()
        .take(2)
        .map(|l| l.trim_end().to_owned())
        .collect()
}

/// The version a UCD file names in its first line.
fn version_of(raw: &str, file: &str) -> String {
    let first = raw.lines().next().unwrap_or("");
    let stem = file.trim_end_matches(".txt");
    first
        .trim_start_matches('#')
        .trim()
        .trim_start_matches(stem)
        .trim_start_matches('-')
        .trim_end_matches(".txt")
        .to_owned()
}

fn write_snapshot(file: &str, header: &[String], rows: &[String]) {
    let mut out = String::new();
    for line in header {
        out.push_str(line);
        out.push('\n');
    }
    for row in rows {
        out.push_str(row);
        out.push('\n');
    }
    std::fs::write(snapshot_dir().join(file), out).expect("write snapshot");
}

/// Joins `(code point, value)` pairs into rows of consecutive code
/// points with one value.
fn run_rows(points: &BTreeMap<u32, String>) -> Vec<String> {
    let mut runs: Vec<(u32, u32, &str)> = Vec::new();
    for (&u, v) in points {
        match runs.last_mut() {
            Some((_, last, value)) if *last + 1 == u && *value == v.as_str() => *last = u,
            _ => runs.push((u, u, v.as_str())),
        }
    }
    runs.iter()
        .map(|&(a, b, v)| {
            if a == b {
                format!("{a:04X}; {v}")
            } else {
                format!("{a:04X}..{b:04X}; {v}")
            }
        })
        .collect()
}

fn refresh_snapshots() {
    let Ok(dir) = std::env::var("SIGILBUZZ_UCD_DIR") else {
        return;
    };
    let ms_use = std::env::var("SIGILBUZZ_MS_USE_DIR").expect("set SIGILBUZZ_MS_USE_DIR");
    let version = std::env::var("SIGILBUZZ_UCD_VERSION").expect("set SIGILBUZZ_UCD_VERSION");
    let harfbuzz =
        std::env::var("SIGILBUZZ_HARFBUZZ_VERSION").expect("set SIGILBUZZ_HARFBUZZ_VERSION");
    let retrieved =
        std::env::var("SIGILBUZZ_UCD_RETRIEVED").expect("set SIGILBUZZ_UCD_RETRIEVED=YYYY-MM-DD");
    std::fs::create_dir_all(snapshot_dir()).expect("create snapshot dir");
    let ucd = |file: &str| read(&Path::new(&dir).join(file));
    let provenance = |file: &str, raw: &str| {
        let url = format!(
            "https://www.unicode.org/Public/{}/ucd/{file}",
            version_of(raw, file)
        );
        let mut header = vec![
            format!("# Source: {url}"),
            format!("# Retrieved: {retrieved}"),
        ];
        header.extend(version_lines(raw));
        header
    };
    // The two category files and the overrides, whole.
    for file in [SYLLABIC, POSITIONAL] {
        let raw = ucd(file);
        let rows: Vec<String> = data_rows(&raw)
            .iter()
            .map(|(range, f)| format!("{range}; {}", f[0]))
            .collect();
        write_snapshot(file, &provenance(file, &raw), &rows);
    }
    for file in [SYLLABIC_ADDITIONAL, POSITIONAL_ADDITIONAL] {
        let raw = read(&Path::new(&ms_use).join(file));
        let mut header = vec![
            format!("# Source: HarfBuzz {harfbuzz}, src/ms-use/{file}"),
            "# MIT License, Copyright (c) Microsoft Corporation.".to_owned(),
        ];
        // The lines `gen-use-table.py` prints as the file's header.
        header.extend(
            raw.lines()
                .take_while(|l| !l.trim().is_empty())
                .map(|l| l.trim_end().to_owned()),
        );
        let rows: Vec<String> = data_rows(&raw)
            .iter()
            .map(|(range, f)| format!("{range}; {}", f[0]))
            .collect();
        write_snapshot(file, &header, &rows);
    }
    // The joining types, and the default ignorables.
    let raw = ucd(ARABIC_SHAPING);
    let rows: Vec<String> = data_rows(&raw)
        .iter()
        .map(|(range, f)| format!("{range}; {}", f[1]))
        .collect();
    write_snapshot(ARABIC_SHAPING, &provenance(ARABIC_SHAPING, &raw), &rows);
    let raw = ucd(CORE_PROPERTIES);
    let rows: Vec<String> = data_rows(&raw)
        .iter()
        .filter(|(_, f)| f[0] == "Default_Ignorable_Code_Point")
        .map(|(range, f)| format!("{range}; {}", f[0]))
        .collect();
    write_snapshot(CORE_PROPERTIES, &provenance(CORE_PROPERTIES, &raw), &rows);
    // The scripts the generator leaves out.
    let raw = ucd(SCRIPTS);
    let rows: Vec<String> = data_rows(&raw)
        .iter()
        .filter(|(_, f)| DISABLED_SCRIPTS.contains(&f[0].as_str()))
        .map(|(range, f)| format!("{range}; {}", f[0]))
        .collect();
    write_snapshot(SCRIPTS, &provenance(SCRIPTS, &raw), &rows);
    // The general categories of the code points the table lists.
    let di: BTreeSet<u32> = per_code_point(&load(CORE_PROPERTIES)).into_keys().collect();
    let listed = listed_code_points(
        &syllabic_categories(),
        &positional_categories(),
        &per_code_point(&load(ARABIC_SHAPING)),
        &di,
    );
    let raw = ucd(UNICODE_DATA);
    let mut gc = BTreeMap::new();
    for line in raw.lines() {
        let fields: Vec<&str> = line.split(';').collect();
        if fields.len() < 3 {
            continue;
        }
        let (u, _) = parse_range(fields[0]);
        if listed.contains(&u) {
            gc.insert(u, fields[2].to_owned());
        }
    }
    let header = vec![
        format!("# Source: https://www.unicode.org/Public/{version}/ucd/{UNICODE_DATA}"),
        format!("# Retrieved: {retrieved}"),
        format!("# {UNICODE_DATA}, Unicode {version}"),
    ];
    write_snapshot(UNICODE_DATA, &header, &run_rows(&gc));
}

#[test]
#[ignore = "writes the generated USE table; run explicitly to regenerate"]
fn regenerate_use_table() {
    refresh_snapshots();
    std::fs::write(root().join(TABLE_RS), generate()).expect("write table");
}

#[test]
fn committed_use_table_matches_snapshots() {
    let committed = read(&root().join(TABLE_RS));
    assert!(
        committed == generate(),
        "{TABLE_RS} is stale; run `cargo test --test use_table_gen -- --ignored`"
    );
}

#[test]
fn snapshots_derive_known_categories() {
    let data = use_data();
    let cat = |u: u32| data.get(&u).map_or("O", String::as_str);
    // TIRHUTA LETTER KA, SIGN VIRAMA, VOWEL SIGN I, SIGN CANDRABINDU.
    assert_eq!(cat(0x1148F), "B");
    assert_eq!(cat(0x114C2), "H");
    assert_eq!(cat(0x114B1), "VPre");
    assert_eq!(cat(0x114BF), "VMAbv");
    // SINHALA SIGN AL-LAKUNA, VOWEL SIGN KOMBUVA.
    assert_eq!(cat(0x0DCA), "HVM");
    assert_eq!(cat(0x0DD9), "VPre");
    // TAI THAM SIGN SAKOT, DOTTED CIRCLE, ZWNJ, ZWJ, CGJ, WORD JOINER.
    assert_eq!(cat(0x1A60), "Sk");
    assert_eq!(cat(0x25CC), "B");
    assert_eq!(cat(0x200C), "ZWNJ");
    assert_eq!(cat(0x200D), "CGJ");
    assert_eq!(cat(0x034F), "CGJ");
    assert_eq!(cat(0x2060), "WJ");
    // The scripts the generator leaves out, and a Latin letter.
    assert_eq!(cat(0x0E01), "O");
    assert_eq!(cat(0x0628), "O");
    assert_eq!(cat(u32::from('A')), "O");
}
