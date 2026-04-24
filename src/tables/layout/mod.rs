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
pub mod device;
pub mod feature_list;
pub mod lookup_list;
pub mod script_list;
pub mod skip_iter;
pub mod state_table;

pub use class_def::ClassDef;
pub use context::{
    ChainClassRule2, ChainClassSet2, ChainContext1, ChainContext2, ChainContext3, ChainRule1,
    ChainRuleSet1, ClassRule2, ClassSet2, Context1, Context2, Context3, Rule1, RuleSet1,
    SequenceLookupRecord,
};
pub use coverage::Coverage;
pub use device::{DeviceOrVariationIndex, VARIATION_INDEX_DELTA_FORMAT};
pub use feature_list::{Feature, FeatureList};
pub use lookup_list::Lookup;
pub use lookup_list::LookupList;
pub use script_list::{LangSys, Script, ScriptList};
pub use skip_iter::{
    MatchFilter, SkipIter, LOOKUP_FLAG_IGNORE_BASE_GLYPHS, LOOKUP_FLAG_IGNORE_LIGATURES,
    LOOKUP_FLAG_IGNORE_MARKS, LOOKUP_FLAG_MARK_ATTACHMENT_TYPE_MASK, LOOKUP_FLAG_RIGHT_TO_LEFT,
    LOOKUP_FLAG_USE_MARK_FILTERING_SET,
};
