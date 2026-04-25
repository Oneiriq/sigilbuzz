//! Subcommand implementations.
//!
//! Each subcommand owns its own file and exports a `run(args) -> Result`
//! entry point. The dispatcher in `main.rs` is a flat match over the
//! `Cmd` enum.

pub mod info;
pub mod paint;
pub mod pdf;
pub mod shape;
pub mod slug;
pub mod subset;
pub mod svg;
pub mod woff;

mod util;
