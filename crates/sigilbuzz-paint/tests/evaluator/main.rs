//! Integration tests for the COLRv1 paint evaluator.
//!
//! These build minimal COLR + CPAL byte blobs in-process, plug them
//! into a synthetic `Face` via the SFNT directory layout sigilbuzz
//! exposes, and assert the [`DrawCmd`](sigilbuzz_paint::DrawCmd) stream the evaluator emits.
//!
//! The fixture style mirrors the hand-authored COLR tests in
//! `src/tables/colr/tests.rs` so additions stay in sync with the parser
//! tests upstream.

mod fixtures;
mod gradients;
mod paint_graph;
mod transforms;
mod variations;
