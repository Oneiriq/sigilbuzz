//! GPOS attachment offsets checked against HarfBuzz.
//!
//! HarfBuzz 14.5.0 gives a mark the cross-stream offset of its parent
//! (y in horizontal runs, x in vertical ones, summed over the parent's
//! cursive chain) when the mark attaches (`resolve_cross_offset` in
//! `OT/Layout/GPOS/MarkArray.hh`). Only the main-direction offset of
//! the parent reaches the mark at the end of GPOS
//! (`propagate_attachment_offsets` in `OT/Layout/GPOS/GPOS.hh`). So a
//! lookup that raises a base after its mark attached leaves the mark
//! where it was, while a shift along the line still carries the mark.
//!
//! `fixtures/attach_chain.ttf` (built by
//! `tests/tools/build_attach_chain_fixture.py`) has a mark-to-mark
//! lookup that runs before the mark-to-base one, a cursive lookup with
//! the RightToLeft flag, mark-to-base, mark-to-ligature and
//! mark-to-mark lookups, and then a `blwm` lookup that moves the bases.
//! Every expectation is HarfBuzz 14.5.0's output (through uharfbuzz
//! 0.56.2) for the same text and direction.

use std::time::{Duration, Instant};

use sigilbuzz::{shape, Blob, Buffer, ClusterLevel, Direction, Face, Feature, Font};

const FONT: &[u8] = include_bytes!("fixtures/attach_chain.ttf");

/// One glyph: id, cluster, x and y advance, x and y offset.
type Out = (u32, u32, i32, i32, i32, i32);

fn shaped(text: &str, direction: Direction, features: &[Feature]) -> Vec<Out> {
    let blob = Blob::new(FONT);
    let face = Face::parse(&blob, 0).expect("face");
    let font = Font::new(face, 1000.0);
    let mut buffer = Buffer::new();
    buffer.push_str(text);
    buffer.set_direction(direction);
    buffer.set_cluster_level(ClusterLevel::MonotoneGraphemes);
    let run = shape(&font, &buffer, features).expect("shape");
    run.glyphs
        .iter()
        .map(|g| {
            (
                g.glyph_id,
                g.cluster,
                g.x_advance,
                g.y_advance,
                g.x_offset,
                g.y_offset,
            )
        })
        .collect()
}

fn ltr(text: &str) -> Vec<Out> {
    shaped(text, Direction::Ltr, &[])
}

#[test]
fn a_mark_keeps_its_height_when_a_later_lookup_raises_its_base() {
    // `blwm` moves `a` by (20, 150) after U+0301 sits on it. The mark
    // follows the 20 along the line but stays at the anchor height.
    let expected = [(1, 0, 500, 0, 20, 150), (8, 0, 0, 0, -330, 600)];
    assert_eq!(ltr("a\u{0301}"), expected);
    assert_eq!(shaped("a\u{0301}", Direction::Rtl, &[]), expected);
    // U+0303 stacks on U+0301 and takes its height at that moment.
    assert_eq!(
        ltr("a\u{0301}\u{0303}"),
        [
            (1, 0, 500, 0, 20, 150),
            (8, 0, 0, 0, -330, 600),
            (10, 0, 0, 0, -330, 1250)
        ]
    );
}

#[test]
fn a_mark_stacked_before_its_parent_attaches_keeps_its_own_height() {
    // U+0300 stacks on U+0301 in a lookup that runs before U+0301 is
    // put on its base, so it never gets U+0301's 600.
    assert_eq!(
        ltr("a\u{0301}\u{0300}"),
        [
            (1, 0, 500, 0, 20, 150),
            (8, 0, 0, 0, -330, 600),
            (7, 0, 0, 0, -330, 700)
        ]
    );
}

#[test]
fn a_mark_on_a_cursive_glyph_takes_the_chain_height_at_attachment() {
    // Each `b` hangs 100 below the next one. A mark on the first `b`
    // adds the heights of the whole chain at the time it attaches, and
    // the later `blwm` shift of 50 per `b` does not reach it.
    assert_eq!(
        ltr("b\u{0301}bb"),
        [
            (2, 0, 500, 0, 0, -50),
            (8, 0, 0, 0, -350, 400),
            (2, 3, 500, 0, 0, 0),
            (2, 4, 600, 0, 0, 50)
        ]
    );
    assert_eq!(
        ltr("bbb\u{0301}"),
        [
            (2, 0, 500, 0, 0, -50),
            (2, 1, 500, 0, 0, 0),
            (2, 2, 600, 0, 0, 50),
            (8, 2, 0, 0, -450, 600)
        ]
    );
    assert_eq!(
        shaped("bb\u{0301}b", Direction::Rtl, &[]),
        [
            (2, 4, 500, 0, 0, -50),
            (2, 1, 500, 0, 0, 0),
            (8, 1, 0, 0, -350, 500),
            (2, 0, 600, 0, 0, 50)
        ]
    );
}

#[test]
fn a_mark_on_a_ligature_keeps_its_height_when_the_ligature_moves() {
    // `f_i` rises by 90 after U+0301 sits on its second component.
    assert_eq!(
        ltr("fi\u{0301}\u{0303}"),
        [
            (6, 0, 550, 0, 0, 90),
            (8, 0, 0, 0, -200, 650),
            (10, 0, 0, 0, -200, 1300)
        ]
    );
}

#[test]
fn vertical_marks_keep_their_cross_offset_and_follow_along_the_column() {
    // In a vertical run x is the cross-stream axis. `blwm` moves `v`
    // by (40, -70): the marks keep the x they attached with and follow
    // the -70 down the column.
    assert_eq!(
        shaped("v\u{0301}\u{0303}", Direction::Ttb, &[]),
        [
            (5, 0, 0, -1000, -235, -820),
            (8, 0, 0, 0, -125, 780),
            (10, 0, 0, 0, -125, 1430)
        ]
    );
    // With `curs` on, the `b` chain runs across the column, and the
    // mark on the first `b` takes the chain's x when it attaches.
    let curs = [Feature {
        tag: *b"curs",
        value: 1,
    }];
    assert_eq!(
        shaped("b\u{0301}bb", Direction::Ttb, &curs),
        [
            (2, 0, 0, -650, -1300, -700),
            (8, 0, 0, 0, -1150, 550),
            (2, 3, 0, 100, -800, 50),
            (2, 4, 0, -250, -300, 50)
        ]
    );
}

#[test]
fn one_walk_resolves_at_most_sixty_four_links() {
    // A hundred `b` in left-to-right text make one chain from the first
    // to the last. HarfBuzz resolves glyphs from the start of the run,
    // each walk following at most 64 links, so glyph 64 keeps its own
    // offset and the walk from glyph 65 starts over.
    let text = "b".repeat(100);
    let y: Vec<i32> = ltr(&text).iter().map(|g| g.5).collect();
    assert_eq!(y.len(), 100);
    assert_eq!(
        [y[0], y[1], y[63], y[64], y[65], y[98], y[99]],
        [-3250, -3200, -100, -50, -1650, 0, 50]
    );
    // Backward runs resolve from the end of the run, one link per walk.
    let y: Vec<i32> = shaped(&text, Direction::Rtl, &[])
        .iter()
        .map(|g| g.5)
        .collect();
    assert_eq!([y[0], y[1], y[98], y[99]], [-3250, -3200, 0, 50]);
}

#[test]
fn marks_on_a_long_cursive_chain_attach_in_linear_time() {
    // Every mark on a `b` walks the cursive chain up to its root, which
    // a long chain of marked glyphs turns into quadratic work. The walks
    // share a budget proportional to the run.
    const N: usize = 40_000;
    let time = |text: &str| {
        let start = Instant::now();
        let glyphs = ltr(text).len();
        (glyphs, start.elapsed())
    };
    let (glyphs, chained) = time(&"b\u{0301}".repeat(N));
    assert_eq!(glyphs, 2 * N);
    let (_, plain) = time(&"a\u{0301}".repeat(N));
    let budget = plain * 20 + Duration::from_secs(2);
    assert!(
        chained < budget,
        "{chained:?} for {N} marks, budget {budget:?}"
    );
}
