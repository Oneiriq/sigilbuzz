//! Integration tests for the `sigilbuzz` binary.
//!
//! Each test invokes the binary via [`std::process::Command`] (no
//! `assert_cmd` dep) and asserts on stdout / stderr / exit status. The
//! binary path comes from the `CARGO_BIN_EXE_sigilbuzz` env var Cargo
//! sets for integration tests on `[[bin]]`-only crates.
//!
//! Fonts are loaded via `include_bytes!` from the workspace's bundled
//! fixtures and dropped onto disk under `std::env::temp_dir()` for the
//! duration of the test — the CLI takes paths, not slices.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

const OPEN_SANS: &[u8] = include_bytes!("../../../tests/fixtures/opensans_regular.ttf");

/// Counter so each fixture path is unique across tests in one process.
static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Drops `bytes` into a temp file and returns its path. The file is
/// not cleaned up — `std::env::temp_dir()` is the OS's responsibility,
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
    let (stdout, stderr, ok) = run_cli([
        "shape".as_ref(),
        font.as_os_str(),
        "Hi".as_ref(),
    ]);
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

fn parse_field<'a>(line: &'a str, key: &str) -> &'a str {
    let after = line.find(key).map(|i| &line[i + key.len()..]).unwrap_or(line);
    after.split_whitespace().next().unwrap_or("")
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
    assert!(stderr.contains("kept glyphs"), "expected status line: {stderr}");
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
    let (stdout, stderr, ok) = run_cli([
        "info".as_ref(),
        font.as_os_str(),
    ]);
    assert!(ok, "binary failed: stderr={stderr}");
    assert!(stdout.contains("num_glyphs:"), "missing num_glyphs: {stdout}");
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
    let (stdout, stderr, ok) = run_cli([
        "paint".as_ref(),
        font.as_os_str(),
        "5".as_ref(),
    ]);
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
    // gid 43 is 'H' in Open Sans — picked because the shape test
    // already established it has a non-empty outline.
    let (stdout, stderr, ok) = run_cli([
        "slug".as_ref(),
        font.as_os_str(),
        "43".as_ref(),
    ]);
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
    assert!(svg.starts_with("<svg "), "expected <svg ... > prefix: {svg}");
    assert!(svg.ends_with("</svg>"), "expected </svg> suffix: {svg}");
    assert!(svg.contains("viewBox=\""));
    assert!(svg.contains("<path d=\""));
}

#[test]
fn svg_rejects_glyph_without_outline() {
    // gid 0 is .notdef — Open Sans's .notdef does have an outline,
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
    let (_stdout, stderr, ok) = run_cli([
        "subset".as_ref(),
        font.as_os_str(),
        out.as_os_str(),
    ]);
    assert!(!ok);
    assert!(
        stderr.contains("no glyphs selected"),
        "expected friendly error, got: {stderr}"
    );
}
