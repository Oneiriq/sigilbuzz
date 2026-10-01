//! Generator for `src/shape/vowel_constraints/table.rs`, the character
//! sequences HarfBuzz's vowel constraints put a dotted circle into.
//!
//! HarfBuzz generates `hb-ot-shaper-vowel-constraints.cc` with
//! `gen-vowel-constraints.py` from `ms-use/IndicShapingInvalidCluster.txt`
//! and `Scripts.txt`. This file ports that script as of HarfBuzz 14.5.0:
//! each line of the constraint file is a prohibited sequence, filed
//! under the script of its first code point, and `ConstraintSet.add`
//! merges each script's sequences into a tree in file order. A
//! sequence that extends a prohibited one adds nothing. The table lists
//! the paths of that tree.
//!
//! The generated C++ inserts the dotted circle at a place that depends
//! on the shape of the tree. [`circle_position`] computes it the way
//! `ConstraintSet.__str__` emits the code, and the generator fails when
//! a sequence would not get its circle before its last code point,
//! which is what the runtime in `src/shape/vowel_constraints.rs` does.
//! It also fails where `gen-vowel-constraints.py` asserts.
//!
//! # Sources
//!
//! The committed snapshots are the only inputs:
//! `tests/tools/ms-use/IndicShapingInvalidCluster.txt` (HarfBuzz's file
//! with its provenance prepended), and `Scripts.txt` and
//! `PropertyValueAliases.txt` under `tests/tools/ucd/` (the script
//! codes).
//!
//! # Commands
//!
//! Regenerate the table from the snapshots:
//!
//! ```text
//! cargo test --test vowel_constraints_gen -- --ignored
//! ```
//!
//! The non-ignored tests regenerate the table in memory and fail when
//! the committed file has drifted from the snapshots, and check that
//! the table prohibits every line of the constraint file.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

const TABLE_RS: &str = "src/shape/vowel_constraints/table.rs";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
        .replace("\r\n", "\n")
}

fn constraints_file() -> String {
    read(
        &root()
            .join("tests")
            .join("tools")
            .join("ms-use")
            .join("IndicShapingInvalidCluster.txt"),
    )
}

fn ucd_file(name: &str) -> String {
    read(&root().join("tests").join("tools").join("ucd").join(name))
}

fn hex(s: &str) -> u32 {
    u32::from_str_radix(s, 16).unwrap_or_else(|e| panic!("{s:?}: {e}"))
}

/// The script of each code point `Scripts.txt` lists, and the order
/// the scripts first appear in (`script_order`).
fn scripts() -> (BTreeMap<u32, String>, BTreeMap<String, u32>) {
    let mut of = BTreeMap::new();
    let mut order = BTreeMap::new();
    for line in ucd_file("Scripts.txt").lines() {
        let data = line.split('#').next().unwrap_or("");
        let Some((range, script)) = data.split_once(';') else {
            continue;
        };
        let range = range.trim();
        let (start, end) = match range.split_once("..") {
            Some((a, b)) => (hex(a), hex(b)),
            None => (hex(range), hex(range)),
        };
        let script = script.trim().to_owned();
        for u in start..=end {
            of.insert(u, script.clone());
        }
        order.entry(script).or_insert(start);
    }
    (of, order)
}

/// The ISO 15924 code of each script's long name in
/// `PropertyValueAliases.txt`.
fn script_codes() -> BTreeMap<String, String> {
    let mut codes = BTreeMap::new();
    for line in ucd_file("PropertyValueAliases.txt").lines() {
        let fields: Vec<&str> = line.split(';').map(str::trim).collect();
        if let ["sc", code, name, ..] = fields[..] {
            codes.insert(name.to_owned(), code.to_owned());
        }
    }
    codes
}

/// The header lines of the constraint file: those before the first
/// line that is a lone `#`.
fn constraints_header() -> Vec<String> {
    constraints_file()
        .lines()
        .map(str::trim)
        .take_while(|l| *l != "#")
        .map(str::to_owned)
        .collect()
}

/// The prohibited sequences, in file order.
fn constraint_lines() -> Vec<Vec<u32>> {
    let text = constraints_file();
    let mut lines = text.lines();
    for line in lines.by_ref() {
        if line.trim() == "#" {
            break;
        }
    }
    let mut out = Vec::new();
    for line in lines {
        let data = line.split('#').next().unwrap_or("");
        let data = data.split(';').next().unwrap_or("");
        let seq: Vec<u32> = data.split_whitespace().map(hex).collect();
        if seq.is_empty() {
            continue;
        }
        assert!(seq.len() >= 2, "Prohibited sequence is too short: {seq:X?}");
        out.push(seq);
    }
    assert!(!out.is_empty(), "No constraints found");
    out
}

/// `ConstraintSet` of `gen-vowel-constraints.py`: either one prohibited
/// sequence, or the sequences that start with each key.
enum ConstraintSet {
    List(Vec<u32>),
    Dict(BTreeMap<u32, ConstraintSet>),
}

impl ConstraintSet {
    /// `ConstraintSet.add`.
    fn add(&mut self, constraint: &[u32]) {
        let Some((&first, rest)) = constraint.split_first() else {
            return;
        };
        if let Self::List(c) = self {
            if constraint.len() <= c.len() && constraint == &c[..constraint.len()] {
                *c = constraint.to_vec();
            } else if c.len() > constraint.len() || c[..] != constraint[..c.len()] {
                let (head, tail) = c.split_first().expect("a list that is not a prefix");
                let mut dict = BTreeMap::new();
                dict.insert(*head, Self::List(tail.to_vec()));
                *self = Self::Dict(dict);
            }
        }
        if let Self::Dict(dict) = self {
            match dict.get_mut(&first) {
                Some(set) => set.add(rest),
                None => {
                    dict.insert(first, Self::List(rest.to_vec()));
                }
            }
        }
    }

    /// Every prohibited sequence of the set, with the position of the
    /// code point the generated code puts the dotted circle before.
    /// `index` is the depth of the set in the tree and `prefix` the
    /// code points that lead to it.
    fn paths(&self, index: usize, prefix: &[u32], out: &mut Vec<(Vec<u32>, usize)>) {
        match self {
            Self::List(c) => {
                let mut seq = prefix.to_vec();
                seq.extend_from_slice(c);
                out.push((seq, circle_position(index, c.len())));
            }
            Self::Dict(dict) => {
                for (&first, rest) in dict {
                    let mut seq = prefix.to_vec();
                    seq.push(first);
                    rest.paths(index + 1, &seq, out);
                }
            }
        }
    }
}

/// Where the code `ConstraintSet.__str__` emits for a list of `len`
/// code points at depth `index` inserts the dotted circle: before the
/// input position the buffer reaches after `index` `next_glyph` calls
/// in the match and one after it. An empty list and a list of one only
/// emit `matched`, which the generator asserts can only happen at
/// depths 2 and 1.
fn circle_position(index: usize, len: usize) -> usize {
    match len {
        0 => {
            assert_eq!(index, 2, "Cannot use `matched` for this constraint");
            1
        }
        1 => {
            assert_eq!(index, 1, "Cannot use `matched` for this constraint");
            1
        }
        _ => index + 1,
    }
}

/// `(ISO 15924 code, sequences)` for each script, in the order of
/// `Scripts.txt`. Each script's sequences are sorted.
fn table() -> Vec<(String, Vec<Vec<u32>>)> {
    let (script_of, order) = scripts();
    let codes = script_codes();
    let mut sets: BTreeMap<String, ConstraintSet> = BTreeMap::new();
    for seq in constraint_lines() {
        let script = script_of
            .get(&seq[0])
            .unwrap_or_else(|| panic!("no script for {:04X}", seq[0]))
            .clone();
        match sets.get_mut(&script) {
            Some(set) => set.add(&seq),
            None => {
                sets.insert(script, ConstraintSet::List(seq));
            }
        }
    }
    let mut scripts: Vec<(&String, &ConstraintSet)> = sets.iter().collect();
    scripts.sort_by_key(|(name, _)| order[*name]);
    scripts
        .into_iter()
        .map(|(name, set)| {
            let mut paths = Vec::new();
            set.paths(0, &[], &mut paths);
            for (seq, circle) in &paths {
                assert_eq!(
                    *circle,
                    seq.len() - 1,
                    "{name}: the circle in {seq:04X?} is not before the last code point"
                );
            }
            let mut seqs: Vec<Vec<u32>> = paths.into_iter().map(|(seq, _)| seq).collect();
            seqs.sort();
            let code = codes
                .get(name)
                .unwrap_or_else(|| panic!("no ISO 15924 code for {name}"));
            (code.clone(), seqs)
        })
        .collect()
}

/// The generated source of `src/shape/vowel_constraints/table.rs`.
fn generate() -> String {
    let mut out = String::new();
    out.push_str("// Generated by `cargo test --test vowel_constraints_gen -- --ignored`.\n");
    out.push_str("// Do not edit by hand. See `tests/vowel_constraints_gen.rs`.\n");
    out.push_str("//\n");
    for line in constraints_header() {
        let _ = writeln!(out, "// {}", line.trim_start_matches('#').trim());
    }
    out.push_str("//\n");
    for line in ucd_file("Scripts.txt").lines().take(4) {
        let _ = writeln!(out, "// {}", line.trim_start_matches('#').trim());
    }
    out.push('\n');
    out.push_str("/// `(script, sequences)`: the ISO 15924 code of each script with\n");
    out.push_str("/// constraints, and the sequences that take a dotted circle before their\n");
    out.push_str("/// last character. Each script's sequences are sorted, and none is a\n");
    out.push_str("/// prefix of another.\n");
    out.push_str("pub(super) static CONSTRAINTS: &[([u8; 4], &[&[char]])] = &[\n");
    for (code, seqs) in table() {
        let items: Vec<String> = seqs
            .iter()
            .map(|seq| {
                let chars: Vec<String> = seq.iter().map(|u| format!("'\\u{{{u:04X}}}'")).collect();
                format!("&[{}]", chars.join(", "))
            })
            .collect();
        // One sequence per line, as rustfmt lays them out, unless there
        // is only one.
        if let [item] = &items[..] {
            let _ = writeln!(out, "    (*b\"{code}\", &[{item}]),");
            continue;
        }
        let _ = writeln!(out, "    (\n        *b\"{code}\",\n        &[");
        for item in items {
            let _ = writeln!(out, "            {item},");
        }
        out.push_str("        ],\n    ),\n");
    }
    out.push_str("];\n");
    out
}

#[test]
#[ignore = "writes the generated vowel constraint table; run explicitly to regenerate"]
fn regenerate_vowel_constraints_table() {
    std::fs::write(root().join(TABLE_RS), generate()).expect("write table");
}

#[test]
fn committed_vowel_constraints_table_matches_snapshots() {
    let committed = read(&root().join(TABLE_RS));
    assert!(
        committed == generate(),
        "{TABLE_RS} is stale; run `cargo test --test vowel_constraints_gen -- --ignored`"
    );
}

#[test]
fn table_prohibits_every_line_of_the_constraint_file() {
    let (script_of, _) = scripts();
    let codes = script_codes();
    let table = table();
    let lines = constraint_lines();
    assert_eq!(lines.len(), 103);
    for seq in lines {
        let code = &codes[&script_of[&seq[0]]];
        let (_, seqs) = table
            .iter()
            .find(|(c, _)| c == code)
            .unwrap_or_else(|| panic!("no constraints for {code}"));
        // The line itself, or a prohibited sequence it extends, which
        // the circle then splits first.
        assert!(
            seqs.iter().any(|s| seq.starts_with(s)),
            "{seq:04X?} is not prohibited"
        );
    }
}

#[test]
fn table_covers_harfbuzz_scripts() {
    let codes: Vec<String> = table().into_iter().map(|(code, _)| code).collect();
    // The `case HB_SCRIPT_*` labels of `hb-ot-shaper-vowel-constraints.cc`,
    // in its order.
    assert_eq!(
        codes,
        [
            "Deva", "Beng", "Guru", "Gujr", "Orya", "Taml", "Telu", "Knda", "Mlym", "Sinh", "Brah",
            "Khoj", "Sind", "Tirh", "Modi", "Takr",
        ]
    );
}

#[test]
fn longer_sequences_fold_into_shorter_ones() {
    let table = table();
    let gujarati = &table.iter().find(|(c, _)| c == "Gujr").expect("Gujr").1;
    // `0A85 0ABE 0AC5` extends `0A85 0ABE`, so only the shorter one
    // stays, as in the generated `case HB_SCRIPT_GUJARATI`.
    assert!(gujarati.contains(&vec![0x0A85, 0x0ABE]));
    assert!(!gujarati.contains(&vec![0x0A85, 0x0ABE, 0x0AC5]));
    let devanagari = &table.iter().find(|(c, _)| c == "Deva").expect("Deva").1;
    assert!(devanagari.contains(&vec![0x0930, 0x094D, 0x0907]));
}
