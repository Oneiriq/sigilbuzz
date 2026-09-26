//! Bad input must end in an error message and a nonzero exit code,
//! never a panic.
//!
//! Every case runs the real binary and checks that it did not exit
//! with Rust's panic code (101) and that stderr carries no panic
//! message.

use std::ffi::OsStr;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

const OPEN_SANS: &[u8] = include_bytes!("../../../tests/fixtures/opensans_regular.ttf");

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_path(stem: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    std::env::temp_dir().join(format!("sigilbuzz-cli-robust-{stem}-{pid}-{n}"))
}

fn write_temp(stem: &str, bytes: &[u8]) -> PathBuf {
    let path = temp_path(stem);
    std::fs::write(&path, bytes).expect("write fixture");
    path
}

fn run<I, S>(args: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Command::new(env!("CARGO_BIN_EXE_sigilbuzz"))
        .args(args)
        .output()
        .expect("spawn binary")
}

/// Asserts the run failed cleanly: nonzero exit, not the panic exit
/// code, and an error message on stderr.
fn assert_clean_failure(out: &Output, what: &str) {
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{what}: expected failure");
    assert_ne!(out.status.code(), Some(101), "{what}: panicked: {stderr}");
    assert!(!stderr.contains("panicked"), "{what}: panicked: {stderr}");
    assert!(!stderr.trim().is_empty(), "{what}: no error message");
}

/// Asserts the run did not panic. Success and clean failure are both
/// fine.
fn assert_no_panic(out: &Output, what: &str) {
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_ne!(out.status.code(), Some(101), "{what}: panicked: {stderr}");
    assert!(!stderr.contains("panicked"), "{what}: panicked: {stderr}");
}

/// Hostile font files: empty, junk, truncated, and a real font with a
/// scrambled table directory.
fn hostile_fonts() -> Vec<(String, PathBuf)> {
    let mut scrambled = OPEN_SANS.to_vec();
    for b in scrambled.iter_mut().skip(12).take(200) {
        *b ^= 0x5A;
    }
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("empty", Vec::new()),
        ("junk", vec![0xA5; 300]),
        ("header", OPEN_SANS[..12].to_vec()),
        ("half", OPEN_SANS[..OPEN_SANS.len() / 2].to_vec()),
        ("scrambled", scrambled),
        ("woff1-magic", b"wOFF\x00\x01\x00\x00".to_vec()),
        (
            "woff2-magic",
            b"wOF2\x00\x01\x00\x00\xff\xff\xff\xff".to_vec(),
        ),
    ];
    cases
        .into_iter()
        .map(|(name, bytes)| (name.to_string(), write_temp(name, &bytes)))
        .collect()
}

#[test]
fn every_subcommand_survives_hostile_fonts() {
    for (name, font) in hostile_fonts() {
        let f = font.as_os_str();
        let out_path = temp_path("out");
        let o = out_path.as_os_str();
        let cases: Vec<Vec<&OsStr>> = vec![
            vec!["shape".as_ref(), f, "Hello".as_ref()],
            vec!["shape".as_ref(), f, "Hello".as_ref(), "--json".as_ref()],
            vec![
                "subset".as_ref(),
                f,
                o,
                "--gids".as_ref(),
                "0..=10".as_ref(),
            ],
            vec!["subset".as_ref(), f, o, "--unicodes".as_ref(), "A".as_ref()],
            vec!["paint".as_ref(), f, "1".as_ref()],
            vec!["slug".as_ref(), f, "1".as_ref()],
            vec!["woff".as_ref(), "wrap".as_ref(), f, o],
            vec![
                "woff".as_ref(),
                "wrap".as_ref(),
                f,
                o,
                "--format".as_ref(),
                "woff1".as_ref(),
            ],
            vec!["woff".as_ref(), "unwrap".as_ref(), f, o],
            vec![
                "pdf".as_ref(),
                "type3".as_ref(),
                f,
                o,
                "--gids".as_ref(),
                "0..=3".as_ref(),
            ],
            vec!["svg".as_ref(), f, "1".as_ref(), o],
            vec!["svg".as_ref(), f, "1".as_ref(), o, "--color".as_ref()],
            vec!["info".as_ref(), f],
        ];
        for args in cases {
            let what = format!("{name}: {args:?}");
            assert_no_panic(&run(&args), &what);
        }
    }
}

#[test]
fn bad_arguments_are_clean_errors() {
    let font = write_temp("font", OPEN_SANS);
    let f = font.as_os_str();
    let out_path = temp_path("out");
    let o = out_path.as_os_str();
    let missing_dir = temp_path("no-such-dir").join("out.bin");
    let m = missing_dir.as_os_str();
    let cases: Vec<Vec<&OsStr>> = vec![
        vec![
            "shape".as_ref(),
            f,
            "Hi".as_ref(),
            "--features".as_ref(),
            "=,x".as_ref(),
        ],
        vec![
            "shape".as_ref(),
            f,
            "Hi".as_ref(),
            "--features".as_ref(),
            "liga=99999999999".as_ref(),
        ],
        vec![
            "shape".as_ref(),
            f,
            "Hi".as_ref(),
            "--direction".as_ref(),
            "up".as_ref(),
        ],
        vec!["subset".as_ref(), f, o],
        vec!["subset".as_ref(), f, o, "--gids".as_ref(), "5..3".as_ref()],
        vec!["subset".as_ref(), f, o, "--gids".as_ref(), "1,,2".as_ref()],
        vec!["subset".as_ref(), f, o, "--gids".as_ref(), "0x".as_ref()],
        vec!["subset".as_ref(), f, o, "--gids".as_ref(), "70000".as_ref()],
        vec![
            "subset".as_ref(),
            f,
            o,
            "--unicodes".as_ref(),
            "U+110000".as_ref(),
        ],
        vec![
            "subset".as_ref(),
            f,
            o,
            "--unicodes".as_ref(),
            "U+D800".as_ref(),
        ],
        vec![
            "subset".as_ref(),
            f,
            o,
            "--unicodes".as_ref(),
            "U+".as_ref(),
        ],
        vec!["subset".as_ref(), f, o, "--unicodes".as_ref(), "".as_ref()],
        vec!["subset".as_ref(), f, m, "--gids".as_ref(), "1".as_ref()],
        vec![
            "woff".as_ref(),
            "wrap".as_ref(),
            f,
            o,
            "--format".as_ref(),
            "woff3".as_ref(),
        ],
        vec!["woff".as_ref(), "wrap".as_ref(), f, m],
        vec![
            "pdf".as_ref(),
            "type3".as_ref(),
            f,
            m,
            "--gids".as_ref(),
            "1".as_ref(),
        ],
        vec![
            "pdf".as_ref(),
            "type3".as_ref(),
            f,
            o,
            "--gids".as_ref(),
            "a-b".as_ref(),
        ],
        vec!["svg".as_ref(), f, "1".as_ref(), m],
        vec!["svg".as_ref(), f, "3".as_ref(), o],
        vec!["slug".as_ref(), f, "3".as_ref()],
        vec!["slug".as_ref(), f, "65535".as_ref()],
    ];
    for args in cases {
        let what = format!("{args:?}");
        assert_clean_failure(&run(&args), &what);
    }
}

/// Extreme but syntactically valid numbers must not crash the encoder.
#[test]
fn extreme_slug_options_do_not_panic() {
    let font = write_temp("font", OPEN_SANS);
    let f = font.as_os_str();
    let options: [&[&str]; 7] = [
        &["--bands", "0"],
        &["--bands", "4294967295"],
        &["--cubic-tolerance", "NaN"],
        &["--cubic-tolerance", "inf"],
        &["--cubic-tolerance", "-5"],
        &["--cubic-tolerance", "1e-45"],
        &["--cubic-tolerance", "3.4e38"],
    ];
    for extra in options {
        let mut args: Vec<&OsStr> = vec!["slug".as_ref(), f, "36".as_ref()];
        args.extend(extra.iter().map(OsStr::new));
        let out = run(&args);
        assert_no_panic(&out, &format!("{extra:?}"));
    }
}

/// Odd text and large glyph ids go through without a crash.
#[test]
fn odd_text_and_ids_do_not_panic() {
    let font = write_temp("font", OPEN_SANS);
    let f = font.as_os_str();
    let out_path = temp_path("out");
    let o = out_path.as_os_str();
    for text in ["", "\u{FFFD}\u{0301}\u{200D}", "\u{10FFFF}", "a\tb\r\nc"] {
        assert_no_panic(&run([OsStr::new("shape"), f, text.as_ref()]), text);
    }
    let cases: Vec<Vec<&OsStr>> = vec![
        vec!["paint".as_ref(), f, "65535".as_ref()],
        vec!["svg".as_ref(), f, "65535".as_ref(), o, "--color".as_ref()],
        vec![
            "pdf".as_ref(),
            "type3".as_ref(),
            f,
            o,
            "--gids".as_ref(),
            "0..=65535".as_ref(),
        ],
        vec![
            "subset".as_ref(),
            f,
            o,
            "--gids".as_ref(),
            "0xFFFF".as_ref(),
            "--unicodes".as_ref(),
            "A".as_ref(),
        ],
    ];
    for args in cases {
        assert_no_panic(&run(&args), &format!("{args:?}"));
    }
}

/// Output larger than any pipe buffer, written into a pipe whose read
/// end is already closed. `print!` used to panic on the failed write
/// and exit with code 101. The write error is now reported like any
/// other error.
#[test]
fn closed_stdout_is_an_error_not_a_panic() {
    let font = write_temp("font", OPEN_SANS);
    let text = "a".repeat(20_000);
    for json in [false, true] {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_sigilbuzz"));
        cmd.arg("shape").arg(&font).arg(&text);
        if json {
            cmd.arg("--json");
        }
        let mut child = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn binary");
        drop(child.stdout.take());
        let out = child.wait_with_output().expect("wait for binary");
        assert_clean_failure(&out, "shape into a closed pipe");
    }
}
