//! Regression coverage for `Name::family_name` and friends against
//! every TTF / OTF fixture vendored under `tests/fonts/`.
//!
//! The synthetic-fixture coverage in `src/tables/name.rs` exercises
//! the parser's encoding picker and the storage-bounds checks; this
//! suite locks the contract that on every real font we ship as a
//! parity / shaping fixture, the family name decodes to a non-empty
//! string. Catches regressions where a future change to the encoding
//! ranking, the UTF-16BE decoder, or the lazy bounds check silently
//! starts dropping records on real fonts.

use std::fs;
use std::path::Path;

#[test]
fn family_name_decodes_for_every_vendored_font() {
    let dir = Path::new("tests/fonts");
    let mut tested = 0usize;
    let mut failures: Vec<String> = Vec::new();
    for entry in fs::read_dir(dir).expect("read tests/fonts") {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        let ext = path.extension().and_then(|x| x.to_str()).unwrap_or("");
        if !matches!(ext, "ttf" | "otf") {
            continue;
        }
        let bytes = fs::read(&path).expect("read font");
        // Collections / non-SFNT bytes are a separate concern; skip silently.
        let Ok(face) = sigilbuzz::Face::parse_bytes(&bytes, 0) else {
            continue;
        };
        match face.name() {
            Ok(Some(n)) => match n.family_name() {
                Some(fam) if !fam.is_empty() => tested += 1,
                Some(_) => failures.push(format!("{}: empty family_name", path.display())),
                None => failures.push(format!("{}: family_name None", path.display())),
            },
            Ok(None) => failures.push(format!("{}: name() returned None", path.display())),
            Err(e) => failures.push(format!("{}: name() error {:?}", path.display(), e)),
        }
    }
    assert!(tested > 0, "no fonts found under tests/fonts/");
    assert!(
        failures.is_empty(),
        "{} of {} vendored fonts failed family_name decode:\n{}",
        failures.len(),
        tested + failures.len(),
        failures.join("\n")
    );
}
