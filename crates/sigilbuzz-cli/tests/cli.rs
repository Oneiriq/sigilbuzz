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
