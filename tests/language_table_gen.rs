//! Generator for `src/language/table.rs`, the table that maps BCP 47
//! language subtags to OpenType language system tags.
//!
//! # Sources
//!
//! The committed snapshots under `tests/tools/langtags/` are the only
//! inputs. Each carries its source URL and retrieval date in a `#`
//! header line:
//!
//! - `ot-language-tags.tsv`: the Microsoft OpenType Language System
//!   Tag registry (the primary source), one row per registered tag:
//!   tag, deprecation flag, language system name, and the "ISO 639 IDs
//!   or other information" cell.
//! - `iso-639-1.tsv`: the ISO 639-3 identifiers that have an ISO
//!   639-1 (two-letter) equivalent, taken from SIL's `iso-639-3.tab`.
//!   BCP 47 requires the two-letter form when one exists, while the
//!   registry lists three-letter codes, so this is the bridge.
//! - `iso-639-3-macrolanguages.tsv`: SIL's macrolanguage mappings,
//!   used to let individual languages inherit the tags of their
//!   macrolanguage (Egyptian Arabic `arz` inherits Arabic `ARA `).
//!
//! # Commands
//!
//! Regenerate the table from the snapshots:
//!
//! ```text
//! cargo test --test language_table_gen -- --ignored
//! ```
//!
//! Refresh the snapshots from fresh downloads first by pointing these
//! variables at the downloaded files (all optional, each refreshes one
//! snapshot) and setting the retrieval date:
//!
//! ```text
//! SIGILBUZZ_LANGTAGS_HTML=path/to/languagetags.html
//! SIGILBUZZ_ISO639_3_TAB=path/to/iso-639-3.tab
//! SIGILBUZZ_ISO639_3_MACRO_TAB=path/to/iso-639-3-macrolanguages.tab
//! SIGILBUZZ_LANGTAGS_RETRIEVED=YYYY-MM-DD
//! ```
//!
//! The non-ignored test in this file regenerates the table in memory
//! and fails when the committed file has drifted from the snapshots.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

const REGISTRY_URL: &str =
    "https://learn.microsoft.com/en-us/typography/opentype/spec/languagetags";
const ISO639_3_URL: &str = "https://iso639-3.sil.org/sites/iso639-3/files/downloads/iso-639-3.tab";
const ISO639_MACRO_URL: &str =
    "https://iso639-3.sil.org/sites/iso639-3/files/downloads/iso-639-3-macrolanguages.tab";

const REGISTRY_TSV: &str = "ot-language-tags.tsv";
const PART1_TSV: &str = "iso-639-1.tsv";
const MACRO_TSV: &str = "iso-639-3-macrolanguages.tsv";
const TABLE_RS: &str = "src/language/table.rs";

/// Deliberate departures from a mechanical reading of the registry,
/// applied before macrolanguage inheritance so the individual
/// languages of a macrolanguage pick them up. Each keeps sigilbuzz in
/// parity with HarfBuzz's generated table (cross-checked against
/// rustybuzz 0.20).
const OVERRIDES: &[(&str, &[&str])] = &[
    // The registry lists `zho` under every Chinese tag (ZHH, ZHP, ZHS,
    // ZHT, ZHTM). Script and region subtags choose among them at run
    // time; a bare Chinese language tag means Simplified Chinese.
    ("zho", &["ZHS "]),
    // 'PGR ' (polytonic) and 'KGE ' (Khutsuri) also list `ell` and
    // `kat`, but they name orthographies that BCP 47 spells with the
    // `-polyton` variant and the `-Geok` script subtag.
    ("ell", &["ELL "]),
    ("kat", &["KAT "]),
    // 'MOR ' lists no ISO code; it is the tag for Moroccan Arabic.
    ("ary", &["MOR ", "ARA "]),
    // The Quechua macrolanguage; 'QUZ ' lists only Cusco Quechua.
    ("que", &["QUZ "]),
];

/// Overrides applied after inheritance, so they do not spread to the
/// individual languages of a macrolanguage.
const FINAL_OVERRIDES: &[(&str, &[&str])] = &[
    // Cantonese and Literary Chinese default to traditional forms.
    ("yue", &["ZHH "]),
    ("lzh", &["ZHT "]),
    // Macrolanguages the registry only covers through their members.
    // Bokmal keeps 'NOR ', Nynorsk keeps 'NYN ', and Serbian, Croatian,
    // and Bosnian keep their own tags.
    ("nor", &["NOR "]),
    ("hbs", &["BOS ", "HRV ", "SRB "]),
    // Retired code for Moldavian; the deprecated `mo` subtag maps here.
    ("mol", &["MOL ", "ROM "]),
];

/// Deprecated two-letter BCP 47 subtags still common in the wild (the
/// Java locale APIs emit `iw`, `in`, and `ji`), mapped to the code whose
/// tags they share.
const DEPRECATED_ALIASES: &[(&str, &str)] = &[
    ("in", "ind"),
    ("iw", "heb"),
    ("ji", "yid"),
    ("jw", "jav"),
    ("mo", "mol"),
    ("sh", "hbs"),
];

/// Maximum emitted line width, matching the crate's rustfmt setting.
const MAX_WIDTH: usize = 100;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn snapshot_dir() -> PathBuf {
    root().join("tests").join("tools").join("langtags")
}

/// One row of the OpenType registry snapshot.
struct RegistryRow {
    tag: [u8; 4],
    deprecated: bool,
    /// Three-letter ISO 639 codes listed for the tag, in cell order.
    codes: Vec<String>,
    /// BCP 47 variant subtag named by a "Cf. BCP 47 variant subtag"
    /// note, if any.
    variant: Option<String>,
    /// ISO 15924 script code for script-variant tags whose name says
    /// "equivalent to ISO 15924 'Xxxx'" (the Syriac variants). Those
    /// tags are chosen by a BCP 47 script subtag, not by language.
    script_variant: Option<String>,
}

/// Extracts the script code from a registry name such as
/// "Syriac, Estrangela script-variant (equivalent to ISO 15924 'Syre')".
fn script_variant_of(name: &str) -> Option<String> {
    let rest = name.split("equivalent to ISO 15924 '").nth(1)?;
    let code = rest.split('\'').next()?;
    (code.len() == 4).then(|| code.to_ascii_lowercase())
}

/// Everything the generator needs, parsed from the snapshots.
struct Sources {
    registry_header: String,
    iso_header: String,
    registry: Vec<RegistryRow>,
    /// ISO 639-3 id to ISO 639-1 code.
    part1: Vec<(String, String)>,
    /// (macrolanguage, individual language) pairs.
    macros: Vec<(String, String)>,
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Splits a snapshot into its `#` header lines and its data rows,
/// dropping the column-name row.
fn split_snapshot(text: &str) -> (String, Vec<Vec<String>>) {
    let mut header = String::new();
    let mut rows = Vec::new();
    let mut saw_columns = false;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix('#') {
            header.push_str(rest.trim());
            header.push('\n');
            continue;
        }
        if !saw_columns {
            saw_columns = true;
            continue;
        }
        if line.is_empty() {
            continue;
        }
        rows.push(line.split('\t').map(str::to_owned).collect());
    }
    (header, rows)
}

fn parse_tag(text: &str) -> [u8; 4] {
    let bytes = text.as_bytes();
    assert_eq!(bytes.len(), 4, "tag {text:?} is not four bytes");
    [bytes[0], bytes[1], bytes[2], bytes[3]]
}

/// Parses the registry's third column: a comma-separated list of
/// three-letter ISO 639 codes, or a note naming a BCP 47 variant.
fn parse_iso_cell(cell: &str) -> (Vec<String>, Option<String>) {
    if let Some(rest) = cell.strip_prefix("Cf. BCP 47 variant subtag ") {
        let variant = rest
            .trim_start_matches('\u{201C}')
            .split('\u{201D}')
            .next()
            .unwrap_or("")
            .to_owned();
        return (Vec::new(), Some(variant));
    }
    let codes = cell
        .split(',')
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map(|c| {
            assert!(
                c.len() == 3 && c.bytes().all(|b| b.is_ascii_lowercase()),
                "unexpected ISO 639 code {c:?}"
            );
            c.to_owned()
        })
        .collect();
    (codes, None)
}

fn load_sources() -> Sources {
    let dir = snapshot_dir();
    let (registry_header, registry_rows) = split_snapshot(&read(&dir.join(REGISTRY_TSV)));
    let registry = registry_rows
        .iter()
        .map(|cols| {
            assert_eq!(cols.len(), 4, "registry row {cols:?}");
            let (codes, variant) = parse_iso_cell(&cols[3]);
            RegistryRow {
                tag: parse_tag(&cols[0]),
                deprecated: cols[1] == "1",
                codes,
                variant,
                script_variant: script_variant_of(&cols[2]),
            }
        })
        .collect();
    let (iso_header, part1_rows) = split_snapshot(&read(&dir.join(PART1_TSV)));
    let part1 = part1_rows
        .into_iter()
        .map(|cols| (cols[0].clone(), cols[1].clone()))
        .collect();
    let (_, macro_rows) = split_snapshot(&read(&dir.join(MACRO_TSV)));
    let macros = macro_rows
        .into_iter()
        .map(|cols| (cols[0].clone(), cols[1].clone()))
        .collect();
    Sources {
        registry_header,
        iso_header,
        registry,
        part1,
        macros,
    }
}

fn tag_literal(tag: &[u8; 4]) -> String {
    format!("*b\"{}\"", String::from_utf8_lossy(tag))
}

fn parse_tags(tags: &[&str]) -> Vec<[u8; 4]> {
    tags.iter().map(|t| parse_tag(t)).collect()
}

/// Builds the subtag to tags map.
///
/// Tags for a code are ordered most specific first. Like HarfBuzz, the
/// rank of a tag is the number of languages it ends up covering, so a
/// narrow tag beats a broad one: Armenian East 'HYE0' covers only
/// `hye` and comes before Armenian 'HYE ', and Malay 'MLY ' comes
/// before Creoles 'CPP ', which covers hundreds. Ties keep registry
/// order and deprecated tags come last.
fn build_map(src: &Sources) -> BTreeMap<String, Vec<[u8; 4]>> {
    let mut map: BTreeMap<String, Vec<[u8; 4]>> = BTreeMap::new();
    for row in src.registry.iter().filter(|r| r.script_variant.is_none()) {
        for code in &row.codes {
            let entry = map.entry(code.clone()).or_default();
            if !entry.contains(&row.tag) {
                entry.push(row.tag);
            }
        }
    }
    let pinned: BTreeSet<&str> = OVERRIDES
        .iter()
        .chain(FINAL_OVERRIDES)
        .map(|(code, _)| *code)
        .collect();
    for (code, tags) in OVERRIDES {
        map.insert((*code).to_owned(), parse_tags(tags));
    }
    // Individual languages inherit their macrolanguage's tags, unless
    // an override pins them.
    let snapshot = map.clone();
    for (macro_code, individual) in &src.macros {
        if pinned.contains(individual.as_str()) {
            continue;
        }
        let Some(inherited) = snapshot.get(macro_code) else {
            continue;
        };
        let entry = map.entry(individual.clone()).or_default();
        for tag in inherited {
            if !entry.contains(tag) {
                entry.push(*tag);
            }
        }
    }
    // Rank: (deprecated, languages covered, registry row).
    let mut covered: BTreeMap<[u8; 4], usize> = BTreeMap::new();
    for tags in map.values() {
        for tag in tags {
            *covered.entry(*tag).or_default() += 1;
        }
    }
    let rank: BTreeMap<[u8; 4], (bool, usize, usize)> = src
        .registry
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let count = covered.get(&row.tag).copied().unwrap_or(0);
            (row.tag, (row.deprecated, count, i))
        })
        .collect();
    for (code, tags) in &mut map {
        if !pinned.contains(code.as_str()) {
            tags.sort_by_key(|t| {
                rank.get(t)
                    .copied()
                    .unwrap_or((true, usize::MAX, usize::MAX))
            });
        }
    }
    for (code, tags) in FINAL_OVERRIDES {
        map.insert((*code).to_owned(), parse_tags(tags));
    }
    // Two-letter aliases share their three-letter code's list.
    let three_letter = map.clone();
    let aliases = src
        .part1
        .iter()
        .map(|(id, part1)| (part1.as_str(), id.as_str()))
        .chain(DEPRECATED_ALIASES.iter().copied());
    for (alias, id) in aliases {
        if let Some(tags) = three_letter.get(id) {
            map.insert(alias.to_owned(), tags.clone());
        }
    }
    map
}

/// Three-letter subtags that must not fall back to their uppercase
/// form: the uppercase string is a registered tag that the registry
/// does not associate with that subtag (`aba` is Abe, but 'ABA ' is
/// Abaza).
fn no_fallback(src: &Sources, map: &BTreeMap<String, Vec<[u8; 4]>>) -> BTreeSet<String> {
    src.registry
        .iter()
        .filter(|row| row.tag[3] == b' ' && row.tag[..3].iter().all(u8::is_ascii_uppercase))
        .map(|row| String::from_utf8_lossy(&row.tag[..3]).to_ascii_lowercase())
        .filter(|code| !map.contains_key(code))
        .collect()
}

/// Appends `items` to `out` as a comma-separated list wrapped to
/// `MAX_WIDTH` columns with a four-space indent.
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
    if line.trim().is_empty() {
        return;
    }
    out.push_str(&line);
    out.push('\n');
}

fn header_comment(out: &mut String, header: &str) {
    for line in header.lines() {
        let _ = writeln!(out, "// {line}");
    }
}

fn generate(src: &Sources) -> String {
    let map = build_map(src);
    let blocked = no_fallback(src, &map);
    let mut chinese: Vec<String> = src
        .macros
        .iter()
        .filter(|(m, _)| m == "zho")
        .map(|(_, i)| i.clone())
        .collect();
    chinese.push("zh".to_owned());
    chinese.sort();
    chinese.dedup();
    let mut variants: Vec<(String, [u8; 4])> = src
        .registry
        .iter()
        .filter(|row| !row.deprecated)
        .filter_map(|row| {
            let subtag = row.variant.clone().or_else(|| row.script_variant.clone())?;
            Some((subtag, row.tag))
        })
        .collect();
    variants.sort();

    let mut out = String::new();
    out.push_str("// Generated by `cargo test --test language_table_gen -- --ignored`.\n");
    out.push_str("// Do not edit by hand. See the module docs in `mod.rs`.\n//\n");
    header_comment(&mut out, &src.registry_header);
    header_comment(&mut out, &src.iso_header);
    out.push('\n');
    out.push_str("/// BCP 47 primary language subtags (ISO 639-1, 639-2, and 639-3 codes)\n");
    out.push_str("/// with their OpenType language system tags, most specific first.\n");
    out.push_str("/// Sorted by subtag for binary search.\n");
    out.push_str("pub(super) static LANGUAGE_TAGS: &[(&str, &[[u8; 4]])] = &[\n");
    let entries: Vec<String> = map
        .iter()
        .filter(|(_, tags)| !tags.is_empty())
        .map(|(code, tags)| {
            let list: Vec<String> = tags.iter().map(tag_literal).collect();
            format!("(\"{code}\", &[{}])", list.join(", "))
        })
        .collect();
    emit_wrapped(&mut out, &entries);
    out.push_str("];\n\n");

    out.push_str("/// Primary subtags of the Chinese macrolanguage family. Script and region\n");
    out.push_str("/// subtags on these select among the Chinese language system tags.\n");
    out.push_str("pub(super) static CHINESE_FAMILY: &[&str] = &[\n");
    let quoted: Vec<String> = chinese.iter().map(|c| format!("\"{c}\"")).collect();
    emit_wrapped(&mut out, &quoted);
    out.push_str("];\n\n");

    out.push_str("/// BCP 47 variant and script subtags the registry ties to a language\n");
    out.push_str("/// system tag (\"Cf. BCP 47 variant subtag\" notes and the ISO 15924\n");
    out.push_str("/// script-variant rows). Sorted by subtag.\n");
    out.push_str("pub(super) static SUBTAG_TAGS: &[(&str, [u8; 4])] = &[\n");
    let pairs: Vec<String> = variants
        .iter()
        .map(|(v, t)| format!("(\"{v}\", {})", tag_literal(t)))
        .collect();
    emit_wrapped(&mut out, &pairs);
    out.push_str("];\n\n");

    out.push_str("/// Three-letter subtags with no entry above whose uppercase form is a\n");
    out.push_str("/// registered tag for a different language. They get no tag instead of\n");
    out.push_str("/// the uppercase fallback. Sorted for binary search.\n");
    out.push_str("pub(super) static NO_UPPERCASE_FALLBACK: &[&str] = &[\n");
    let quoted: Vec<String> = blocked.iter().map(|c| format!("\"{c}\"")).collect();
    emit_wrapped(&mut out, &quoted);
    out.push_str("];\n");
    out
}

// --- Snapshot refresh from raw downloads -----------------------------------

/// Decodes the handful of HTML entities the registry page uses and
/// strips any markup left inside a cell.
fn html_text(cell: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for ch in cell.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
        .trim()
        .to_owned()
}

/// Converts the registry web page into the `ot-language-tags.tsv`
/// snapshot. Only the table matters; the ISO cell keeps the text
/// before any `<br>` (the rest is "See also" commentary).
fn registry_tsv_from_html(html: &str, retrieved: &str) -> String {
    let start = html.find("<table>").expect("registry table start");
    let end = html[start..].find("</table>").expect("registry table end") + start;
    let table = &html[start..end];
    let mut out = String::new();
    let _ = writeln!(out, "# Microsoft OpenType Language System Tag registry");
    let _ = writeln!(out, "# Source: {REGISTRY_URL}");
    let _ = writeln!(out, "# Retrieved: {retrieved}");
    out.push_str("tag\tdeprecated\tname\tiso639\n");
    for row in table.split("<tr>").skip(1) {
        let cells: Vec<&str> = row
            .split("<td>")
            .skip(1)
            .map(|c| c.split("</td>").next().unwrap_or(""))
            .collect();
        if cells.len() != 3 {
            continue;
        }
        let name = html_text(cells[0]);
        let tag_cell = html_text(cells[1]);
        let deprecated = tag_cell.contains("(deprecated)");
        let tag_text = tag_cell
            .replace("(deprecated)", "")
            .replace('*', "")
            .trim()
            .trim_matches('\'')
            .to_owned();
        let tag = format!("{tag_text:<4}");
        assert_eq!(tag.len(), 4, "odd registry tag cell {tag_cell:?}");
        let iso = html_text(cells[2].split("<br>").next().unwrap_or(""));
        let _ = writeln!(out, "{tag}\t{}\t{name}\t{iso}", u8::from(deprecated));
    }
    out
}

/// Keeps the ISO 639-3 identifiers that carry a Part1 code.
fn part1_tsv_from_tab(tab: &str, retrieved: &str) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "# SIL ISO 639-3 code set, filtered to rows with an ISO 639-1 code"
    );
    let _ = writeln!(out, "# Source: {ISO639_3_URL}");
    let _ = writeln!(out, "# Retrieved: {retrieved}");
    out.push_str("id\tpart1\n");
    for line in tab.lines().skip(1) {
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() > 3 && !cols[3].is_empty() {
            let _ = writeln!(out, "{}\t{}", cols[0], cols[3]);
        }
    }
    out
}

fn macro_tsv_from_tab(tab: &str, retrieved: &str) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# SIL ISO 639-3 macrolanguage mappings");
    let _ = writeln!(out, "# Source: {ISO639_MACRO_URL}");
    let _ = writeln!(out, "# Retrieved: {retrieved}");
    out.push_str("macrolanguage\tindividual\tstatus\n");
    for line in tab.lines().skip(1) {
        let line = line.trim_end_matches('\r');
        if !line.is_empty() {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Turns a raw download plus its retrieval date into a snapshot.
type Converter = fn(&str, &str) -> String;

fn refresh_snapshots() {
    let retrieved = std::env::var("SIGILBUZZ_LANGTAGS_RETRIEVED").ok();
    let dir = snapshot_dir();
    let jobs: [(&str, &str, Converter); 3] = [
        (
            "SIGILBUZZ_LANGTAGS_HTML",
            REGISTRY_TSV,
            registry_tsv_from_html,
        ),
        ("SIGILBUZZ_ISO639_3_TAB", PART1_TSV, part1_tsv_from_tab),
        (
            "SIGILBUZZ_ISO639_3_MACRO_TAB",
            MACRO_TSV,
            macro_tsv_from_tab,
        ),
    ];
    for (var, file, convert) in jobs {
        let Ok(path) = std::env::var(var) else {
            continue;
        };
        let date = retrieved
            .as_deref()
            .expect("set SIGILBUZZ_LANGTAGS_RETRIEVED=YYYY-MM-DD when refreshing snapshots");
        let text = read(Path::new(&path));
        std::fs::write(dir.join(file), convert(&text, date)).expect("write snapshot");
    }
}

#[test]
#[ignore = "writes src/language/table.rs; run explicitly to regenerate"]
fn regenerate_language_table() {
    refresh_snapshots();
    let table = generate(&load_sources());
    std::fs::write(root().join(TABLE_RS), table).expect("write table");
}

#[test]
fn committed_language_table_matches_snapshots() {
    let expected = generate(&load_sources());
    let committed = read(&root().join(TABLE_RS)).replace("\r\n", "\n");
    assert!(
        committed == expected,
        "{TABLE_RS} is stale; run `cargo test --test language_table_gen -- --ignored`"
    );
}

#[test]
fn registry_snapshot_parses_known_rows() {
    let src = load_sources();
    let find = |tag: &[u8; 4]| src.registry.iter().find(|r| &r.tag == tag);
    assert_eq!(
        find(b"SRB ").map(|r| r.codes.clone()),
        Some(vec!["cnr".into(), "srp".into()])
    );
    assert_eq!(
        find(b"IPPH").and_then(|r| r.variant.clone()),
        Some("fonipa".into())
    );
    assert!(find(b"DHV ").is_some_and(|r| r.deprecated));
}
