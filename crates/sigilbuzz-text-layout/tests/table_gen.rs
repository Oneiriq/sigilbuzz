//! Generator for the tables sigilbuzz-text-layout derives from the
//! Unicode Character Database:
//!
//! - `src/line_break_table.rs`: the `Line_Break` property (UAX #14)
//!   of every code point, with a few flag bits the line breaking rules
//!   read alongside it: East_Asian_Width F, W, or H (LB19a, LB30), the
//!   initial and final quotation marks (LB15a, LB15b, LB19), the SA
//!   characters that are marks (LB1), the letter units of CSS
//!   `word-break: keep-all` as Blink finds them, and the unassigned
//!   Extended_Pictographic code points (LB30b).
//! - `src/word_break_table.rs`: the `Word_Break` property (UAX #29)
//!   and `Extended_Pictographic` (WB3c).
//!
//! # Sources
//!
//! The committed snapshots under `tests/tools/ucd/` are the only
//! inputs. Each starts with `#` lines naming its source URL, the
//! retrieval date, and the version lines of the original file, and
//! keeps only the data the generator reads, with trailing comments
//! removed. The `@missing` lines that give the default value of the
//! code points a file does not list are kept as data.
//!
//! - `LineBreak.txt`: every data line, as `range;class;category`. The
//!   General_Category comes from the comment of the original line,
//!   where `L&` stands for a range of cased letters. The refresh checks
//!   that each comment starts with one category and counts the code
//!   points of its own line, and fails if the layout ever changes.
//! - `EastAsianWidth.txt`: the F, W, and H lines.
//! - `WordBreakProperty.txt`: every data line.
//! - `emoji-data.txt`: the `Extended_Pictographic` lines.
//!
//! # Commands
//!
//! Regenerate the tables from the snapshots:
//!
//! ```text
//! cargo test -p sigilbuzz-text-layout --test table_gen -- --ignored
//! ```
//!
//! Refresh the snapshots first by pointing `SIGILBUZZ_UCD_DIR` at a
//! directory holding the four files as downloaded from
//! `https://www.unicode.org/Public/<version>/ucd/`
//! (`WordBreakProperty.txt` is under `auxiliary/` there and
//! `emoji-data.txt` under `emoji/`), with `SIGILBUZZ_UCD_VERSION`
//! (for example `17.0.0`) and `SIGILBUZZ_UCD_RETRIEVED=YYYY-MM-DD`
//! set:
//!
//! ```text
//! SIGILBUZZ_UCD_DIR=<dir> SIGILBUZZ_UCD_VERSION=17.0.0 \
//!     SIGILBUZZ_UCD_RETRIEVED=2026-10-02 \
//!     cargo test -p sigilbuzz-text-layout --test table_gen -- --ignored
//! ```
//!
//! The non-ignored test in this file regenerates every table in memory
//! and fails when a committed file has drifted from the snapshots.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

const LINE_BREAK: &str = "LineBreak.txt";
const EAST_ASIAN_WIDTH: &str = "EastAsianWidth.txt";
const WORD_BREAK: &str = "WordBreakProperty.txt";
const EMOJI_DATA: &str = "emoji-data.txt";

const LINE_BREAK_RS: &str = "src/line_break_table.rs";
const WORD_BREAK_RS: &str = "src/word_break_table.rs";

/// Every `Line_Break` value, in the order of `LineBreakClass`.
const LINE_BREAK_CLASSES: [&str; 49] = [
    "BK", "CR", "LF", "NL", "SP", "ZW", "WJ", "GL", "CM", "ZWJ", "CB", "CL", "CP", "OP", "QU",
    "EX", "IS", "SY", "NS", "BA", "BB", "B2", "HY", "HH", "IN", "NU", "PR", "PO", "AL", "HL", "ID",
    "EB", "EM", "RI", "H2", "H3", "JL", "JV", "JT", "AK", "AP", "AS", "VF", "VI", "AI", "SA", "SG",
    "CJ", "XX",
];

/// Every `Word_Break` value except Other, as the file names it and as
/// `WordClass` names it.
const WORD_BREAK_VALUES: [(&str, &str); 18] = [
    ("CR", "Cr"),
    ("LF", "Lf"),
    ("Newline", "Newline"),
    ("Extend", "Extend"),
    ("ZWJ", "Zwj"),
    ("Regional_Indicator", "RegionalIndicator"),
    ("Format", "Format"),
    ("Katakana", "Katakana"),
    ("Hebrew_Letter", "HebrewLetter"),
    ("ALetter", "ALetter"),
    ("Single_Quote", "SingleQuote"),
    ("Double_Quote", "DoubleQuote"),
    ("MidNumLet", "MidNumLet"),
    ("MidLetter", "MidLetter"),
    ("MidNum", "MidNum"),
    ("Numeric", "Numeric"),
    ("ExtendNumLet", "ExtendNumLet"),
    ("WSegSpace", "WSegSpace"),
];

// The flag bits of the line break table. `src/class.rs` defines the
// same values.

/// East_Asian_Width is F, W, or H (`$EastAsian` in UAX #14).
const EAST_ASIAN: u8 = 1;
/// A QU character with General_Category Pi.
const INITIAL_QUOTE: u8 = 2;
/// A QU character with General_Category Pf.
const FINAL_QUOTE: u8 = 4;
/// An SA character with General_Category Mn or Mc, which LB1 resolves
/// to CM.
const SA_MARK: u8 = 8;
/// A letter unit for CSS `word-break: keep-all`, as Blink's
/// `ShouldKeepAfterKeepAll` finds them: a letter or number
/// (General_Category L* or N*) that is not of class SA.
const LETTER_UNIT: u8 = 16;
/// An unassigned (General_Category Cn) Extended_Pictographic code
/// point (LB30b).
const UNASSIGNED_PICTOGRAPHIC: u8 = 32;

/// Maximum emitted line width, matching the workspace rustfmt setting.
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

/// A snapshot: its provenance header, its `@missing` default rows, and
/// its data rows.
struct Snapshot {
    header: Vec<String>,
    missing: Vec<Vec<String>>,
    rows: Vec<Vec<String>>,
}

fn load(file: &str) -> Snapshot {
    let text = read(&snapshot_dir().join(file));
    let mut header = Vec::new();
    let mut missing = Vec::new();
    let mut rows = Vec::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix('#') {
            header.push(rest.trim().to_owned());
        } else if let Some(rest) = line.strip_prefix("@missing:") {
            missing.push(rest.split(';').map(|f| f.trim().to_owned()).collect());
        } else if !line.trim().is_empty() {
            rows.push(line.split(';').map(|f| f.trim().to_owned()).collect());
        }
    }
    Snapshot {
        header,
        missing,
        rows,
    }
}

/// Parses `XXXX` or `XXXX..YYYY`.
fn parse_range(field: &str) -> (u32, u32) {
    let hex = |s: &str| u32::from_str_radix(s, 16).unwrap_or_else(|e| panic!("{s:?}: {e}"));
    match field.split_once("..") {
        Some((a, b)) => (hex(a), hex(b)),
        None => (hex(field), hex(field)),
    }
}

/// Sets `values[cp]` for every code point of every row whose value
/// field passes `pick`, to what `pick` returns. `@missing` rows apply
/// first, so the listed rows override them.
fn fill<T: Copy>(values: &mut [T], snapshot: &Snapshot, pick: impl Fn(&[String]) -> Option<T>) {
    for row in snapshot.missing.iter().chain(&snapshot.rows) {
        let Some(value) = pick(row) else {
            continue;
        };
        let (start, end) = parse_range(&row[0]);
        for cp in start..=end {
            values[cp as usize] = value;
        }
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
    out.push_str(
        "// Generated by `cargo test -p sigilbuzz-text-layout --test table_gen -- --ignored`.\n",
    );
    out.push_str("// Do not edit by hand. See `tests/table_gen.rs`.\n");
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

/// The index of a `Line_Break` value in `LINE_BREAK_CLASSES`.
fn class_index(name: &str) -> u8 {
    let index = LINE_BREAK_CLASSES
        .iter()
        .position(|c| *c == name)
        .unwrap_or_else(|| panic!("unknown Line_Break value {name:?}"));
    u8::try_from(index).expect("fewer than 256 classes")
}

// --- Line break ----------------------------------------------------------------

/// The per-code-point `(class index, flags)` of the line break table.
fn line_break_values() -> Vec<(u8, u8)> {
    let line_break = load(LINE_BREAK);
    let widths = load(EAST_ASIAN_WIDTH);
    let emoji = load(EMOJI_DATA);

    let xx = class_index("XX");
    let mut classes = vec![xx; CODE_SPACE];
    fill(&mut classes, &line_break, |row| Some(class_index(&row[1])));
    // General_Category: the unlisted code points are unassigned. Every
    // data row carries the category of its whole range.
    for row in &line_break.rows {
        assert_eq!(
            row.len(),
            3,
            "{LINE_BREAK} snapshot row {row:?} has no category"
        );
    }
    let mut categories = vec!["Cn"; CODE_SPACE];
    fill(&mut categories, &line_break, |row| {
        row.get(2).map(|gc| match gc.as_str() {
            "L&" => "L&",
            other => LETTER_OR_OTHER
                .iter()
                .find(|c| **c == other)
                .copied()
                .unwrap_or_else(|| panic!("unknown General_Category {other:?}")),
        })
    });
    let mut east_asian = vec![false; CODE_SPACE];
    fill(&mut east_asian, &widths, |row| {
        Some(matches!(row[1].as_str(), "F" | "W" | "H"))
    });
    let mut pictographic = vec![false; CODE_SPACE];
    fill(&mut pictographic, &emoji, |row| {
        (row[1] == "Extended_Pictographic").then_some(true)
    });

    let is = |cp: usize, name: &str| LINE_BREAK_CLASSES[classes[cp] as usize] == name;
    (0..CODE_SPACE)
        .map(|cp| {
            let gc = categories[cp];
            let mut flags = 0;
            if east_asian[cp] {
                flags |= EAST_ASIAN;
            }
            if is(cp, "QU") && gc == "Pi" {
                flags |= INITIAL_QUOTE;
            }
            if is(cp, "QU") && gc == "Pf" {
                flags |= FINAL_QUOTE;
            }
            if is(cp, "SA") && matches!(gc, "Mn" | "Mc") {
                flags |= SA_MARK;
            }
            let letter = gc.starts_with('L') || gc.starts_with('N');
            if letter && !is(cp, "SA") {
                flags |= LETTER_UNIT;
            }
            if pictographic[cp] && gc == "Cn" {
                flags |= UNASSIGNED_PICTOGRAPHIC;
            }
            (classes[cp], flags)
        })
        .collect()
}

/// Every General_Category value but `L&`.
const LETTER_OR_OTHER: [&str; 30] = [
    "Lu", "Ll", "Lt", "Lm", "Lo", "Mn", "Mc", "Me", "Nd", "Nl", "No", "Pc", "Pd", "Ps", "Pe", "Pi",
    "Pf", "Po", "Sm", "Sc", "Sk", "So", "Zs", "Zl", "Zp", "Cc", "Cf", "Cs", "Co", "Cn",
];

fn generate_line_break() -> String {
    let line_break = load(LINE_BREAK);
    let widths = load(EAST_ASIAN_WIDTH);
    let emoji = load(EMOJI_DATA);
    let values = line_break_values();
    let table = runs(&values, (class_index("XX"), 0));

    let mut used: Vec<u8> = table.iter().map(|(_, _, (class, _))| *class).collect();
    used.sort_unstable();
    used.dedup();
    let names: Vec<&str> = used
        .iter()
        .map(|&i| LINE_BREAK_CLASSES[i as usize])
        .collect();

    let mut out = String::new();
    file_header(&mut out, &[&line_break, &widths, &emoji]);
    out.push_str("// Code point ranges read best in hex without digit separators.\n");
    out.push_str("#![allow(clippy::unreadable_literal)]\n\n");
    out.push_str("use crate::class::LineBreakClass::{\n");
    let mut imports = vec![String::from("self")];
    imports.extend(names.iter().map(|n| (*n).to_owned()));
    emit_wrapped(&mut out, &imports);
    out.push_str("};\n\n");
    out.push_str("/// `(first, last, Line_Break, flags)` for every code point that is not\n");
    out.push_str("/// XX without flags. Sorted, non-overlapping, inclusive. The flag bits\n");
    out.push_str("/// are the constants of `crate::class`: 1 East Asian (F, W, H), 2\n");
    out.push_str("/// initial quotation mark (QU and Pi), 4 final quotation mark (QU and\n");
    out.push_str("/// Pf), 8 SA mark (Mn or Mc), 16 keep-all letter unit, and 32\n");
    out.push_str("/// unassigned Extended_Pictographic.\n");
    out.push_str("pub(crate) static LINE_BREAK: &[(u32, u32, LineBreakClass, u8)] = &[\n");
    let items: Vec<String> = table
        .iter()
        .map(|(s, e, (class, flags))| {
            format!(
                "(0x{s:04X}, 0x{e:04X}, {}, {flags})",
                LINE_BREAK_CLASSES[*class as usize]
            )
        })
        .collect();
    emit_wrapped(&mut out, &items);
    out.push_str("];\n");
    out
}

// --- Word break ----------------------------------------------------------------

fn generate_word_break() -> String {
    let word_break = load(WORD_BREAK);
    let emoji = load(EMOJI_DATA);

    // 0 is Other, the default; the others are 1 + their index.
    let mut values = vec![0u8; CODE_SPACE];
    fill(&mut values, &word_break, |row| {
        let index = WORD_BREAK_VALUES
            .iter()
            .position(|(name, _)| *name == row[1]);
        match (row[1].as_str(), index) {
            ("Other", _) => Some(0),
            (_, Some(i)) => Some(u8::try_from(i + 1).expect("few values")),
            (other, None) => panic!("unknown Word_Break value {other:?}"),
        }
    });
    let mut pictographic = vec![false; CODE_SPACE];
    fill(&mut pictographic, &emoji, |row| {
        (row[1] == "Extended_Pictographic").then_some(true)
    });

    let table = runs(&values, 0);
    let mut used: Vec<u8> = table.iter().map(|(_, _, v)| *v).collect();
    used.sort_unstable();
    used.dedup();
    let name = |v: u8| WORD_BREAK_VALUES[v as usize - 1].1;

    let mut out = String::new();
    file_header(&mut out, &[&word_break, &emoji]);
    out.push_str("// Code point ranges read best in hex without digit separators.\n");
    out.push_str("#![allow(clippy::unreadable_literal)]\n\n");
    out.push_str("use crate::word::WordClass::{\n");
    let mut imports = vec![String::from("self")];
    imports.extend(used.iter().map(|&v| name(v).to_owned()));
    emit_wrapped(&mut out, &imports);
    out.push_str("};\n\n");
    out.push_str("/// `(first, last, Word_Break)` for every code point whose Word_Break is\n");
    out.push_str("/// not Other. Sorted, non-overlapping, inclusive.\n");
    out.push_str("pub(crate) static WORD_BREAK: &[(u32, u32, WordClass)] = &[\n");
    let items: Vec<String> = table
        .iter()
        .map(|(s, e, v)| format!("(0x{s:04X}, 0x{e:04X}, {})", name(*v)))
        .collect();
    emit_wrapped(&mut out, &items);
    out.push_str("];\n\n");
    out.push_str("/// `(first, last)` for every Extended_Pictographic code point. Sorted,\n");
    out.push_str("/// non-overlapping, inclusive.\n");
    out.push_str("pub(crate) static EXTENDED_PICTOGRAPHIC: &[(u32, u32)] = &[\n");
    let items: Vec<String> = runs(&pictographic, false)
        .iter()
        .map(|(s, e, _)| format!("(0x{s:04X}, 0x{e:04X})"))
        .collect();
    emit_wrapped(&mut out, &items);
    out.push_str("];\n");
    out
}

// --- Snapshots -----------------------------------------------------------------

/// What a snapshot keeps of one data line of a downloaded file, given
/// the fields of the line and its trailing comment.
type Keep = fn(&[&str], &str) -> Option<String>;

/// Reduces a downloaded UCD file to a snapshot: the provenance lines,
/// the `@missing` lines as data, then what `keep` keeps of each data
/// line, fields joined by `;` and trailing comments removed.
fn reduce(raw: &str, url: &str, retrieved: &str, keep: Keep) -> String {
    let mut out = format!("# Source: {url}\n# Retrieved: {retrieved}\n");
    for line in raw.lines().take(2) {
        out.push_str(line.trim_end());
        out.push('\n');
    }
    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("# @missing:") {
            let fields: Vec<&str> = rest.split(';').map(str::trim).collect();
            let _ = writeln!(out, "@missing: {}", fields.join(";"));
            continue;
        }
        let (data, comment) = line.split_once('#').unwrap_or((line, ""));
        let data = data.trim();
        if data.is_empty() {
            continue;
        }
        let fields: Vec<&str> = data.split(';').map(str::trim).collect();
        if let Some(kept) = keep(&fields, comment.trim()) {
            out.push_str(&kept);
            out.push('\n');
        }
    }
    out
}

/// What the `LineBreak.txt` snapshot keeps of a data line: the range,
/// the class, and the General_Category, which the comment gives first.
///
/// The generator reads one category per line. The file guarantees
/// that today: its header says the comment lists the General_Category
/// value or `L&`, followed by the code point count of the line's range.
/// Panics when a comment does not start with one known category, or
/// when its count does not match the range, so a change to that layout
/// fails the refresh instead of mislabeling code points.
fn keep_line_break(fields: &[&str], comment: &str) -> Option<String> {
    let mut words = comment.split_whitespace();
    let gc = words.next().unwrap_or_default();
    assert!(
        gc == "L&" || LETTER_OR_OTHER.contains(&gc),
        "LineBreak.txt {}: the comment {comment:?} does not start with one General_Category",
        fields[0]
    );
    let (start, end) = parse_range(fields[0]);
    let count = words
        .next()
        .and_then(|word| word.strip_prefix('['))
        .and_then(|word| word.strip_suffix(']'));
    let expected = (end > start).then(|| (end - start + 1).to_string());
    assert!(
        count == expected.as_deref(),
        "LineBreak.txt {}: the comment {comment:?} does not count the code points of the line",
        fields[0]
    );
    Some(format!("{};{};{gc}", fields[0], fields[1]))
}

fn refresh_snapshots() {
    let Ok(dir) = std::env::var("SIGILBUZZ_UCD_DIR") else {
        return;
    };
    let version = std::env::var("SIGILBUZZ_UCD_VERSION").expect("set SIGILBUZZ_UCD_VERSION");
    let retrieved =
        std::env::var("SIGILBUZZ_UCD_RETRIEVED").expect("set SIGILBUZZ_UCD_RETRIEVED=YYYY-MM-DD");
    let base = format!("https://www.unicode.org/Public/{version}/ucd");
    let jobs: [(&str, String, Keep); 4] = [
        (LINE_BREAK, format!("{base}/{LINE_BREAK}"), keep_line_break),
        (
            EAST_ASIAN_WIDTH,
            format!("{base}/{EAST_ASIAN_WIDTH}"),
            |f, _| matches!(f[1], "F" | "W" | "H").then(|| f.join(";")),
        ),
        (
            WORD_BREAK,
            format!("{base}/auxiliary/{WORD_BREAK}"),
            |f, _| Some(f.join(";")),
        ),
        (EMOJI_DATA, format!("{base}/emoji/{EMOJI_DATA}"), |f, _| {
            (f[1] == "Extended_Pictographic").then(|| f.join(";"))
        }),
    ];
    std::fs::create_dir_all(snapshot_dir()).expect("create snapshot dir");
    for (file, url, keep) in jobs {
        let raw = read(&Path::new(&dir).join(file));
        let snapshot = reduce(&raw, &url, &retrieved, keep);
        std::fs::write(snapshot_dir().join(file), snapshot).expect("write snapshot");
    }
}

fn outputs() -> [(&'static str, String); 2] {
    [
        (LINE_BREAK_RS, generate_line_break()),
        (WORD_BREAK_RS, generate_word_break()),
    ]
}

#[test]
#[ignore = "writes the generated UCD tables; run explicitly to regenerate"]
fn regenerate_tables() {
    refresh_snapshots();
    for (path, table) in outputs() {
        std::fs::write(root().join(path), table).expect("write table");
    }
}

#[test]
fn committed_tables_match_snapshots() {
    for (path, expected) in outputs() {
        let committed = read(&root().join(path));
        assert!(
            committed == expected,
            "{path} is stale; run \
             `cargo test -p sigilbuzz-text-layout --test table_gen -- --ignored`"
        );
    }
}

#[test]
fn line_break_comments_give_one_category() {
    let keep = |range, class, comment| keep_line_break(&[range, class], comment);
    assert_eq!(
        keep(
            "0000..0008",
            "CM",
            "Cc     [9] <control-0000>..<control-0008>"
        )
        .as_deref(),
        Some("0000..0008;CM;Cc")
    );
    assert_eq!(
        keep("0020", "SP", "Zs         SPACE").as_deref(),
        Some("0020;SP;Zs")
    );
    assert_eq!(
        keep(
            "01C4..01CC",
            "AL",
            "L&     [9] LATIN CAPITAL LETTER DZ WITH CARON.."
        )
        .as_deref(),
        Some("01C4..01CC;AL;L&")
    );
}

#[test]
#[should_panic(expected = "does not start with one General_Category")]
fn line_break_comment_without_a_category_fails() {
    let _ = keep_line_break(&["0000..0008", "CM"], "[9] <control-0000>..<control-0008>");
}

#[test]
#[should_panic(expected = "does not count the code points of the line")]
fn line_break_comment_with_the_wrong_count_fails() {
    let _ = keep_line_break(
        &["0000..0008", "CM"],
        "Cc     [8] <control-0000>..<control-0007>",
    );
}

#[test]
fn snapshots_derive_known_values() {
    let values = line_break_values();
    let class = |cp: u32| LINE_BREAK_CLASSES[values[cp as usize].0 as usize];
    let flags = |cp: u32| values[cp as usize].1;
    assert_eq!(class(0xAC00), "H2");
    assert_eq!(class(0xAC01), "H3");
    assert_eq!(class(0x1100), "JL");
    assert_eq!(class(0x1161), "JV");
    assert_eq!(class(0x11AB), "JT");
    assert_eq!(class(0x002C), "IS");
    assert_eq!(class(0x3041), "CJ");
    assert_eq!(class(0x0E01), "SA");
    // Unlisted code points default to XX.
    assert_eq!(class(0xE0080), "XX");
    // Hangul syllables are wide letters.
    assert_eq!(flags(0xAC00), EAST_ASIAN | LETTER_UNIT);
    assert_eq!(flags(0x201C), INITIAL_QUOTE);
    assert_eq!(flags(0x201D), FINAL_QUOTE);
    assert_eq!(flags(0x0E31) & SA_MARK, SA_MARK);
    assert_eq!(flags(0x0E01) & SA_MARK, 0);
    assert_eq!(flags(0x300C), EAST_ASIAN);
    assert_eq!(flags(0x0028), 0);
    // An unassigned code point in an emoji block.
    assert_eq!(class(0x1F02C), "ID");
    assert_eq!(flags(0x1F02C), UNASSIGNED_PICTOGRAPHIC);
    // Letter units are the letters and numbers outside class SA. Emoji
    // and symbols are not, whatever their class.
    assert_eq!(flags(0x0041) & LETTER_UNIT, LETTER_UNIT);
    assert_eq!(flags(0x0E01) & LETTER_UNIT, 0);
    assert_eq!(flags(0x1F600) & LETTER_UNIT, 0);
    assert_eq!(flags(0x0040) & LETTER_UNIT, 0);
}
