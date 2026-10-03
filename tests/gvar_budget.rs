//! A hostile variable font must not make one outline walk run for
//! minutes.
//!
//! The crafted font below is about 120 KB: a composite tree four levels
//! deep that names its child 15 times per level, over a 4-point square,
//! and 4,095 `gvar` tuples on every glyph. Drawing the root visits
//! 50,625 squares, and each visit decodes all 4,095 of the square's
//! tuples, so a cap that only counted the work of one glyph let a single
//! outline call spend close to a minute. The tuple work of the whole
//! walk now shares one cap, as HarfBuzz shares one budget across its
//! `get_points` recursion, and the walk fails with `Malformed` once the
//! cap runs out.

use std::time::{Duration, Instant};

use sigilbuzz::tables::glyf::PhantomMetrics;
use sigilbuzz::tables::{Glyf, Gvar, Hmtx, IndexToLocFormat, Loca};
use sigilbuzz::{Blob, Error, Face};

/// What one call may take in an optimized build. Debug builds are
/// slower and only check the result.
const TIME_LIMIT: Duration = Duration::from_secs(1);

fn be16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_be_bytes());
}

fn be32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_be_bytes());
}

/// The crafted font's tables. Glyph 0 is empty, glyph 1 the square, and
/// glyph `k + 1` a composite naming glyph `k` `fanout` times (every
/// component flagged `USE_MY_METRICS` when `use_my_metrics` is set), up
/// to the root, glyph `levels + 1`. Every glyph but the empty one has
/// `tuples` tuples at the shared peak `wght` 1.0, each over all of its
/// points with zero deltas: six bytes a tuple.
struct Crafted {
    glyf: Vec<u8>,
    loca: Vec<u8>,
    gvar: Vec<u8>,
    hmtx: Vec<u8>,
    num_glyphs: u16,
    root: u16,
}

fn crafted(levels: u16, fanout: u16, tuples: u16, use_my_metrics: bool) -> Crafted {
    let num_glyphs = levels + 2;
    let mut glyphs: Vec<Vec<u8>> = vec![Vec::new()];
    let mut square = Vec::new();
    for v in [1i16, 0, 0, 100, 100] {
        square.extend_from_slice(&v.to_be_bytes());
    }
    be16(&mut square, 3); // endPtsOfContours
    be16(&mut square, 0); // instruction length
    square.extend_from_slice(&[0x01; 4]); // on curve, word coordinates
    for x in [0i16, 100, 0, -100] {
        square.extend_from_slice(&x.to_be_bytes());
    }
    for y in [0i16, 0, 100, 0] {
        square.extend_from_slice(&y.to_be_bytes());
    }
    glyphs.push(square);
    for level in 1..=levels {
        let mut composite = Vec::new();
        for v in [-1i16, 0, 0, 100, 100] {
            composite.extend_from_slice(&v.to_be_bytes());
        }
        for i in 0..fanout {
            // ARG_1_AND_2_ARE_WORDS | ARGS_ARE_XY_VALUES.
            let mut flags = 0x0003;
            if i + 1 < fanout {
                flags |= 0x0020; // MORE_COMPONENTS
            }
            if use_my_metrics {
                flags |= 0x0200;
            }
            be16(&mut composite, flags);
            be16(&mut composite, level);
            be16(&mut composite, 0);
            be16(&mut composite, 0);
        }
        glyphs.push(composite);
    }
    let mut glyf = Vec::new();
    let mut loca = Vec::new();
    for g in &glyphs {
        be32(&mut loca, glyf.len() as u32);
        glyf.extend_from_slice(g);
    }
    be32(&mut loca, glyf.len() as u32);

    let mut data = Vec::new();
    let mut offsets = vec![0u32];
    for gid in 0..num_glyphs {
        if gid > 0 {
            // The glyph's own points plus the four phantom points.
            let count = if gid == 1 { 8 } else { fanout + 4 };
            assert!(count <= 64, "one run of zero deltas per stream");
            be16(&mut data, tuples);
            be16(&mut data, 4 + 4 * tuples);
            for _ in 0..tuples {
                be16(&mut data, 2); // data size
                be16(&mut data, 0); // shared tuple 0, all points
            }
            for _ in 0..tuples {
                let zeros = 0x80 | (count as u8 - 1);
                data.extend_from_slice(&[zeros, zeros]);
            }
        }
        offsets.push(data.len() as u32);
    }
    let mut gvar = Vec::new();
    be16(&mut gvar, 1); // major
    be16(&mut gvar, 0); // minor
    be16(&mut gvar, 1); // axis count
    be16(&mut gvar, 1); // shared tuple count
    let shared_tuples = 20 + 4 * offsets.len() as u32;
    be32(&mut gvar, shared_tuples);
    be16(&mut gvar, num_glyphs);
    be16(&mut gvar, 1); // long offsets
    be32(&mut gvar, shared_tuples + 2);
    for o in offsets {
        be32(&mut gvar, o);
    }
    be16(&mut gvar, 0x4000); // the shared peak, 1.0
    gvar.extend(data);

    let mut hmtx = Vec::new();
    for _ in 0..num_glyphs {
        be16(&mut hmtx, 600);
        be16(&mut hmtx, 0);
    }
    Crafted {
        glyf,
        loca,
        gvar,
        hmtx,
        num_glyphs,
        root: levels + 1,
    }
}

/// The crafted tables in a font, with `head`, `hhea`, `maxp`, and a
/// `cmap` that maps `A` to the root. No `HVAR`, so a varied advance
/// comes from the phantom points.
fn sfnt(c: &Crafted) -> Vec<u8> {
    let mut head = Vec::new();
    be16(&mut head, 1);
    be16(&mut head, 0);
    be32(&mut head, 0x0001_0000); // fontRevision
    be32(&mut head, 0); // checksumAdjustment
    be32(&mut head, 0x5F0F_3CF5); // magic
    be16(&mut head, 0); // flags
    be16(&mut head, 1000); // unitsPerEm
    head.extend_from_slice(&[0; 30]); // dates, bbox, macStyle, ppem, hint
    be16(&mut head, 1); // long loca
    be16(&mut head, 0);
    let mut maxp = Vec::new();
    be32(&mut maxp, 0x0000_5000);
    be16(&mut maxp, c.num_glyphs);
    let mut hhea = Vec::new();
    be16(&mut hhea, 1);
    be16(&mut hhea, 0);
    be16(&mut hhea, 800); // ascender
    hhea.extend_from_slice(&(-200i16).to_be_bytes());
    hhea.extend_from_slice(&[0; 26]); // lineGap through metricDataFormat
    be16(&mut hhea, c.num_glyphs);
    // cmap format 4: 'A' to the root, then the final 0xFFFF segment.
    let mut cmap = Vec::new();
    for v in [0, 1, 3, 1] {
        be16(&mut cmap, v);
    }
    be32(&mut cmap, 12);
    for v in [4, 32, 0, 4, 4, 1, 0, 0x41, 0xFFFF, 0, 0x41, 0xFFFF] {
        be16(&mut cmap, v);
    }
    for v in [c.root.wrapping_sub(0x41), 1, 0, 0] {
        be16(&mut cmap, v);
    }
    let tables: [(&[u8; 4], &[u8]); 8] = [
        (b"cmap", &cmap),
        (b"glyf", &c.glyf),
        (b"gvar", &c.gvar),
        (b"head", &head),
        (b"hhea", &hhea),
        (b"hmtx", &c.hmtx),
        (b"loca", &c.loca),
        (b"maxp", &maxp),
    ];
    let mut out = Vec::new();
    be32(&mut out, 0x0001_0000);
    be16(&mut out, tables.len() as u16);
    out.extend_from_slice(&[0; 6]);
    let mut offset = 12 + 16 * tables.len();
    for (tag, body) in &tables {
        out.extend_from_slice(*tag);
        be32(&mut out, 0);
        be32(&mut out, offset as u32);
        be32(&mut out, body.len() as u32);
        offset += (body.len() + 3) & !3;
    }
    for (_, body) in &tables {
        out.extend_from_slice(body);
        out.resize((out.len() + 3) & !3, 0);
    }
    out
}

/// Runs `f`, checks it stayed within [`TIME_LIMIT`] in an optimized
/// build, and returns its result.
fn timed<T>(what: &str, f: impl FnOnce() -> T) -> T {
    let start = Instant::now();
    let out = f();
    let elapsed = start.elapsed();
    if !cfg!(debug_assertions) {
        assert!(elapsed < TIME_LIMIT, "{what} took {elapsed:?}");
    }
    out
}

fn assert_over_budget<T: std::fmt::Debug>(result: Result<T, Error>) {
    assert!(
        matches!(
            result,
            Err(Error::Malformed {
                context: "gvar variation work exceeds the cap",
                ..
            })
        ),
        "{result:?}"
    );
}

#[test]
fn a_crafted_composite_tree_stops_at_the_shared_budget() {
    let c = crafted(4, 15, 4095, false);
    let bytes = sfnt(&c);
    assert!(bytes.len() < 130_000, "{}", bytes.len());
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    assert_over_budget(timed("outline", || {
        face.glyph_outline_at_coords(c.root, &[1.0])
    }));
    assert_over_budget(timed("bounds", || {
        face.glyph_bounds_at_coords(c.root, &[1.0])
    }));
    // The static outline is all 50,625 squares.
    let outline = face.glyph_outline_at_coords(c.root, &[]).unwrap().unwrap();
    assert_eq!(outline.ops().len(), 50_625 * 6);
}

#[test]
fn a_crafted_use_my_metrics_tree_stops_at_the_shared_budget() {
    let c = crafted(3, 15, 4095, true);
    let loca = Loca::parse(&c.loca, IndexToLocFormat::Long, c.num_glyphs).unwrap();
    let glyf = Glyf::new(&c.glyf);
    let gvar = Gvar::parse(&c.gvar).unwrap();
    let hmtx = Hmtx::parse(&c.hmtx, c.num_glyphs, c.num_glyphs).unwrap();
    let metrics = PhantomMetrics {
        hmtx: &hmtx,
        vmtx: None,
    };
    assert_over_budget(timed("phantom points", || {
        glyf.phantom_points_at_coords(&loca, c.root, &metrics, Some(&gvar), &[1.0])
    }));
}
