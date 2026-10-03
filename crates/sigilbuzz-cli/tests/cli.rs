//! Integration tests for the `sigilbuzz` binary.
//!
//! Each test invokes the binary via [`std::process::Command`] (no
//! `assert_cmd` dep) and asserts on stdout / stderr / exit status. The
//! binary path comes from the `CARGO_BIN_EXE_sigilbuzz` env var Cargo
//! sets for integration tests on `[[bin]]`-only crates.
//!
//! Fonts are loaded via `include_bytes!` from the workspace's bundled
//! fixtures and dropped onto disk under `std::env::temp_dir()` for the
//! duration of the test. The CLI takes paths, not slices.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

const OPEN_SANS: &[u8] = include_bytes!("../../../tests/fixtures/opensans_regular.ttf");
const AMIRI: &[u8] = include_bytes!("../../../tests/fixtures/amiri_regular.ttf");

/// Counter so each fixture path is unique across tests in one process.
static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Drops `bytes` into a temp file and returns its path. The file is
/// not cleaned up: `std::env::temp_dir()` is the OS's responsibility,
/// and a stable filename per (test, counter) is friendlier for
/// post-mortem debugging than a `tempfile`-style auto-delete.
fn write_tempfile(stem: &str, bytes: &[u8]) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let path = std::env::temp_dir().join(format!("sigilbuzz-cli-{stem}-{pid}-{n}"));
    std::fs::write(&path, bytes).expect("write fixture");
    path
}

/// Runs the binary with `args`, returns `(stdout_str, stderr_str,
/// success_bool)`. Panics on a missing binary.
fn run_cli<I, S>(args: I) -> (String, String, bool)
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let bin = env!("CARGO_BIN_EXE_sigilbuzz");
    let out = Command::new(bin).args(args).output().expect("spawn binary");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    (stdout, stderr, out.status.success())
}

fn open_sans_path() -> PathBuf {
    write_tempfile("opensans.ttf", OPEN_SANS)
}

#[test]
fn shape_emits_one_line_per_glyph() {
    let font = open_sans_path();
    let (stdout, stderr, ok) = run_cli(["shape".as_ref(), font.as_os_str(), "Hi".as_ref()]);
    assert!(ok, "binary failed: stderr={stderr}");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 2, "expected 2 glyphs, got: {stdout}");
    for line in &lines {
        assert!(line.starts_with("gid="), "bad line: {line}");
        assert!(line.contains("advance="), "bad line: {line}");
        assert!(line.contains("cluster="), "bad line: {line}");
    }
    // 'H' and 'i' shape to two distinct gids in Open Sans.
    let gid0 = parse_field(lines[0], "gid=");
    let gid1 = parse_field(lines[1], "gid=");
    assert_ne!(gid0, gid1, "H and i should map to different gids");
}

#[test]
fn shape_json_emits_valid_array() {
    let font = open_sans_path();
    let (stdout, stderr, ok) = run_cli([
        "shape".as_ref(),
        font.as_os_str(),
        "Hi".as_ref(),
        "--json".as_ref(),
    ]);
    assert!(ok, "binary failed: stderr={stderr}");
    let trimmed = stdout.trim();
    assert!(trimmed.starts_with('['), "json must start with [: {stdout}");
    assert!(trimmed.ends_with(']'), "json must end with ]: {stdout}");
    // Two glyphs => one separator comma between them.
    assert_eq!(
        trimmed.matches("\"gid\":").count(),
        2,
        "expected two gid entries: {stdout}"
    );
}

#[test]
fn shape_rtl_prints_visual_order() {
    // hb-shape parity: an RTL run comes out reversed, so the cluster
    // values count down.
    let font = open_sans_path();
    let shape_with = |direction: &str| {
        let (stdout, stderr, ok) = run_cli([
            "shape".as_ref(),
            font.as_os_str(),
            "Hi".as_ref(),
            "--direction".as_ref(),
            direction.as_ref(),
        ]);
        assert!(ok, "binary failed: stderr={stderr}");
        stdout
            .lines()
            .map(|l| {
                (
                    parse_field(l, "gid=").to_owned(),
                    parse_field(l, "cluster=").to_owned(),
                )
            })
            .collect::<Vec<_>>()
    };
    let ltr = shape_with("ltr");
    let rtl = shape_with("rtl");
    assert_eq!(ltr.len(), 2);
    assert_eq!(ltr[0].1, "0");
    assert_eq!(rtl[0].1, "1", "RTL output starts with the last cluster");
    let mut reversed = rtl.clone();
    reversed.reverse();
    assert_eq!(ltr, reversed);
}

#[test]
fn shape_rejects_missing_font() {
    let (_stdout, stderr, ok) = run_cli([
        "shape".as_ref(),
        Path::new("/no/such/font.ttf").as_os_str(),
        "Hi".as_ref(),
    ]);
    assert!(!ok);
    assert!(
        stderr.contains("could not read font"),
        "expected friendly error, got: {stderr}"
    );
}

#[test]
fn shape_rejects_bad_feature_tag() {
    let font = open_sans_path();
    let (_stdout, stderr, ok) = run_cli([
        "shape".as_ref(),
        font.as_os_str(),
        "Hi".as_ref(),
        "--features".as_ref(),
        "lig".as_ref(), // 3 chars, not 4
    ]);
    assert!(!ok);
    assert!(
        stderr.contains("must be exactly 4"),
        "expected feature-tag error, got: {stderr}"
    );
}

/// Glyph ids from `sigilbuzz shape font text extra...`.
fn shaped_gids(font: &Path, text: &str, extra: &[&str]) -> Vec<String> {
    let mut args: Vec<&std::ffi::OsStr> = vec!["shape".as_ref(), font.as_os_str(), text.as_ref()];
    args.extend(extra.iter().map(std::ffi::OsStr::new));
    let (stdout, stderr, ok) = run_cli(args);
    assert!(ok, "binary failed: stderr={stderr}");
    stdout
        .lines()
        .map(|l| parse_field(l, "gid=").to_owned())
        .collect()
}

#[test]
fn shape_language_selects_the_language_system() {
    // Open Sans has Romanian comma-below forms under latn/ROM.
    let font = open_sans_path();
    let text = "\u{0218}\u{0219}";
    let default = shaped_gids(&font, text, &[]);
    let romanian = shaped_gids(&font, text, &["--language", "ro"]);
    assert_eq!(default.len(), 2);
    assert_ne!(default, romanian, "--language ro must reach locl");
    assert_eq!(romanian, shaped_gids(&font, text, &["--language", "ro-RO"]));
}

#[test]
fn shape_script_shapes_the_whole_text_as_one_script() {
    // As Latin, the Arabic letters are not joined: they keep their
    // isolated forms instead of init + fina.
    let font = write_tempfile("amiri.ttf", AMIRI);
    let text = "\u{0628}\u{0628}";
    let arabic = shaped_gids(&font, text, &["--direction", "rtl"]);
    let latin = shaped_gids(&font, text, &["--direction", "rtl", "--script", "latn"]);
    assert_eq!(arabic.len(), 2);
    assert_ne!(arabic, latin);
    assert_eq!(latin[0], latin[1], "unjoined behs share one glyph");
    assert_eq!(
        arabic,
        shaped_gids(&font, text, &["--direction", "rtl", "--script", "Arab"])
    );
}

/// Clusters from `sigilbuzz shape font text extra...`.
fn shaped_clusters(font: &Path, text: &str, extra: &[&str]) -> Vec<String> {
    let mut args: Vec<&std::ffi::OsStr> = vec!["shape".as_ref(), font.as_os_str(), text.as_ref()];
    args.extend(extra.iter().map(std::ffi::OsStr::new));
    let (stdout, stderr, ok) = run_cli(args);
    assert!(ok, "binary failed: stderr={stderr}");
    stdout
        .lines()
        .map(|l| parse_field(l, "cluster=").to_owned())
        .collect()
}

#[test]
fn shape_bidi_prints_runs_in_visual_order() {
    // "ab " then beh, alef: the Arabic run comes out right to left
    // after the Latin run, and the clusters index the input text.
    let font = write_tempfile("amiri.ttf", AMIRI);
    let text = "ab \u{0628}\u{0627}";
    assert_eq!(
        shaped_clusters(&font, text, &["--bidi"]),
        ["0", "1", "2", "5", "3"]
    );
    // Forcing a right-to-left paragraph puts the Latin run at the right.
    assert_eq!(
        shaped_clusters(&font, text, &["--bidi", "--direction", "rtl"]),
        ["5", "3", "2", "0", "1"]
    );
    let (_stdout, stderr, ok) = run_cli([
        "shape".as_ref(),
        font.as_os_str(),
        text.as_ref(),
        "--bidi".as_ref(),
        "--direction".as_ref(),
        "ttb".as_ref(),
    ]);
    assert!(!ok);
    assert!(stderr.contains("ltr or rtl"), "{stderr}");
}

#[test]
fn shape_bidi_orders_each_paragraph_on_its_own() {
    // An Arabic paragraph ("beh alef ab" and U+2029), then a Latin one.
    // The first is right to left: its separator (byte 7) at the left,
    // then "ab", then the Arabic letters. The second follows, left to
    // right.
    let font = write_tempfile("amiri.ttf", AMIRI);
    let text = "\u{0628}\u{0627} ab\u{2029}cd";
    assert_eq!(
        shaped_clusters(&font, text, &["--bidi"]),
        ["7", "5", "6", "4", "2", "0", "10", "11"]
    );
}

#[test]
fn shape_rejects_bad_script_and_language() {
    let font = open_sans_path();
    for (flag, value) in [
        ("--script", "Arabic"),
        ("--script", "ar1b"),
        ("--language", "@"),
    ] {
        let (_stdout, stderr, ok) = run_cli([
            "shape".as_ref(),
            font.as_os_str(),
            "Hi".as_ref(),
            flag.as_ref(),
            value.as_ref(),
        ]);
        assert!(!ok, "{flag} {value} should fail");
        assert!(stderr.contains("invalid"), "{flag} {value}: {stderr}");
    }
}

fn parse_field<'a>(line: &'a str, key: &str) -> &'a str {
    let after = line
        .find(key)
        .map(|i| &line[i + key.len()..])
        .unwrap_or(line);
    after.split_whitespace().next().unwrap_or("")
}

#[test]
fn binary_advertises_every_subcommand_in_help() {
    let (stdout, _stderr, ok) = run_cli(["--help"]);
    assert!(ok, "help should always succeed");
    for sub in [
        "shape", "subset", "paint", "slug", "woff", "pdf", "svg", "info",
    ] {
        assert!(
            stdout.contains(sub),
            "help missing subcommand '{sub}': {stdout}"
        );
    }
}

#[test]
fn binary_reports_version() {
    let (stdout, _stderr, ok) = run_cli(["--version"]);
    assert!(ok);
    assert!(stdout.contains("sigilbuzz"), "version line: {stdout}");
}

#[test]
fn subset_then_shape_round_trip() {
    // End-to-end: subset down to .notdef + 'H' + 'i', then shape "Hi"
    // against the subset and confirm it produces two glyphs.
    let font = open_sans_path();
    let sub = write_tempfile("e2e_subset.ttf", b"");
    let (_so, stderr, ok) = run_cli([
        "subset".as_ref(),
        font.as_os_str(),
        sub.as_os_str(),
        "--unicodes".as_ref(),
        "H,i".as_ref(),
    ]);
    assert!(ok, "subset failed: stderr={stderr}");
    let (stdout, stderr, ok) = run_cli(["shape".as_ref(), sub.as_os_str(), "Hi".as_ref()]);
    assert!(ok, "shape on subset failed: stderr={stderr}");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines.len(),
        2,
        "expected 2 glyphs after subset round-trip: {stdout}"
    );
}

#[test]
fn subset_writes_smaller_font_for_gid_list() {
    let font = open_sans_path();
    let out = write_tempfile("subset.ttf", b"");
    let (_stdout, stderr, ok) = run_cli([
        "subset".as_ref(),
        font.as_os_str(),
        out.as_os_str(),
        "--gids".as_ref(),
        "0,1,2,3".as_ref(),
    ]);
    assert!(ok, "binary failed: stderr={stderr}");
    let bytes = std::fs::read(&out).expect("read output");
    let src_len = OPEN_SANS.len();
    assert!(
        bytes.len() < src_len,
        "subset ({}) should be smaller than source ({})",
        bytes.len(),
        src_len
    );
    assert!(
        stderr.contains("kept glyphs"),
        "expected status line: {stderr}"
    );
}

#[test]
fn subset_resolves_unicodes_through_cmap() {
    let font = open_sans_path();
    let out = write_tempfile("subset_uni.ttf", b"");
    let (_stdout, stderr, ok) = run_cli([
        "subset".as_ref(),
        font.as_os_str(),
        out.as_os_str(),
        "--unicodes".as_ref(),
        "H,i,A,B".as_ref(),
    ]);
    assert!(ok, "binary failed: stderr={stderr}");
    assert!(out.exists());
    // Output should be parseable as a Face.
    let bytes = std::fs::read(&out).expect("read output");
    let blob = sigilbuzz::Blob::from_vec(bytes);
    let _face = sigilbuzz::Face::parse(&blob, 0).expect("parsed subset");
}

#[test]
fn subset_accepts_inclusive_range() {
    let font = open_sans_path();
    let out = write_tempfile("subset_range.ttf", b"");
    let (_stdout, stderr, ok) = run_cli([
        "subset".as_ref(),
        font.as_os_str(),
        out.as_os_str(),
        "--gids".as_ref(),
        "0..=10".as_ref(),
    ]);
    assert!(ok, "binary failed: stderr={stderr}");
    assert!(out.exists());
}

#[test]
fn info_prints_table_list_and_features() {
    let font = open_sans_path();
    let (stdout, stderr, ok) = run_cli(["info".as_ref(), font.as_os_str()]);
    assert!(ok, "binary failed: stderr={stderr}");
    assert!(
        stdout.contains("num_glyphs:"),
        "missing num_glyphs: {stdout}"
    );
    assert!(stdout.contains("units_per_em:"), "missing upem: {stdout}");
    assert!(stdout.contains("tables ("), "missing tables list: {stdout}");
    // Open Sans carries cmap/head/hhea/hmtx/maxp/glyf/loca at minimum.
    for tag in &["cmap", "head", "hhea", "hmtx", "maxp", "glyf", "loca"] {
        assert!(stdout.contains(tag), "missing table {tag}: {stdout}");
    }
    // Open Sans carries GSUB and GPOS so we expect a feature list.
    assert!(stdout.contains("features ("), "missing features: {stdout}");
}

#[test]
fn woff_round_trips_ttf_through_woff2() {
    let font = open_sans_path();
    let woff2 = write_tempfile("round.woff2", b"");
    let recovered = write_tempfile("recovered.ttf", b"");
    let (_so, stderr, ok) = run_cli([
        "woff".as_ref(),
        "wrap".as_ref(),
        font.as_os_str(),
        woff2.as_os_str(),
        "--format".as_ref(),
        "woff2".as_ref(),
    ]);
    assert!(ok, "wrap failed: stderr={stderr}");
    let (_so, stderr, ok) = run_cli([
        "woff".as_ref(),
        "unwrap".as_ref(),
        woff2.as_os_str(),
        recovered.as_os_str(),
    ]);
    assert!(ok, "unwrap failed: stderr={stderr}");
    let bytes = std::fs::read(&recovered).expect("read recovered");
    let blob = sigilbuzz::Blob::from_vec(bytes);
    let _face = sigilbuzz::Face::parse(&blob, 0).expect("recovered face parses");
}

#[test]
fn woff_unwrap_rejects_non_woff_input() {
    let font = open_sans_path();
    let out = write_tempfile("bogus.ttf", b"");
    let (_so, stderr, ok) = run_cli([
        "woff".as_ref(),
        "unwrap".as_ref(),
        font.as_os_str(),
        out.as_os_str(),
    ]);
    assert!(!ok);
    assert!(
        stderr.contains("unknown WOFF magic"),
        "expected friendly error, got: {stderr}"
    );
}

#[test]
fn pdf_type3_emits_charprocs() {
    let font = open_sans_path();
    let out = write_tempfile("type3.pdfx", b"");
    let (_stdout, stderr, ok) = run_cli([
        "pdf".as_ref(),
        "type3".as_ref(),
        font.as_os_str(),
        out.as_os_str(),
        "--gids".as_ref(),
        "0..=4".as_ref(),
    ]);
    assert!(ok, "binary failed: stderr={stderr}");
    let body = std::fs::read_to_string(&out).expect("read output");
    assert!(body.contains("FontBBox"), "missing FontBBox: {body}");
    assert!(body.contains("FontMatrix"), "missing FontMatrix: {body}");
    assert!(body.contains("CharProc[0]"), "missing CharProc[0]: {body}");
    assert!(body.contains("stream"), "missing stream marker: {body}");
}

#[test]
fn paint_reports_no_colr_for_open_sans() {
    // Open Sans carries no COLR table, so paint exits 0 and prints
    // a friendly message to stderr (not stdout).
    let font = open_sans_path();
    let (stdout, stderr, ok) = run_cli(["paint".as_ref(), font.as_os_str(), "5".as_ref()]);
    assert!(ok, "binary failed: stderr={stderr}");
    assert!(stdout.is_empty(), "stdout should be empty: {stdout}");
    assert!(
        stderr.contains("no COLRv1 paint tree"),
        "expected no-COLR message, got: {stderr}"
    );
}

#[test]
fn slug_emits_valid_json_for_outline_glyph() {
    let font = open_sans_path();
    // gid 43 is 'H' in Open Sans, picked because the shape test
    // already established it has a non-empty outline.
    let (stdout, stderr, ok) = run_cli(["slug".as_ref(), font.as_os_str(), "43".as_ref()]);
    assert!(ok, "binary failed: stderr={stderr}");
    let trimmed = stdout.trim();
    assert!(trimmed.starts_with('{') && trimmed.ends_with('}'));
    assert!(trimmed.contains("\"bbox\":"));
    assert!(trimmed.contains("\"bands\":["));
    assert!(trimmed.contains("\"segments\":["));
}

#[test]
fn svg_writes_well_formed_document() {
    let font = open_sans_path();
    let out = write_tempfile("glyph.svg", b"");
    let (_stdout, stderr, ok) = run_cli([
        "svg".as_ref(),
        font.as_os_str(),
        "43".as_ref(),
        out.as_os_str(),
    ]);
    assert!(ok, "binary failed: stderr={stderr}");
    let svg = std::fs::read_to_string(&out).expect("read svg");
    assert!(
        svg.starts_with("<svg "),
        "expected <svg ... > prefix: {svg}"
    );
    assert!(svg.ends_with("</svg>"), "expected </svg> suffix: {svg}");
    assert!(svg.contains("viewBox=\""));
    assert!(svg.contains("<path d=\""));
}

#[test]
fn svg_rejects_glyph_without_outline() {
    // gid 0 is .notdef. Open Sans's .notdef does have an outline,
    // so we go for an out-of-range gid instead.
    let font = open_sans_path();
    let out = write_tempfile("glyph_oob.svg", b"");
    let (_stdout, stderr, ok) = run_cli([
        "svg".as_ref(),
        font.as_os_str(),
        "65535".as_ref(),
        out.as_os_str(),
    ]);
    assert!(!ok);
    assert!(
        stderr.contains("no outline"),
        "expected friendly error, got: {stderr}"
    );
}

#[test]
fn subset_rejects_empty_selection() {
    let font = open_sans_path();
    let out = write_tempfile("subset_empty.ttf", b"");
    let (_stdout, stderr, ok) = run_cli(["subset".as_ref(), font.as_os_str(), out.as_os_str()]);
    assert!(!ok);
    assert!(
        stderr.contains("no glyphs selected"),
        "expected friendly error, got: {stderr}"
    );
}

/// Runs `sigilbuzz subset` on Open Sans with `extra` arguments.
/// Returns the output bytes (empty on failure), stderr, and success.
fn subset_open_sans(stem: &str, extra: &[&std::ffi::OsStr]) -> (Vec<u8>, String, bool) {
    let font = open_sans_path();
    let out = write_tempfile(stem, b"");
    let mut args: Vec<&std::ffi::OsStr> =
        vec!["subset".as_ref(), font.as_os_str(), out.as_os_str()];
    args.extend_from_slice(extra);
    let (_stdout, stderr, ok) = run_cli(args);
    let bytes = if ok {
        std::fs::read(&out).expect("read output")
    } else {
        Vec::new()
    };
    (bytes, stderr, ok)
}

#[test]
fn subset_text_keeps_the_same_glyphs_as_unicodes() {
    let (by_unicodes, stderr, ok) = subset_open_sans(
        "subset_hi_uni.ttf",
        &["--unicodes".as_ref(), "H,i".as_ref()],
    );
    assert!(ok, "{stderr}");
    // Repeated characters count once.
    let (by_text, stderr, ok) =
        subset_open_sans("subset_hi_text.ttf", &["--text".as_ref(), "HiHi".as_ref()]);
    assert!(ok, "{stderr}");
    assert_eq!(by_text, by_unicodes);
    // A text file loses its line breaks and leading byte order mark.
    let file = write_tempfile("subset_hi.txt", "\u{FEFF}H\r\ni\n".as_bytes());
    let (by_file, stderr, ok) = subset_open_sans(
        "subset_hi_file.ttf",
        &["--text-file".as_ref(), file.as_os_str()],
    );
    assert!(ok, "{stderr}");
    assert_eq!(by_file, by_unicodes);
}

#[test]
fn subset_text_combines_with_unicodes_and_gids() {
    let (split, stderr, ok) = subset_open_sans(
        "subset_split.ttf",
        &[
            "--unicodes".as_ref(),
            "H".as_ref(),
            "--text".as_ref(),
            "i".as_ref(),
            "--gids".as_ref(),
            "0".as_ref(),
        ],
    );
    assert!(ok, "{stderr}");
    let (joined, stderr, ok) =
        subset_open_sans("subset_joined.ttf", &["--text".as_ref(), "Hi".as_ref()]);
    assert!(ok, "{stderr}");
    assert_eq!(split, joined);
}

#[test]
fn subset_text_file_must_exist() {
    let missing = std::env::temp_dir().join("sigilbuzz-cli-no-such-text-file.txt");
    let (_, stderr, ok) = subset_open_sans(
        "subset_nofile.ttf",
        &["--text-file".as_ref(), missing.as_os_str()],
    );
    assert!(!ok);
    assert!(stderr.contains("could not read text file"), "{stderr}");
}

#[test]
fn subset_rejects_missing_characters_unless_asked_to_skip() {
    // Open Sans has no Hangul.
    let (_, stderr, ok) = subset_open_sans(
        "subset_missing.ttf",
        &["--text".as_ref(), "H\u{AC00}".as_ref()],
    );
    assert!(!ok);
    assert!(stderr.contains("U+AC00 has no cmap entry"), "{stderr}");
    assert!(
        stderr.contains("--skip-missing"),
        "the error names the flag: {stderr}"
    );

    let (skipped, stderr, ok) = subset_open_sans(
        "subset_skip.ttf",
        &[
            "--text".as_ref(),
            "H\u{AC00}\u{AC01}".as_ref(),
            "--skip-missing".as_ref(),
        ],
    );
    assert!(ok, "{stderr}");
    assert!(
        stderr.contains("skipped 2 characters the font has no glyph for: U+AC00, U+AC01"),
        "{stderr}"
    );
    let (just_h, _, _) = subset_open_sans("subset_h.ttf", &["--unicodes".as_ref(), "H".as_ref()]);
    assert_eq!(skipped, just_h);

    // Skipping every character leaves nothing to keep.
    let (_, stderr, ok) = subset_open_sans(
        "subset_skip_all.ttf",
        &[
            "--text".as_ref(),
            "\u{AC00}".as_ref(),
            "--skip-missing".as_ref(),
        ],
    );
    assert!(!ok);
    assert!(stderr.contains("skipped 1 character the font"), "{stderr}");
    assert!(
        stderr.contains("none of the requested characters"),
        "{stderr}"
    );
}

#[test]
fn subset_keeps_vertical_metrics_for_vertical_text() {
    // A CJK subset must lay out vertical text like its source. Subsets
    // used to drop vhea, vmtx and VORG, which turned these 1000-unit
    // advances into 1448.
    const NOTO_KR: &[u8] =
        include_bytes!("../../../tests/fixtures/noto_sans_kr_vf_vertical_subset.otf");
    let font = write_tempfile("noto_kr.otf", NOTO_KR);
    let out = write_tempfile("noto_kr_sub.otf", b"");
    let text = "\u{300C}\u{300D}\u{3002}";
    let (_, stderr, ok) = run_cli([
        "subset".as_ref(),
        font.as_os_str(),
        out.as_os_str(),
        "--text".as_ref(),
        text.as_ref(),
    ]);
    assert!(ok, "{stderr}");
    let ttb = |path: &Path| {
        let (stdout, stderr, ok) = run_cli([
            "shape".as_ref(),
            path.as_os_str(),
            text.as_ref(),
            "--direction".as_ref(),
            "ttb".as_ref(),
            "--json".as_ref(),
        ]);
        assert!(ok, "{stderr}");
        stdout
            .split("},{")
            .map(|g| {
                ["y_advance\":", "x_offset\":", "y_offset\":"]
                    .map(|key| parse_json_int(g, key))
                    .to_vec()
            })
            .collect::<Vec<_>>()
    };
    let want = ttb(&font);
    assert_eq!(want, vec![vec![-1000, -500, -880]; 3]);
    assert_eq!(ttb(&out), want);
}

/// The integer after `key` in a flat JSON object.
fn parse_json_int(object: &str, key: &str) -> i32 {
    let rest = &object[object.find(key).expect("key present") + key.len()..];
    let end = rest
        .find(|c: char| c != '-' && !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse().expect("an integer")
}
