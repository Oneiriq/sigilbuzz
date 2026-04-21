//! OpenType Layout common primitives — the building blocks shared by
//! every GSUB and GPOS lookup.
//!
//! [`Coverage`] answers "is glyph X in this lookup's domain, and if
//! so at what index?". [`ClassDef`] answers "what class is glyph X
//! in?". Every non-trivial lookup in GSUB and GPOS is built on top
//! of one or both.

pub mod class_def;
pub mod coverage;

pub use class_def::ClassDef;
pub use coverage::Coverage;
