//! `morx`: Apple Extended Glyph Metamorphosis.
//!
//! `morx` is Apple's successor to `mort`. AAT-only fonts (legacy
//! macOS Zapfino, older Apple Chancery variants, and most third-party
//! AAT designs) ship substitution logic here instead of in GSUB.
//! sigilbuzz consults `morx` only when the font has no GSUB, so
//! modern OpenType fonts are unaffected. This mirrors HarfBuzz's
//! AAT shaper policy.
//!
//! # Layout
//!
//! ```text
//!   u16 version       (2: without subtable coverage;
//!                      3: with u32 subtable coverage)
//!   u16 _pad
//!   u32 nChains
//!   Chain chains[nChains]
//!
//!   Chain:
//!     u32 defaultFlags
//!     u32 chainLength   (bytes, incl. this header)
//!     u32 featureCount  (feature selectors: sigilbuzz ignores these
//!                        and applies only the defaults)
//!     u32 subtableCount
//!     Feature features[featureCount]
//!     Subtable subtables[subtableCount]
//!
//!   Subtable header:
//!     u32 length        (bytes, incl. this header)
//!     u32 coverage      (low byte = type; high bits = flags)
//!     u32 subFeatureFlags
//!     Body body         (type-specific)
//! ```
//!
//! Subtables run sequentially, each one reading (and possibly
//! mutating) the glyph stream produced by the previous subtable.
//! sigilbuzz implements five subtable types:
//!
//! - **Type 0**: Rearrangement. Stateless over classes but stateful
//!   over a pending "marked glyph range"; used for Indic vowel
//!   reordering in AAT-only Indic fonts.
//! - **Type 1**: Contextual Glyph Substitution. Each state-entry
//!   carries two indexes into a substitution lookup; classic home
//!   of Apple Chancery's contextual swashes.
//! - **Type 2**: Ligature Substitution. State machine walks the
//!   input, stacking pending glyphs; on an accept entry it looks up
//!   a ligature action array to emit a single replacement.
//! - **Type 4**: Non-Contextual Substitution. The simplest morx
//!   subtable type: just an AAT lookup table giving a gid -> gid
//!   replacement applied unconditionally to every glyph in the run.
//! - **Type 5**: Insertion. State machine that inserts up to five
//!   glyphs before / after the current position based on context.
//!   Used by Apple Chancery to inject decorative glyphs and by
//!   Hebrew / Arabic AAT fonts for cantillation marks.
//!
//! # Feature selector handling
//!
//! AAT chains parameterize subtables by 16-bit feature selectors
//! (e.g. "contextual alternates on/off"). sigilbuzz applies only
//! the chain's `defaultFlags`; a subtable's `subFeatureFlags`
//! decides participation by ANDing with the default flags, and
//! zero-flag subtables are skipped. This matches HarfBuzz's default
//! policy and is what Apple's AAT renderers do when the caller
//! supplies no per-feature overrides.

mod contextual;
mod insertion;
mod ligature;
mod non_contextual;
mod rearrangement;

use alloc::vec::Vec;

use crate::error::{Error, Result};
use crate::tables::layout::state_table::{
    StateTableHeader, CLASS_DELETED_GLYPH, CLASS_END_OF_TEXT,
};
use crate::tables::parse::Reader;

use contextual::apply_contextual;
use insertion::apply_insertion;
use ligature::apply_ligature;
use non_contextual::apply_non_contextual;
use rearrangement::apply_rearrangement;

/// Subtable-type numeric code pulled from the low byte of `coverage`.
const TYPE_REARRANGEMENT: u8 = 0;
const TYPE_CONTEXTUAL: u8 = 1;
const TYPE_LIGATURE: u8 = 2;
const TYPE_NON_CONTEXTUAL: u8 = 4;
const TYPE_INSERTION: u8 = 5;

/// Chain header: defaultFlags, chainLength, featureCount, subtableCount.
const CHAIN_HEADER_LEN: usize = 16;
/// Subtable header: length, coverage, subFeatureFlags.
const SUBTABLE_HEADER_LEN: usize = 12;

/// Insertion subtables stop inserting once the run would grow past
/// this multiple of the input length. Each insertion subtable can
/// multiply the run length, so without a cap a chain of them grows
/// the run exponentially.
const MAX_LEN_FACTOR: usize = 8;
/// Floor for the run-length cap, so short runs can still take
/// several full-size insertions.
const MAX_LEN_MIN: usize = 1024;

/// Upper bound on state-machine steps for a run of `len` glyphs.
/// DontAdvance entries revisit a glyph, and a malformed table can
/// keep doing that forever.
fn max_steps(len: usize) -> usize {
    len.saturating_mul(8).saturating_add(16)
}

/// Parsed `morx` table: owns pointers into the source bytes.
#[derive(Debug, Clone)]
pub struct Morx<'a> {
    version: u16,
    chains: Vec<Chain<'a>>,
}

/// One chain of subtables in a `morx` table.
#[derive(Debug, Clone)]
pub struct Chain<'a> {
    default_flags: u32,
    subtables: Vec<Subtable<'a>>,
}

/// A single `morx` subtable: a state-machine-driven transform over
/// the glyph stream.
#[derive(Debug, Clone)]
pub struct Subtable<'a> {
    /// Subtable `subFeatureFlags`. A subtable participates when
    /// `subFeatureFlags & chain.defaultFlags != 0`.
    sub_feature_flags: u32,
    /// Parsed body, or `None` for subtable types sigilbuzz does not
    /// implement. [`Morx::apply`] skips those silently.
    body: Option<SubtableBody<'a>>,
}

#[derive(Debug, Clone)]
enum SubtableBody<'a> {
    /// Type 0: Rearrangement. Body is a plain state table header
    /// followed by its backing arrays. Entries are 4 bytes
    /// (newState, flags). The 16-bit flags encode the verb in their
    /// high four bits and the mark/advance flags in the low bits.
    Rearrangement(StateTableHeader<'a>),
    /// Type 1: Contextual substitution. State-table entries are
    /// 8 bytes: (newState, flags, markIndex, currentIndex). Marks
    /// and current-indices are into a parallel "substitution lookup
    /// table": an array of AAT lookups, one per index.
    Contextual {
        state: StateTableHeader<'a>,
        substitutions: &'a [u8],
    },
    /// Type 2: Ligature substitution. State-table entries are
    /// 6 bytes: (newState, flags, actionIndex). Actions reference
    /// a three-array group: ligAction (u32), component (u16),
    /// ligature (u16).
    Ligature {
        state: StateTableHeader<'a>,
        lig_actions: &'a [u8],
        components: &'a [u8],
        ligatures: &'a [u8],
    },
    /// Type 4: Non-contextual substitution. The body is one AAT
    /// lookup table mapping every input glyph id directly to its
    /// replacement; the "no rule" sentinel falls back to the input.
    NonContextual { lookup: &'a [u8] },
    /// Type 5: Insertion. The state machine walks the input; on an
    /// insertion entry, it splices `currentInsertCount` glyphs from
    /// `currentInsertList` before / after the current glyph, and
    /// `markedInsertCount` glyphs at the most recent mark. Inserted
    /// glyphs are u16 ids drawn from the `insertion glyph table`, a
    /// flat u16 array indexed by the entry's u16 list offsets.
    Insertion {
        state: StateTableHeader<'a>,
        insertion_table: &'a [u8],
    },
}

// --- Type 0 flags ---
// bit 15: MarkFirst:   remember the current position as "first"
// bit 14: DontAdvance: stay on the same glyph
// bit 13: MarkLast:    remember the current position as "last"
// bits 12-8 reserved
// bits 7-0 verb (0..15)
const FLAG_MARK_FIRST: u16 = 1 << 15;
const FLAG_DONT_ADVANCE: u16 = 1 << 14;
const FLAG_MARK_LAST: u16 = 1 << 13;
const FLAG_REARRANGE_VERB_MASK: u16 = 0x000F;

// --- Type 1 flags ---
// bit 15: SetMark: record current position as the "mark"
const FLAG_CTX_SET_MARK: u16 = 1 << 15;
// bit 14: DontAdvance reused

// --- Type 2 flags ---
// bit 15: SetComponent: push current glyph onto the component stack
const FLAG_LIG_SET_COMPONENT: u16 = 1 << 15;
// bit 14: DontAdvance reused
// bit 13: PerformAction: run the ligature action referenced by the entry
const FLAG_LIG_PERFORM_ACTION: u16 = 1 << 13;

// --- Type 5 flags ---
// bit 15: SetMark: record the current position as the mark
const FLAG_INS_SET_MARK: u16 = 1 << 15;
// bit 14: DontAdvance reused
// bit 13: CurrentIsKashidaLike: kashida hint, ignored for shaping correctness
// bit 12: MarkedIsKashidaLike:  kashida hint, ignored
// bit 11: CurrentInsertBefore:  insert relative to the current glyph (0 = after, 1 = before)
// bit 10: MarkedInsertBefore:   insert relative to the marked glyph
// bits 5-9: currentInsertCount (5 bits -> max 31)
// bits 0-4: markedInsertCount  (5 bits -> max 31)
const FLAG_INS_CURRENT_BEFORE: u16 = 1 << 11;
const FLAG_INS_MARKED_BEFORE: u16 = 1 << 10;
const FLAG_INS_CURRENT_COUNT_MASK: u16 = 0x03E0;
const FLAG_INS_CURRENT_COUNT_SHIFT: u32 = 5;
const FLAG_INS_MARKED_COUNT_MASK: u16 = 0x001F;

// Ligature action flags in the u32 action word.
const LIG_ACTION_LAST: u32 = 1 << 31;
const LIG_ACTION_STORE: u32 = 1 << 30;
const LIG_ACTION_OFFSET_SIGN: u32 = 1 << 29;
const LIG_ACTION_OFFSET_MASK: u32 = 0x3FFF_FFFF;

impl<'a> Morx<'a> {
    /// Parses a `morx` table. The data slice starts at the table's
    /// first byte (`version`).
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let version = r.read_u16()?;
        if version != 2 && version != 3 {
            return Err(Error::Unsupported {
                context: "morx version outside {2, 3}",
            });
        }
        let _pad = r.read_u16()?;
        let n_chains = r.read_u32()?;

        // Every chain needs at least its header, so the remaining bytes
        // bound how many chains can exist. Reserve no more than that.
        let mut chains =
            Vec::with_capacity((n_chains as usize).min(r.remaining() / CHAIN_HEADER_LEN));
        for _ in 0..n_chains {
            let chain_start = r.position();
            if chain_start + CHAIN_HEADER_LEN > data.len() {
                return Err(Error::Truncated {
                    offset: chain_start,
                    context: "morx chain header",
                });
            }
            let default_flags = r.read_u32()?;
            let chain_length = r.read_u32()? as usize;
            let feature_count = r.read_u32()?;
            let subtable_count = r.read_u32()?;

            let chain_end = chain_start
                .checked_add(chain_length)
                .ok_or(Error::Malformed {
                    offset: chain_start,
                    context: "morx chain length overflow",
                })?;
            if chain_end > data.len() {
                return Err(Error::Truncated {
                    offset: chain_end,
                    context: "morx chain extends past table",
                });
            }
            // Skip feature array: 12 bytes per feature (u16 featureType,
            // u16 featureSetting, u32 enableFlags, u32 disableFlags).
            r.skip((feature_count as usize).saturating_mul(12))?;

            // Same bound as for chains: each subtable needs its header.
            let room = chain_end.saturating_sub(r.position()) / SUBTABLE_HEADER_LEN;
            let mut subtables = Vec::with_capacity((subtable_count as usize).min(room));
            for _ in 0..subtable_count {
                let sub_start = r.position();
                if sub_start + SUBTABLE_HEADER_LEN > chain_end {
                    return Err(Error::Truncated {
                        offset: sub_start,
                        context: "morx subtable header",
                    });
                }
                let sub_length = r.read_u32()? as usize;
                let coverage = r.read_u32()?;
                let sub_feature_flags = r.read_u32()?;

                let sub_end = sub_start.checked_add(sub_length).ok_or(Error::Malformed {
                    offset: sub_start,
                    context: "morx subtable length overflow",
                })?;
                if sub_end > chain_end {
                    return Err(Error::Truncated {
                        offset: sub_end,
                        context: "morx subtable extends past chain",
                    });
                }
                // Subtable body starts right after the 12-byte header.
                let body_off = sub_start + SUBTABLE_HEADER_LEN;
                // A declared length shorter than the header leaves no
                // body. Drop the subtable and continue after its header,
                // so the cursor always moves forward.
                let Some(body_bytes) = data.get(body_off..sub_end) else {
                    r.seek(body_off)?;
                    continue;
                };
                let sub_type = (coverage & 0xFF) as u8;
                let body = parse_subtable_body(sub_type, body_bytes)?;
                subtables.push(Subtable {
                    sub_feature_flags,
                    body,
                });
                r.seek(sub_end)?;
            }
            chains.push(Chain {
                default_flags,
                subtables,
            });
            // A chain length shorter than the header would send the
            // cursor back to this chain's start and read it again.
            // Continue after the header instead.
            r.seek(chain_end.max(chain_start + CHAIN_HEADER_LEN))?;
        }

        Ok(Self { version, chains })
    }

    /// Reported version word (2 or 3).
    #[must_use]
    pub const fn version(&self) -> u16 {
        self.version
    }

    /// Parsed chains. Callers iterate in order so output of the N-th
    /// chain flows into the N+1-th.
    #[must_use]
    pub fn chains(&self) -> &[Chain<'a>] {
        &self.chains
    }

    /// Applies every chain's default-flag subtables over the input
    /// glyph id stream, in sequence. Returns a mapping from the
    /// output glyph index back to the originating input index, so
    /// callers can carry cluster / unicode-props metadata across
    /// ligation without the morx module having to know about
    /// `Glyph`. A mapping entry of `usize::MAX` marks a glyph that
    /// was synthesized without a single originating input (used for
    /// ligatures: the ligature inherits the lowest-index parent's
    /// cluster via `merge_runs`).
    ///
    /// The returned vector is the new glyph id stream; it is always
    /// the same length as the mapping vector.
    ///
    /// Insertion subtables stop inserting once the run would exceed
    /// eight times the input length (at least 1024 glyphs), and each
    /// subtable walk stops after eight state-machine steps per glyph.
    #[must_use]
    pub fn apply(&self, input: &[u16]) -> (Vec<u16>, Vec<usize>) {
        let mut glyphs: Vec<u16> = input.to_vec();
        let mut origins: Vec<usize> = (0..input.len()).collect();
        let max_len = input.len().saturating_mul(MAX_LEN_FACTOR).max(MAX_LEN_MIN);
        for chain in &self.chains {
            for subtable in &chain.subtables {
                if subtable.sub_feature_flags & chain.default_flags == 0 {
                    continue;
                }
                let Some(ref body) = subtable.body else {
                    continue;
                };
                apply_subtable(body, &mut glyphs, &mut origins, max_len);
            }
        }
        (glyphs, origins)
    }
}

fn parse_subtable_body(sub_type: u8, bytes: &[u8]) -> Result<Option<SubtableBody<'_>>> {
    match sub_type {
        TYPE_REARRANGEMENT => {
            let state = StateTableHeader::parse(bytes)?;
            Ok(Some(SubtableBody::Rearrangement(state)))
        }
        TYPE_CONTEXTUAL => {
            // Contextual subtable extends the state-table header with
            // one extra offset: substitutionTable.
            if bytes.len() < StateTableHeader::SIZE + 4 {
                return Err(Error::Truncated {
                    offset: bytes.len(),
                    context: "morx type 1 header",
                });
            }
            let state = StateTableHeader::parse(bytes)?;
            let subs_off = u32::from_be_bytes([
                bytes[StateTableHeader::SIZE],
                bytes[StateTableHeader::SIZE + 1],
                bytes[StateTableHeader::SIZE + 2],
                bytes[StateTableHeader::SIZE + 3],
            ]) as usize;
            let substitutions = bytes.get(subs_off..).ok_or(Error::Truncated {
                offset: subs_off,
                context: "morx contextual substitution table",
            })?;
            Ok(Some(SubtableBody::Contextual {
                state,
                substitutions,
            }))
        }
        TYPE_LIGATURE => parse_ligature_body(bytes),
        TYPE_NON_CONTEXTUAL => {
            // The whole body IS the AAT lookup table: no extra
            // header, no offsets. We hand the slice straight to
            // [`lookup_value`] at apply time.
            Ok(Some(SubtableBody::NonContextual { lookup: bytes }))
        }
        TYPE_INSERTION => {
            // Insertion subtable extends the state-table header with
            // one extra offset: insertionGlyphTable.
            if bytes.len() < StateTableHeader::SIZE + 4 {
                return Err(Error::Truncated {
                    offset: bytes.len(),
                    context: "morx type 5 header",
                });
            }
            let state = StateTableHeader::parse(bytes)?;
            let ins_off = u32::from_be_bytes([
                bytes[StateTableHeader::SIZE],
                bytes[StateTableHeader::SIZE + 1],
                bytes[StateTableHeader::SIZE + 2],
                bytes[StateTableHeader::SIZE + 3],
            ]) as usize;
            let insertion_table = bytes.get(ins_off..).ok_or(Error::Truncated {
                offset: ins_off,
                context: "morx insertion glyph table",
            })?;
            Ok(Some(SubtableBody::Insertion {
                state,
                insertion_table,
            }))
        }
        // Other subtable types are not implemented and are skipped.
        _ => Ok(None),
    }
}

fn parse_ligature_body(bytes: &[u8]) -> Result<Option<SubtableBody<'_>>> {
    // Ligature subtable extends the header with three offsets.
    if bytes.len() < StateTableHeader::SIZE + 12 {
        return Err(Error::Truncated {
            offset: bytes.len(),
            context: "morx type 2 header",
        });
    }
    let state = StateTableHeader::parse(bytes)?;
    let base = StateTableHeader::SIZE;
    let lig_action_off = u32::from_be_bytes([
        bytes[base],
        bytes[base + 1],
        bytes[base + 2],
        bytes[base + 3],
    ]) as usize;
    let component_off = u32::from_be_bytes([
        bytes[base + 4],
        bytes[base + 5],
        bytes[base + 6],
        bytes[base + 7],
    ]) as usize;
    let ligature_off = u32::from_be_bytes([
        bytes[base + 8],
        bytes[base + 9],
        bytes[base + 10],
        bytes[base + 11],
    ]) as usize;
    let lig_actions = bytes.get(lig_action_off..).ok_or(Error::Truncated {
        offset: lig_action_off,
        context: "morx ligature action table",
    })?;
    let components = bytes.get(component_off..).ok_or(Error::Truncated {
        offset: component_off,
        context: "morx ligature component table",
    })?;
    let ligatures = bytes.get(ligature_off..).ok_or(Error::Truncated {
        offset: ligature_off,
        context: "morx ligature list",
    })?;
    Ok(Some(SubtableBody::Ligature {
        state,
        lig_actions,
        components,
        ligatures,
    }))
}

fn apply_subtable(
    body: &SubtableBody<'_>,
    glyphs: &mut Vec<u16>,
    origins: &mut Vec<usize>,
    max_len: usize,
) {
    match body {
        SubtableBody::Rearrangement(state) => apply_rearrangement(state, glyphs, origins),
        SubtableBody::Contextual {
            state,
            substitutions,
        } => apply_contextual(state, substitutions, glyphs),
        SubtableBody::Ligature {
            state,
            lig_actions,
            components,
            ligatures,
        } => apply_ligature(state, lig_actions, components, ligatures, glyphs, origins),
        SubtableBody::NonContextual { lookup } => apply_non_contextual(lookup, glyphs),
        SubtableBody::Insertion {
            state,
            insertion_table,
        } => apply_insertion(state, insertion_table, glyphs, origins, max_len),
    }
}

fn class_for(state: &StateTableHeader<'_>, glyph: Option<u16>) -> Result<u16> {
    match glyph {
        None => Ok(CLASS_END_OF_TEXT),
        Some(0xFFFF) => Ok(CLASS_DELETED_GLYPH),
        Some(g) => state.class_of(g),
    }
}

#[cfg(test)]
mod tests;
