//! Drop-in C-link integration test.
//!
//! Compiles `tests/c/test_shape.c` with the `cc` crate, links the
//! resulting executable against the sigilbuzz cdylib that `cargo
//! test` has just built, runs it, and asserts exit 0.
//!
//! This is the canonical "does sigilbuzz really link as a HarfBuzz
//! drop-in?" check. The C source includes only `hb.h` (no
//! sigilbuzz-specific headers), so any drift between the symbol set
//! `hb.h` declares and the symbols the cdylib actually exports
//! surfaces here at link time.
//!
//! The test is gated `#[cfg(unix)]` because the cdylib path
//! discovery and process invocation use POSIX conventions; Windows
//! support follows once the crate has macOS / Linux landed.
#![cfg(unix)]

use std::fs::{File, OpenOptions};
use std::path::PathBuf;
use std::process::Command;

/// Acquires the workspace-level cdylib build lock used by every
/// C-link test in this file.
///
/// History: #101 introduced a process-local `Mutex<()>` to serialize
/// the `cargo build -p sigilbuzz-capi --lib` invocations these tests
/// fan out. That worked while `cargo test -p sigilbuzz-capi` was the
/// only consumer of the cdylib output, but under
/// `cargo test --workspace` other crates' build / test jobs touch
/// the same `target/debug/libsigilbuzz_capi.{dylib,so}` and the
/// process-local mutex no longer covers them. The visible failure
/// mode is the same one #101 chased: one capi test sees `Finished`
/// while a sibling `cargo build` has the cdylib mid-link and the
/// file briefly disappears, so the C-side `cc` invocation links
/// against nothing.
///
/// An OS-level advisory file lock at
/// `target/sigilbuzz-capi.lock` survives the process boundary, so
/// every workspace job cooperates on the same lock regardless of
/// which crate started it. The lock file itself is content-free:
/// only its inode matters.
fn acquire_workspace_build_lock() -> fd_lock::RwLock<File> {
    let dir = locate_target_dir();
    std::fs::create_dir_all(&dir).expect("create target dir for build lock");
    let path = dir.join("sigilbuzz-capi.lock");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .read(true)
        .open(&path)
        .unwrap_or_else(|e| panic!("open build lock {}: {e}", path.display()));
    fd_lock::RwLock::new(file)
}

/// The bundled Open Sans fixture we shape against. Same file the
/// core sigilbuzz integration tests use.
const OPEN_SANS: &str = "../../tests/fixtures/opensans_regular.ttf";

#[test]
fn c_program_links_and_shapes() {
    run_c_test("test_shape.c", "test_shape");
}

#[test]
fn c_program_links_subset_bridge() {
    run_c_test("test_subset.c", "test_subset");
}

#[test]
fn c_program_links_paint_bridge() {
    run_c_test("test_paint.c", "test_paint");
}

#[test]
fn c_program_links_introspect_helpers() {
    run_c_test("test_introspect.c", "test_introspect");
}

#[test]
fn c_program_follows_harfbuzz_ownership_rules() {
    run_c_test("test_refcount.c", "test_refcount");
}

/// Compiles `tests/c/<source>` against the sigilbuzz cdylib and runs
/// the resulting executable, asserting exit 0. Shared by every C-side
/// integration test in this file.
fn run_c_test(source_filename: &str, exe_basename: &str) {
    // 1. Ensure the cdylib is built and current. `cargo test` only
    //    builds the rlib of the crate under test; the cdylib has to
    //    be requested explicitly so we drive a `cargo build` here.
    //    This costs nothing on a warm tree and guarantees the symbol
    //    set the C source links against matches the source we just
    //    edited.
    //
    //    The build is serialized via a workspace-level file lock so
    //    concurrent C-link tests (even ones running in sibling
    //    workspace crates under `cargo test --workspace`) don't
    //    race on the cdylib output path. We hold the *exclusive*
    //    write side of the lock for the duration of `cargo build`
    //    so no other job can observe the cdylib mid-rebuild, then
    //    drop it before linking so the link step can run in
    //    parallel with other readers.
    let mut build_lock = acquire_workspace_build_lock();
    {
        let _exclusive = build_lock
            .write()
            .expect("acquire exclusive workspace build lock");
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
        let build_status = Command::new(&cargo)
            .args(["build", "-p", "sigilbuzz-capi", "--lib"])
            .status()
            .expect("failed to invoke cargo");
        assert!(
            build_status.success(),
            "cargo build of sigilbuzz-capi failed"
        );
    }

    // 2. Locate the cdylib. Cargo lays this out under
    //    `target/debug/lib<name>.{dylib,so}` (or wherever
    //    `CARGO_TARGET_DIR` points). We hold a *shared* read lock
    //    across the existence-check + link step so a parallel
    //    rebuild can't unlink the file out from under cc.
    let _shared = build_lock
        .read()
        .expect("acquire shared workspace build lock");
    let target_dir = locate_target_dir();
    let cdylib_name = if cfg!(target_os = "macos") {
        "libsigilbuzz_capi.dylib"
    } else {
        "libsigilbuzz_capi.so"
    };
    let cdylib_path = target_dir.join("debug").join(cdylib_name);
    assert!(
        cdylib_path.exists(),
        "expected cdylib at {} after cargo build",
        cdylib_path.display()
    );

    // 3. Compile the C source with cc. We do not link inside this
    //    invocation (cc would default to producing an object file),
    //    so we drive the compiler manually for the executable step.
    let out_dir = target_dir.join("debug").join("c_link_test");
    std::fs::create_dir_all(&out_dir).unwrap();
    let exe_path = out_dir.join(exe_basename);

    // Resolve $CC, falling back to `cc`.
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let c_src = manifest_dir.join("tests/c").join(source_filename);
    let include_dir = manifest_dir.join("include");

    // The cdylib is a `dylib` / `.so` whose import name omits the
    // `lib` prefix and the suffix at link time: `-lsigilbuzz_capi`.
    let mut cmd = Command::new(&cc);
    cmd.arg(&c_src)
        .arg("-I")
        .arg(&include_dir)
        .arg("-L")
        .arg(target_dir.join("debug"))
        .arg("-lsigilbuzz_capi")
        .arg("-o")
        .arg(&exe_path);

    // Linux / BSD need `-Wl,-rpath` to resolve the cdylib at run
    // time; on macOS we set DYLD_LIBRARY_PATH below instead because
    // codesigning interferes with @rpath in test builds.
    if cfg!(target_os = "linux") {
        cmd.arg(format!("-Wl,-rpath,{}", target_dir.join("debug").display()));
    }

    let status = cmd.status().expect("failed to invoke C compiler");
    assert!(
        status.success(),
        "C compilation of {source_filename} failed; ensure $CC is a working C toolchain",
    );

    // 4. Run the resulting binary against the bundled font.
    let mut run = Command::new(&exe_path);
    run.arg(manifest_dir.join(OPEN_SANS));
    if cfg!(target_os = "macos") {
        run.env("DYLD_LIBRARY_PATH", target_dir.join("debug"));
    }
    let output = run.output().expect("failed to run C test binary");
    assert!(
        output.status.success(),
        "{source_filename} exited with {:?}\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Walks up from `CARGO_MANIFEST_DIR` looking for the `target`
/// directory. Cargo lays this out predictably: workspace root /
/// target / debug / libNAME.dylib.
fn locate_target_dir() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // crate dir -> crates/ -> workspace root
    let workspace_root = manifest
        .parent()
        .and_then(|p| p.parent())
        .expect("expected crate to live two levels under workspace root");
    if let Ok(custom) = std::env::var("CARGO_TARGET_DIR") {
        return PathBuf::from(custom);
    }
    workspace_root.join("target")
}
