//! `OwnedFace` equivalence: the owned face must behave byte-for-byte
//! like a freshly parsed borrowed [`Face`] across every vendored
//! fixture font, and must be shareable across threads.

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::thread;

use sigilbuzz::{Blob, Face, OwnedFace};

/// Every outline fixture under `tests/fonts/` (TTF glyf and OTF CFF).
fn fixture_fonts() -> Vec<(String, Vec<u8>)> {
    let dir = Path::new("tests/fonts");
    let mut fonts = Vec::new();
    for entry in fs::read_dir(dir).expect("read tests/fonts") {
        let path = entry.expect("dir entry").path();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if ext == "ttf" || ext == "otf" {
            let bytes = fs::read(&path).expect("read font");
            fonts.push((path.display().to_string(), bytes));
        }
    }
    assert!(!fonts.is_empty(), "no fixture fonts found");
    fonts
}

#[test]
fn owned_face_matches_borrowed_face_on_every_fixture() {
    for (name, bytes) in fixture_fonts() {
        let blob = Blob::new(&bytes);
        let face = Face::parse(&blob, 0).unwrap_or_else(|e| panic!("{name}: borrowed parse: {e}"));
        let owned = OwnedFace::parse(bytes.clone(), 0)
            .unwrap_or_else(|e| panic!("{name}: owned parse: {e}"));

        assert_eq!(owned.sfnt_version(), face.sfnt_version(), "{name}");
        assert_eq!(owned.num_tables(), face.num_tables(), "{name}");

        let view = owned.as_face();
        assert_eq!(view.records(), face.records(), "{name}");

        let num_glyphs = face
            .maxp()
            .unwrap_or_else(|e| panic!("{name}: maxp: {e}"))
            .num_glyphs;
        for gid in 0..num_glyphs.min(40) {
            let borrowed_bounds = face.glyph_bounds(gid);
            let owned_bounds = view.glyph_bounds(gid);
            assert_eq!(
                borrowed_bounds.is_err(),
                owned_bounds.is_err(),
                "{name} gid {gid}: bounds err mismatch"
            );
            assert_eq!(
                borrowed_bounds.ok(),
                owned_bounds.ok(),
                "{name} gid {gid}: bounds"
            );

            let borrowed_outline = face.glyph_outline(gid);
            let owned_outline = view.glyph_outline(gid);
            assert_eq!(
                borrowed_outline.is_err(),
                owned_outline.is_err(),
                "{name} gid {gid}: outline err mismatch"
            );
            assert_eq!(
                borrowed_outline.ok(),
                owned_outline.ok(),
                "{name} gid {gid}: outline"
            );
        }
    }
}

#[test]
fn owned_face_extracts_outlines_across_threads() {
    let (name, bytes) = fixture_fonts().into_iter().next().expect("one fixture");
    let owned =
        Arc::new(OwnedFace::parse(bytes, 0).unwrap_or_else(|e| panic!("{name}: owned parse: {e}")));

    let num_glyphs = owned.as_face().maxp().expect("maxp").num_glyphs.min(32);

    // Reference outlines, single-threaded.
    let reference: Vec<_> = (0..num_glyphs)
        .map(|gid| owned.as_face().glyph_outline(gid).ok())
        .collect();

    // The same extraction fanned out over four threads sharing one parse.
    let mut results: Vec<Option<_>> = (0..num_glyphs).map(|_| None).collect();
    thread::scope(|s| {
        let chunk = (usize::from(num_glyphs) / 4).max(1);
        for (t, out_chunk) in results.chunks_mut(chunk).enumerate() {
            let owned = Arc::clone(&owned);
            let start = t * chunk;
            s.spawn(move || {
                for (i, slot) in out_chunk.iter_mut().enumerate() {
                    #[allow(clippy::cast_possible_truncation)]
                    let gid = (start + i) as u16;
                    *slot = Some(owned.as_face().glyph_outline(gid).ok());
                }
            });
        }
    });

    for (gid, (got, want)) in results.into_iter().zip(reference).enumerate() {
        assert_eq!(
            got.expect("thread filled slot"),
            want,
            "gid {gid} outline differs across threads"
        );
    }
}

#[test]
fn owned_face_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<OwnedFace>();
}
