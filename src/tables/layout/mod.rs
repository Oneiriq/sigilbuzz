//! OpenType Layout common primitives — the building blocks shared by
//! every GSUB and GPOS lookup.
//!
//! [`Coverage`] answers "is glyph X in this lookup's domain, and if
//! so at what index?". [`ClassDef`] answers "what class is glyph X
//! in?". Every non-trivial lookup in GSUB and GPOS is built on top
//! of one or both.

pub mod class_def;
pub mod context;
pub mod coverage;
pub mod feature_list;
pub mod lookup_list;
pub mod script_list;

pub use class_def::ClassDef;
pub use context::{
    ChainClassRule2, ChainClassSet2, ChainContext1, ChainContext2, ChainContext3, ChainRule1,
    ChainRuleSet1, ClassRule2, ClassSet2, Context1, Context2, Context3, Rule1, RuleSet1,
    SequenceLookupRecord,
};
pub use coverage::Coverage;
pub use feature_list::{Feature, FeatureList};
pub use lookup_list::{Lookup, LookupList, LOOKUP_FLAG_USE_MARK_FILTERING_SET};
pub use script_list::{LangSys, Script, ScriptList};
