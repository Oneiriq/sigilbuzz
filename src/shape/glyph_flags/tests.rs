//! Tests for the glyph flag primitives, against HarfBuzz's
//! `hb-buffer.hh`.

use super::*;
use alloc::vec::Vec;

fn run(clusters: &[u32]) -> Vec<Glyph> {
    clusters.iter().map(|&c| Glyph::new(1, c)).collect()
}

fn flags(glyphs: &[Glyph]) -> Vec<u32> {
    glyphs.iter().map(|g| g.flags.bits()).collect()
}

const MC: ClusterLevel = ClusterLevel::MonotoneCharacters;

#[test]
fn unsafe_to_break_skips_the_smallest_cluster() {
    let mut g = run(&[0, 1, 2]);
    unsafe_to_break(&mut g, 0, 3, MC);
    assert_eq!(flags(&g), [0, 3, 3]);
    // A range of one glyph marks nothing.
    let mut g = run(&[0, 1]);
    unsafe_to_break(&mut g, 1, 2, MC);
    assert_eq!(flags(&g), [0, 0]);
}

#[test]
fn monotone_levels_mark_only_past_the_edge_cluster() {
    // Smallest cluster at the start: from the end back to the first
    // glyph of that cluster.
    let mut g = run(&[4, 4, 5, 4]);
    unsafe_to_break(&mut g, 0, 4, MC);
    assert_eq!(flags(&g), [0, 0, 0, 0]);
    let mut g = run(&[4, 5, 6]);
    unsafe_to_break(&mut g, 0, 3, MC);
    assert_eq!(flags(&g), [0, 3, 3]);
    // Smallest at the end (reversed clusters): up to its run.
    let mut g = run(&[6, 5, 4]);
    unsafe_to_break(&mut g, 0, 3, MC);
    assert_eq!(flags(&g), [3, 3, 0]);
    // At the character level every glyph off the smallest cluster.
    let mut g = run(&[4, 5, 4]);
    unsafe_to_break(&mut g, 0, 3, ClusterLevel::Characters);
    assert_eq!(flags(&g), [0, 3, 0]);
}

#[test]
fn ranges_past_255_glyphs_or_backward_are_ignored() {
    let mut g = run(&(0..300).collect::<Vec<u32>>());
    unsafe_to_break(&mut g, 0, 257, MC);
    unsafe_to_concat(&mut g, 5, 2);
    assert!(g.iter().all(|g| g.flags.is_empty()));
    unsafe_to_concat(&mut g, 10, 12);
    assert_eq!(flags(&g[9..13]), [0, 2, 2, 0]);
}

#[test]
fn propagation_shares_flags_across_a_cluster() {
    let mut g = run(&[0, 0, 1, 2, 2]);
    g[1].flags = BREAK;
    g[3].flags = CONCAT;
    propagate(&mut g, BufferFlags::DEFAULT);
    assert_eq!(flags(&g), [1, 1, 0, 0, 0]);
    let mut g = run(&[0, 0, 1, 2, 2]);
    g[1].flags = BREAK;
    g[3].flags = CONCAT;
    propagate(&mut g, BufferFlags::PRODUCE_UNSAFE_TO_CONCAT);
    assert_eq!(flags(&g), [3, 3, 0, 2, 2]);
}

#[test]
fn tatweel_flags_give_way_to_break_flags() {
    let tatweel = GlyphFlags::SAFE_TO_INSERT_TATWEEL;
    let mut g = run(&[0, 1, 1]);
    g[0].flags = tatweel;
    g[1].flags = tatweel;
    g[2].flags = BREAK;
    propagate(&mut g, BufferFlags::PRODUCE_SAFE_TO_INSERT_TATWEEL);
    assert_eq!(flags(&g), [5, 1, 1]);
}

#[test]
fn a_cluster_change_replaces_the_flags() {
    let mut g = Glyph::new(1, 3);
    g.flags = BREAK;
    set_cluster(&mut g, 3, GlyphFlags::empty());
    assert_eq!(g.flags, BREAK);
    set_cluster(&mut g, 2, CONCAT);
    assert_eq!((g.cluster, g.flags), (2, CONCAT));
}
