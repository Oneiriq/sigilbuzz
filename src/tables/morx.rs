//! `morx` — Apple Extended Glyph Metamorphosis.
//!
//! `morx` is Apple's successor to `mort`. AAT-only fonts (legacy
//! macOS Zapfino, older Apple Chancery variants, and most third-party
//! AAT designs) ship substitution logic here instead of in GSUB.
//! sigilbuzz consults `morx` only when the font has no GSUB, so
//! modern OpenType fonts are unaffected — this mirrors HarfBuzz's
//! AAT shaper policy.
//!
//! # Layout
//!
//! ```text
//!   u16 version       (2 — without subtable coverage;
//!                      3 — with u32 subtable coverage)
//!   u16 _pad
//!   u32 nChains
//!   Chain chains[nChains]
//!
//!   Chain:
//!     u32 defaultFlags
//!     u32 chainLength   (bytes, incl. this header)
//!     u32 featureCount  (feature selectors — sigilbuzz ignores these
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
//! sigilbuzz implements the three most common types:
//!
//! - **Type 0** — Rearrangement. Stateless over classes but stateful
//!   over a pending "marked glyph range"; used for Indic vowel
//!   reordering in AAT-only Indic fonts.
//! - **Type 1** — Contextual Glyph Substitution. Each state-entry
//!   carries two indexes into a substitution lookup; classic home
//!   of Apple Chancery's contextual swashes.
//! - **Type 2** — Ligature Substitution. State machine walks the
//!   input, stacking pending glyphs; on an accept entry it looks up
//!   a ligature action array to emit a single replacement.
//! - **Type 4** — Non-Contextual Substitution. The simplest morx
//!   subtable type: just an AAT lookup table giving a gid → gid
//!   replacement applied unconditionally to every glyph in the run.
//! - **Type 5** — Insertion. State machine that inserts up to five
//!   glyphs before / after the current position based on context.
//!   Used by Apple Chancery to inject decorative glyphs and by
//!   Hebrew / Arabic AAT fonts for cantillation marks.
//!
//! # Feature selector handling
//!
//! AAT chains parameterise subtables by 16-bit feature selectors
//! (e.g. "contextual alternates on/off"). sigilbuzz applies only
//! the chain's `defaultFlags`; a subtable's `subFeatureFlags`
//! decides participation by ANDing with the default flags, and
//! zero-flag subtables are skipped. This matches HarfBuzz's default
//! policy and is what Apple's AAT renderers do when the caller
//! supplies no per-feature overrides.

use alloc::vec::Vec;

use crate::error::{Error, Result};
use crate::tables::layout::state_table::{
    StateTableHeader, CLASS_DELETED_GLYPH, CLASS_END_OF_TEXT, CLASS_OUT_OF_BOUNDS,
};
use crate::tables::parse::Reader;

/// Subtable-type numeric code pulled from the low byte of `coverage`.
const TYPE_REARRANGEMENT: u8 = 0;
const TYPE_CONTEXTUAL: u8 = 1;
const TYPE_LIGATURE: u8 = 2;
const TYPE_NON_CONTEXTUAL: u8 = 4;
const TYPE_INSERTION: u8 = 5;

/// Parsed `morx` table — owns pointers into the source bytes.
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

/// A single `morx` subtable — a state-machine-driven transform over
/// the glyph stream.
#[derive(Debug, Clone)]
pub struct Subtable<'a> {
    /// Subtable `subFeatureFlags`. A subtable participates when
    /// `subFeatureFlags & chain.defaultFlags != 0`.
    sub_feature_flags: u32,
    /// Parsed body, or `None` for subtable types we recognise but do
    /// not yet implement (e.g. type 4 non-contextual, type 5
    /// insertion). The `morx` iterator skips those silently.
    body: Option<SubtableBody<'a>>,
}

#[derive(Debug, Clone)]
enum SubtableBody<'a> {
    /// Type 0 — Rearrangement. Body is a plain state table header
    /// followed by its backing arrays. Entries are 4 bytes
    /// (newState, flags) — the 16-bit flags encode the verb in their
    /// high four bits and the mark/advance flags in the low bits.
    Rearrangement(StateTableHeader<'a>),
    /// Type 1 — Contextual substitution. State-table entries are
    /// 8 bytes: (newState, flags, markIndex, currentIndex). Marks
    /// and current-indices are into a parallel "substitution lookup
    /// table" — an array of AAT lookups, one per index.
    Contextual {
        state: StateTableHeader<'a>,
        substitutions: &'a [u8],
    },
    /// Type 2 — Ligature substitution. State-table entries are
    /// 6 bytes: (newState, flags, actionIndex). Actions reference
    /// a three-array group: ligAction (u32), component (u16),
    /// ligature (u16).
    Ligature {
        state: StateTableHeader<'a>,
        lig_actions: &'a [u8],
        components: &'a [u8],
        ligatures: &'a [u8],
    },
    /// Type 4 — Non-contextual substitution. The body is one AAT
    /// lookup table mapping every input glyph id directly to its
    /// replacement; the "no rule" sentinel falls back to the input.
    NonContextual { lookup: &'a [u8] },
    /// Type 5 — Insertion. The state machine walks the input; on an
    /// insertion entry, it splices `currentInsertCount` glyphs from
    /// `currentInsertList` before / after the current glyph, and
    /// `markedInsertCount` glyphs at the most recent mark. Inserted
    /// glyphs are u16 ids drawn from the `insertion glyph table` — a
    /// flat u16 array indexed by the entry's u16 list offsets.
    Insertion {
        state: StateTableHeader<'a>,
        insertion_table: &'a [u8],
    },
}

// --- Type 0 flags ---
// bit 15: MarkFirst  — remember the current position as "first"
// bit 14: DontAdvance — stay on the same glyph
// bit 13: MarkLast   — remember the current position as "last"
// bits 12-8 reserved
// bits 7-0 verb (0..15)
const FLAG_MARK_FIRST: u16 = 1 << 15;
const FLAG_DONT_ADVANCE: u16 = 1 << 14;
const FLAG_MARK_LAST: u16 = 1 << 13;
const FLAG_REARRANGE_VERB_MASK: u16 = 0x000F;

// --- Type 1 flags ---
// bit 15: SetMark   — record current position as the "mark"
const FLAG_CTX_SET_MARK: u16 = 1 << 15;
// bit 14: DontAdvance reused

// --- Type 2 flags ---
// bit 15: SetComponent — push current glyph onto the component stack
const FLAG_LIG_SET_COMPONENT: u16 = 1 << 15;
// bit 14: DontAdvance reused
// bit 13: PerformAction — run the ligature action referenced by the entry
const FLAG_LIG_PERFORM_ACTION: u16 = 1 << 13;

// --- Type 5 flags ---
// bit 15: SetMark — record the current position as the mark
const FLAG_INS_SET_MARK: u16 = 1 << 15;
// bit 14: DontAdvance reused
// bit 13: CurrentIsKashidaLike — kashida hint, ignored for shaping correctness
// bit 12: MarkedIsKashidaLike   — kashida hint, ignored
// bit 11: CurrentInsertBefore — insert relative to the current glyph: 0 = after, 1 = before
// bit 10: MarkedInsertBefore  — insert relative to the marked glyph
// bits 5-9: currentInsertCount (5 bits → max 31)
// bits 0-4: markedInsertCount  (5 bits → max 31)
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

        let mut chains = Vec::with_capacity(n_chains as usize);
        for _ in 0..n_chains {
            let chain_start = r.position();
            if chain_start + 16 > data.len() {
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
            r.skip(feature_count as usize * 12)?;

            let mut subtables = Vec::with_capacity(subtable_count as usize);
            for _ in 0..subtable_count {
                let sub_start = r.position();
                if sub_start + 12 > chain_end {
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
                let body_off = sub_start + 12;
                let body_bytes = &data[body_off..sub_end];
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
            r.seek(chain_end)?;
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
    /// was synthesised without a single originating input (used for
    /// ligatures — the ligature inherits the lowest-index parent's
    /// cluster via [`merge_runs`]).
    ///
    /// The returned vector is the new glyph id stream; it is always
    /// the same length as the mapping vector.
    #[must_use]
    pub fn apply(&self, input: &[u16]) -> (Vec<u16>, Vec<usize>) {
        let mut glyphs: Vec<u16> = input.to_vec();
        let mut origins: Vec<usize> = (0..input.len()).collect();
        for chain in &self.chains {
            for subtable in &chain.subtables {
                if subtable.sub_feature_flags & chain.default_flags == 0 {
                    continue;
                }
                let Some(ref body) = subtable.body else {
                    continue;
                };
                apply_subtable(body, &mut glyphs, &mut origins);
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
            // The whole body IS the AAT lookup table — no extra
            // header, no offsets. We hand the slice straight to
            // [`lookup_via_state_table`] at apply time.
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
        // Types 6+ remain deferred for now.
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
    let lig_action_off =
        u32::from_be_bytes([bytes[base], bytes[base + 1], bytes[base + 2], bytes[base + 3]])
            as usize;
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

fn apply_subtable(body: &SubtableBody<'_>, glyphs: &mut Vec<u16>, origins: &mut Vec<usize>) {
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
        } => apply_insertion(state, insertion_table, glyphs, origins),
    }
}

// --- Type 0: Rearrangement ---

fn apply_rearrangement(state: &StateTableHeader<'_>, glyphs: &mut [u16], origins: &mut [usize]) {
    let mut cur_state: u16 = 0;
    let mut i = 0;
    let mut first: Option<usize> = None;
    let mut last: Option<usize> = None;
    // Iterate through the run, with an extra end-of-text step so a
    // state carrying a pending mark gets one more chance to fire.
    while i <= glyphs.len() {
        let class = class_for(state, glyphs.get(i).copied()).unwrap_or(CLASS_OUT_OF_BOUNDS);
        let Ok(entry_idx) = state.entry_index(cur_state, class) else {
            return;
        };
        let Ok((new_state, flags)) = state.entry_prefix(entry_idx, 4) else {
            return;
        };
        if flags & FLAG_MARK_FIRST != 0 {
            first = Some(i);
        }
        if flags & FLAG_MARK_LAST != 0 {
            last = Some(i);
        }
        let verb = flags & FLAG_REARRANGE_VERB_MASK;
        if verb != 0 {
            if let (Some(a), Some(b)) = (first, last) {
                if a <= b && b < glyphs.len() {
                    rearrange(verb, glyphs, origins, a, b);
                }
            }
        }
        cur_state = new_state;
        if flags & FLAG_DONT_ADVANCE == 0 {
            i += 1;
        } else if i == glyphs.len() {
            // End-of-text + DontAdvance would loop forever; bail.
            return;
        }
    }
}

// Rearrangement verbs — standard AAT table of 16 permutations on a
// window described by (A = first, B = first+1, C?, D = last-1, E = last).
// Only the verbs sigilbuzz is likely to see (1 = "Ax → xA" and
// related swaps) are implemented; unknown verbs are a no-op so an
// unsupported rearrangement can't corrupt the glyph stream.
fn rearrange(verb: u16, glyphs: &mut [u16], origins: &mut [usize], first: usize, last: usize) {
    let len = last - first + 1;
    if len < 2 {
        return;
    }
    // Rearrangement verbs 1 / 2 / 3 all reduce to the same single
    // swap in sigilbuzz's two-element window coverage — a
    // conservative subset. Rarer verbs (4..=15) handle 3- to
    // 5-element windows and stay no-op until a real font needs them,
    // because producing a wrong permutation would corrupt the glyph
    // stream worse than leaving it alone.
    let _ = len;
    if let 1..=3 = verb {
        glyphs.swap(first, last);
        origins.swap(first, last);
    }
}

// --- Type 1: Contextual glyph substitution ---

fn apply_contextual(state: &StateTableHeader<'_>, substitutions: &[u8], glyphs: &mut [u16]) {
    const ENTRY_SIZE: usize = 8; // newState + flags + markIdx + currentIdx
    let mut cur_state: u16 = 0;
    let mut mark: Option<usize> = None;
    let mut i = 0;
    while i <= glyphs.len() {
        let class = class_for(state, glyphs.get(i).copied()).unwrap_or(CLASS_OUT_OF_BOUNDS);
        let Ok(entry_idx) = state.entry_index(cur_state, class) else {
            return;
        };
        let Ok((new_state, flags)) = state.entry_prefix(entry_idx, ENTRY_SIZE) else {
            return;
        };
        let mark_idx = state
            .entry_tail_u16(entry_idx, ENTRY_SIZE, 4)
            .unwrap_or(0xFFFF);
        let cur_idx = state
            .entry_tail_u16(entry_idx, ENTRY_SIZE, 6)
            .unwrap_or(0xFFFF);

        if mark_idx != 0xFFFF {
            if let Some(m) = mark {
                if m < glyphs.len() {
                    if let Some(replacement) = sub_lookup(substitutions, mark_idx, glyphs[m]) {
                        glyphs[m] = replacement;
                    }
                }
            }
        }
        if cur_idx != 0xFFFF && i < glyphs.len() {
            if let Some(replacement) = sub_lookup(substitutions, cur_idx, glyphs[i]) {
                glyphs[i] = replacement;
            }
        }

        if flags & FLAG_CTX_SET_MARK != 0 {
            mark = Some(i);
        }
        cur_state = new_state;
        if flags & FLAG_DONT_ADVANCE == 0 {
            i += 1;
        } else if i == glyphs.len() {
            return;
        }
    }
}

// Each "substitution lookup" referenced by index is itself an AAT
// lookup table; the substitutions blob is a sequence of such tables
// indexed by u32 offsets at its head.
//
// Layout: u16 lookupCount, then u32 offsets[lookupCount] pointing at
// the individual lookups relative to the substitutions blob.
//
// We wrap each lookup in the StateTableHeader's class-lookup helper
// by mapping glyph -> replacement-glyph-id directly.
fn sub_lookup(substitutions: &[u8], idx: u16, glyph: u16) -> Option<u16> {
    // The substitutions table is laid out as in the type-1 spec:
    // u16 nTables, u32 offsets[nTables] (relative to substitutions
    // blob start). A missing or malformed entry yields None.
    if substitutions.len() < 2 {
        return None;
    }
    let n_tables = u16::from_be_bytes([substitutions[0], substitutions[1]]);
    if idx >= n_tables {
        return None;
    }
    let off_base = 2 + idx as usize * 4;
    if substitutions.len() < off_base + 4 {
        return None;
    }
    let off = u32::from_be_bytes([
        substitutions[off_base],
        substitutions[off_base + 1],
        substitutions[off_base + 2],
        substitutions[off_base + 3],
    ]) as usize;
    let lookup = substitutions.get(off..)?;
    // Reuse the class-lookup machinery: class value == replacement
    // glyph id; out-of-bounds yields the reserved class, which we
    // map back to None so the caller knows not to substitute.
    let Ok(replacement) = lookup_via_state_table(lookup, glyph) else {
        return None;
    };
    if replacement == CLASS_OUT_OF_BOUNDS {
        None
    } else {
        Some(replacement)
    }
}

/// Calls the format-2/6 AAT lookup parser without constructing a
/// whole `StateTableHeader`. Not exposed outside this module.
fn lookup_via_state_table(data: &[u8], glyph: u16) -> Result<u16> {
    // Cheap trampoline via a throwaway header that only uses its
    // class resolver. Build a synthetic 16-byte prefix that points
    // class_table_off back at offset 16 so we can bolt the real
    // lookup on. This avoids duplicating the format parser while
    // keeping the call simple.
    let mut synthetic = Vec::with_capacity(16 + data.len());
    synthetic.extend_from_slice(&0u32.to_be_bytes()); // nClasses (unused)
    synthetic.extend_from_slice(&16u32.to_be_bytes()); // class off = 16
    synthetic.extend_from_slice(&0u32.to_be_bytes()); // state off (unused)
    synthetic.extend_from_slice(&0u32.to_be_bytes()); // entry off (unused)
    synthetic.extend_from_slice(data);
    let hdr = StateTableHeader::parse(&synthetic)?;
    hdr.class_of(glyph)
}

// --- Type 2: Ligature substitution ---

fn apply_ligature(
    state: &StateTableHeader<'_>,
    lig_actions: &[u8],
    components: &[u8],
    ligatures: &[u8],
    glyphs: &mut Vec<u16>,
    origins: &mut Vec<usize>,
) {
    const ENTRY_SIZE: usize = 6; // newState + flags + actionIndex
    let mut cur_state: u16 = 0;
    let mut component_stack: Vec<usize> = Vec::new();
    let mut i = 0;
    while i <= glyphs.len() {
        let class = class_for(state, glyphs.get(i).copied()).unwrap_or(CLASS_OUT_OF_BOUNDS);
        let Ok(entry_idx) = state.entry_index(cur_state, class) else {
            return;
        };
        let Ok((new_state, flags)) = state.entry_prefix(entry_idx, ENTRY_SIZE) else {
            return;
        };
        let action_idx = state.entry_tail_u16(entry_idx, ENTRY_SIZE, 4).unwrap_or(0);

        if flags & FLAG_LIG_SET_COMPONENT != 0 && i < glyphs.len() {
            component_stack.push(i);
        }
        if flags & FLAG_LIG_PERFORM_ACTION != 0 && !component_stack.is_empty() {
            perform_ligature_action(
                action_idx,
                lig_actions,
                components,
                ligatures,
                &mut component_stack,
                glyphs,
                origins,
                &mut i,
            );
        }

        cur_state = new_state;
        if flags & FLAG_DONT_ADVANCE == 0 {
            i += 1;
        } else if i == glyphs.len() {
            return;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn perform_ligature_action(
    action_idx: u16,
    lig_actions: &[u8],
    components: &[u8],
    ligatures: &[u8],
    stack: &mut Vec<usize>,
    glyphs: &mut Vec<u16>,
    origins: &mut Vec<usize>,
    cursor: &mut usize,
) {
    // Walk action entries starting at `action_idx`, summing
    // component-table lookups into a component-index offset. The
    // `LAST` bit ends the walk; on the final action, if `STORE` is
    // set, the offset is used to index the ligature table and emit
    // the resulting glyph id.
    let mut offset: i32 = 0;
    let mut action_pos = action_idx as usize;
    let mut consumed: Vec<usize> = Vec::new();
    loop {
        if stack.is_empty() {
            return;
        }
        let stack_top = stack.pop().unwrap();
        consumed.push(stack_top);

        let action_off = action_pos * 4;
        let Some(action_bytes) = lig_actions.get(action_off..action_off + 4) else {
            return;
        };
        let action = u32::from_be_bytes([
            action_bytes[0],
            action_bytes[1],
            action_bytes[2],
            action_bytes[3],
        ]);

        let raw_off = action & LIG_ACTION_OFFSET_MASK;
        // Sign-extend from the 30-bit signed offset field to i32. Do
        // the arithmetic with two's-complement-safe casts so clippy's
        // cast_possible_wrap stays happy — we actively want the wrap,
        // that is the point of the conversion.
        let signed_off: i32 = if action & LIG_ACTION_OFFSET_SIGN != 0 {
            #[allow(clippy::cast_possible_wrap)]
            {
                (raw_off | 0xC000_0000) as i32
            }
        } else {
            #[allow(clippy::cast_possible_wrap)]
            {
                raw_off as i32
            }
        };
        let glyph_id = i32::from(glyphs[stack_top]);
        let comp_idx = glyph_id + signed_off;
        let comp_byte_off = (comp_idx as usize).saturating_mul(2);
        let Some(comp_bytes) = components.get(comp_byte_off..comp_byte_off + 2) else {
            return;
        };
        let comp_val = i32::from(u16::from_be_bytes([comp_bytes[0], comp_bytes[1]]));
        offset = offset.wrapping_add(comp_val);

        if action & LIG_ACTION_LAST != 0 {
            if action & LIG_ACTION_STORE != 0 {
                let lig_byte_off = (offset as usize).saturating_mul(2);
                if let Some(lig_bytes) = ligatures.get(lig_byte_off..lig_byte_off + 2) {
                    let lig_glyph = u16::from_be_bytes([lig_bytes[0], lig_bytes[1]]);
                    // Replace the earliest consumed slot with the
                    // ligature, drop the later slots. Sort in
                    // ascending order so the earliest index lands
                    // first — stack was LIFO so the natural order is
                    // reversed.
                    consumed.sort_unstable();
                    let keep = consumed[0];
                    glyphs[keep] = lig_glyph;
                    // origins[keep] keeps the smallest originating
                    // input index so cluster merging finds the
                    // correct grapheme root.
                    // Remove every other consumed slot, highest index
                    // first so earlier indices stay valid.
                    for &idx in consumed.iter().skip(1).rev() {
                        glyphs.remove(idx);
                        origins.remove(idx);
                        if idx < *cursor {
                            *cursor -= 1;
                        }
                    }
                }
            }
            return;
        }
        action_pos += 1;
    }
}

// --- Type 4: Non-Contextual Substitution ---

/// Walks every glyph in the run and replaces it with whatever the
/// subtable's AAT lookup yields. A lookup that returns
/// [`CLASS_OUT_OF_BOUNDS`] (the AAT "glyph not covered" sentinel) or
/// errors on a malformed slice falls through to "keep the original
/// glyph", so a partly-broken subtable can't blank out the run.
fn apply_non_contextual(lookup: &[u8], glyphs: &mut [u16]) {
    for slot in glyphs.iter_mut() {
        if let Ok(replacement) = lookup_via_state_table(lookup, *slot) {
            if replacement != CLASS_OUT_OF_BOUNDS {
                *slot = replacement;
            }
        }
    }
}

// --- Type 5: Insertion Substitution ---

/// Applies one type-5 (insertion) subtable. State-table entries are
/// 8 bytes each: `(newState, flags, currentInsertIndex, markedInsertIndex)`.
/// Flags carry the SetMark / DontAdvance bits plus before/after
/// orientation flags and the two count fields (5 bits each).
///
/// On an entry whose currentInsertIndex (or markedInsertIndex) is
/// non-`0xFFFF` and whose corresponding count is non-zero, we splice
/// `count` glyphs from the insertion-glyph table at the chosen
/// position, taking care to keep the cursor and origin map in sync.
///
/// The insertion-glyph table is a flat u16 array indexed in units of
/// glyph ids (so byte offset = index * 2).
fn apply_insertion(
    state: &StateTableHeader<'_>,
    insertion_table: &[u8],
    glyphs: &mut Vec<u16>,
    origins: &mut Vec<usize>,
) {
    const ENTRY_SIZE: usize = 8;
    let mut cur_state: u16 = 0;
    let mut mark: Option<usize> = None;
    let mut i = 0usize;
    // Bound the walk: every glyph processed at most a handful of
    // times (DontAdvance retries) before we cap, so a malformed font
    // can't loop the shaper.
    let max_iters = glyphs.len().saturating_mul(8) + 16;
    let mut iters = 0usize;
    while i <= glyphs.len() {
        iters += 1;
        if iters > max_iters {
            return;
        }
        let class = class_for(state, glyphs.get(i).copied()).unwrap_or(CLASS_OUT_OF_BOUNDS);
        let Ok(entry_idx) = state.entry_index(cur_state, class) else {
            return;
        };
        let Ok((new_state, flags)) = state.entry_prefix(entry_idx, ENTRY_SIZE) else {
            return;
        };
        let cur_index = state
            .entry_tail_u16(entry_idx, ENTRY_SIZE, 4)
            .unwrap_or(0xFFFF);
        let marked_index = state
            .entry_tail_u16(entry_idx, ENTRY_SIZE, 6)
            .unwrap_or(0xFFFF);

        let cur_count =
            ((flags & FLAG_INS_CURRENT_COUNT_MASK) >> FLAG_INS_CURRENT_COUNT_SHIFT) as usize;
        let mark_count = (flags & FLAG_INS_MARKED_COUNT_MASK) as usize;

        // Apply marked insertions first — they sit earlier in the
        // run, so splicing them first leaves the current-position
        // index valid afterwards. When the marked position lands at
        // or before the cursor, we shift the cursor forward by the
        // number of inserted glyphs.
        if marked_index != 0xFFFF && mark_count > 0 {
            if let Some(m) = mark {
                let pos = if flags & FLAG_INS_MARKED_BEFORE != 0 {
                    m
                } else {
                    m + 1
                };
                let n = splice_insertions(
                    insertion_table,
                    marked_index,
                    mark_count,
                    pos,
                    glyphs,
                    origins,
                );
                if pos <= i {
                    i += n;
                }
            }
        }
        // Current-glyph insertions. After-position inserts leave the
        // cursor on the same glyph (so the next tick advances past
        // both it and the inserted glyphs); before-position inserts
        // push the cursor past the new run so the original glyph is
        // re-processed in the new state.
        if cur_index != 0xFFFF && cur_count > 0 && i <= glyphs.len() {
            let before = flags & FLAG_INS_CURRENT_BEFORE != 0;
            let pos = if before { i } else { i + 1 };
            let n = splice_insertions(
                insertion_table,
                cur_index,
                cur_count,
                pos,
                glyphs,
                origins,
            );
            if before {
                i += n;
            }
        }

        if flags & FLAG_INS_SET_MARK != 0 {
            mark = Some(i);
        }
        cur_state = new_state;
        if flags & FLAG_DONT_ADVANCE == 0 {
            i += 1;
        } else if i == glyphs.len() {
            return;
        }
    }
}

/// Reads `count` u16 glyph ids from `insertion_table` at `index`
/// and splices them into `glyphs` / `origins` at `pos`. Returns the
/// number of glyphs actually inserted (zero when `pos` is past the
/// run end or the table doesn't cover the request).
fn splice_insertions(
    insertion_table: &[u8],
    index: u16,
    count: usize,
    pos: usize,
    glyphs: &mut Vec<u16>,
    origins: &mut Vec<usize>,
) -> usize {
    if pos > glyphs.len() {
        return 0;
    }
    let inserts = read_insertions(insertion_table, index, count);
    for (k, g) in inserts.iter().enumerate() {
        glyphs.insert(pos + k, *g);
        origins.insert(pos + k, usize::MAX);
    }
    inserts.len()
}

/// Reads `count` u16 glyph ids from the insertion-glyph table
/// starting at `index` (units of u16, not bytes). Returns an empty
/// vector if the slice doesn't cover the request — the caller treats
/// that as "no insertion".
fn read_insertions(table: &[u8], index: u16, count: usize) -> Vec<u16> {
    let start = index as usize * 2;
    let end = start + count * 2;
    let Some(slice) = table.get(start..end) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(count);
    for chunk in slice.chunks_exact(2) {
        out.push(u16::from_be_bytes([chunk[0], chunk[1]]));
    }
    out
}

fn class_for(state: &StateTableHeader<'_>, glyph: Option<u16>) -> Result<u16> {
    match glyph {
        None => Ok(CLASS_END_OF_TEXT),
        Some(0xFFFF) => Ok(CLASS_DELETED_GLYPH),
        Some(g) => state.class_of(g),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Local copy of the format-6 lookup builder used by the
    /// state_table tests; duplicated here so the morx tests do not
    /// reach into a sibling test module (`mod tests` is private).
    fn build_lookup_format6(pairs: &[(u16, u16)]) -> alloc::vec::Vec<u8> {
        let mut out: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
        out.extend_from_slice(&6u16.to_be_bytes()); // format
        out.extend_from_slice(&4u16.to_be_bytes()); // unitSize
        out.extend_from_slice(&(pairs.len() as u16).to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        for (g, v) in pairs {
            out.extend_from_slice(&g.to_be_bytes());
            out.extend_from_slice(&v.to_be_bytes());
        }
        out
    }

    // Builds a morx version-2 header, one chain with one type-2
    // ligature subtable. Returns the table bytes plus, for debugging,
    // the offset of the subtable body within the table.
    //
    // The ligature mapping: class 4 = 'f' glyph, class 5 = 'i'
    // glyph. A successful walk (class 4, then class 5) emits a
    // single replacement glyph.
    fn build_ligature_morx(f_gid: u16, i_gid: u16, lig_gid: u16) -> alloc::vec::Vec<u8> {
        // --- Inner subtable body layout ---
        // Header (16B): nClasses=6, classOff, stateOff, entryOff.
        // Then 12B: ligActionOff, componentOff, ligatureOff.
        //
        // We lay out arrays immediately after the 28-byte subtable
        // body prefix in a deterministic order.
        //
        // Classes (6): 0=EOT, 1=OOB, 2=DEL, 3=EOL, 4=f, 5=i.
        //
        // State table: 3 states × 6 classes × u16.
        //   State 0 (start):
        //     class 4 (f) -> entry 1 (newState=1, SetComponent)
        //     everything else -> entry 0 (newState=0, noop)
        //   State 1 (seen f):
        //     class 5 (i) -> entry 2 (newState=0,
        //                             SetComponent | PerformAction)
        //     everything else -> entry 0 (noop, reset)
        //
        // Entries (3 × 6 bytes):
        //   #0: newState=0, flags=0,              actionIdx=0
        //   #1: newState=1, flags=0x8000 (SetComp), actionIdx=0
        //   #2: newState=0, flags=0xA000 (SetComp|Perform), actionIdx=0
        //
        // LigAction array (1 × u32):
        //   #0: LAST | STORE | offset=0        → 0xC000_0000
        //
        // Components (f_gid entry): the sum of offsets accumulated
        // into ligature-table index; we want the accumulated
        // offset to be 0, i.e. components[f_gid] + components[i_gid]
        // = 0. Simplest: both contribute 0. But we must index by
        // glyph + signed_action_offset. With signed_offset = 0 and
        // glyph in {f_gid, i_gid} we read components[f_gid] and
        // components[i_gid]. Size the components table generously,
        // zero everywhere except — we want ligatures[0] = lig_gid.
        //
        // So: components is size max(f_gid, i_gid)+1, all zero.
        //     ligatures is size 1, ligatures[0] = lig_gid.
        //
        // NB: With one action word using LAST|STORE, both the 'f'
        // and the 'i' push pops one action read — but the state
        // machine is wired so only the second pop happens on the
        // last (PerformAction) entry, and it is that single read
        // that carries LAST|STORE. See FLAG_LIG_PERFORM_ACTION
        // semantics in apply_ligature: it executes on both popped
        // components in a single call, re-entering the loop.
        use alloc::vec;

        let classes = build_lookup_format6(&[(f_gid, 4), (i_gid, 5)]);
        // Header placeholder (16B) + 12B extension.
        let mut body: Vec<u8> = vec![0; 28];

        let class_off = body.len();
        body.extend_from_slice(&classes);

        // State array offset must be 2-byte aligned; extend to even.
        if body.len() % 2 != 0 {
            body.push(0);
        }
        let state_off = body.len();
        let n_classes = 6u16;
        let n_states = 2u16;
        // state rows
        let nc = n_classes as usize;
        let mut row = vec![0u16; nc * n_states as usize];
        // State 0 : class 4 (f) -> entry 1, else entry 0
        row[4] = 1;
        // State 1 : class 5 (i) -> entry 2, else entry 0
        row[nc + 5] = 2;
        for v in &row {
            body.extend_from_slice(&v.to_be_bytes());
        }

        // Entries
        let entry_off = body.len();
        // #0 noop
        body.extend_from_slice(&0u16.to_be_bytes()); // newState
        body.extend_from_slice(&0u16.to_be_bytes()); // flags
        body.extend_from_slice(&0u16.to_be_bytes()); // actionIdx
                                                     // #1 SetComponent -> state 1
        body.extend_from_slice(&1u16.to_be_bytes()); // newState
        body.extend_from_slice(&0x8000u16.to_be_bytes());
        body.extend_from_slice(&0u16.to_be_bytes());
        // #2 SetComponent|Perform -> state 0
        body.extend_from_slice(&0u16.to_be_bytes());
        body.extend_from_slice(&0xA000u16.to_be_bytes());
        body.extend_from_slice(&0u16.to_be_bytes());

        // Ligature actions: two words — one per component. Walked in
        // reverse pop order, so the first word corresponds to the
        // last-pushed glyph (i_gid here) and the second (with LAST |
        // STORE) to the first-pushed (f_gid). Both contribute zero
        // to the accumulated offset so the emitted ligature index is
        // 0, which maps to `ligatures[0] = lig_gid`.
        let lig_action_off = body.len();
        // Offset = -i_gid so glyph + offset = 0, picking
        // components[0] = 0. Use negative sign encoding.
        let neg_i: u32 =
            LIG_ACTION_OFFSET_SIGN | ((-(i_gid as i32)) as u32 & LIG_ACTION_OFFSET_MASK);
        body.extend_from_slice(&neg_i.to_be_bytes());
        // Last action word for f: offset = -f_gid, plus LAST | STORE.
        let neg_f: u32 = LIG_ACTION_LAST
            | LIG_ACTION_STORE
            | LIG_ACTION_OFFSET_SIGN
            | ((-(f_gid as i32)) as u32 & LIG_ACTION_OFFSET_MASK);
        body.extend_from_slice(&neg_f.to_be_bytes());

        // Components: index by glyph. Pad to max(f_gid, i_gid) + 1.
        let comp_off = body.len();
        let comp_count = core::cmp::max(f_gid, i_gid) as usize + 1;
        body.extend_from_slice(&alloc::vec![0u8; comp_count * 2]);

        // Ligatures: one entry at index 0 = lig_gid.
        let lig_off = body.len();
        body.extend_from_slice(&lig_gid.to_be_bytes());

        // Fill in the header pieces we deferred. All offsets are
        // relative to the subtable body start.
        let mut write_u32 = |pos: usize, v: u32| {
            body[pos..pos + 4].copy_from_slice(&v.to_be_bytes());
        };
        write_u32(0, n_classes as u32); // nClasses
        write_u32(4, class_off as u32);
        write_u32(8, state_off as u32);
        write_u32(12, entry_off as u32);
        write_u32(16, lig_action_off as u32);
        write_u32(20, comp_off as u32);
        write_u32(24, lig_off as u32);

        // Wrap in subtable header (12B) + chain header (16B) +
        // table header (8B).
        let sub_len = 12 + body.len();
        let mut subtable: Vec<u8> = Vec::new();
        subtable.extend_from_slice(&(sub_len as u32).to_be_bytes()); // length
        subtable.extend_from_slice(&(0x0000_0002u32).to_be_bytes()); // coverage: type 2
        subtable.extend_from_slice(&(0x0000_0001u32).to_be_bytes()); // subFeatureFlags
        subtable.extend_from_slice(&body);

        let chain_len = 16 + subtable.len();
        let mut chain: Vec<u8> = Vec::new();
        chain.extend_from_slice(&(0x0000_0001u32).to_be_bytes()); // defaultFlags
        chain.extend_from_slice(&(chain_len as u32).to_be_bytes());
        chain.extend_from_slice(&0u32.to_be_bytes()); // featureCount
        chain.extend_from_slice(&1u32.to_be_bytes()); // subtableCount
        chain.extend_from_slice(&subtable);

        let mut table: Vec<u8> = Vec::new();
        table.extend_from_slice(&2u16.to_be_bytes()); // version
        table.extend_from_slice(&0u16.to_be_bytes()); // pad
        table.extend_from_slice(&1u32.to_be_bytes()); // nChains
        table.extend_from_slice(&chain);
        table
    }

    #[test]
    fn morx_parses_version_and_chains() {
        let bytes = build_ligature_morx(10, 20, 99);
        let m = Morx::parse(&bytes).unwrap();
        assert_eq!(m.version(), 2);
        assert_eq!(m.chains().len(), 1);
    }

    #[test]
    fn morx_ligature_subtable_produces_single_glyph() {
        let bytes = build_ligature_morx(10, 20, 99);
        let m = Morx::parse(&bytes).unwrap();
        let (out, origins) = m.apply(&[10, 20]);
        assert_eq!(out, &[99]);
        assert_eq!(origins.len(), 1);
        // The ligature inherits the smaller originating index (the f
        // was input slot 0) so cluster merging finds the f's cluster
        // as the canonical root.
        assert_eq!(origins[0], 0);
    }

    #[test]
    fn morx_ligature_keeps_non_matching_input_intact() {
        let bytes = build_ligature_morx(10, 20, 99);
        let m = Morx::parse(&bytes).unwrap();
        let (out, _) = m.apply(&[10, 30, 20]);
        // f, then x (out of class), then i: no ligation.
        assert_eq!(out, &[10, 30, 20]);
    }

    #[test]
    fn morx_rejects_unknown_version() {
        let mut bytes: Vec<u8> = Vec::new();
        bytes.extend_from_slice(&7u16.to_be_bytes());
        bytes.extend_from_slice(&0u16.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());
        assert!(matches!(
            Morx::parse(&bytes),
            Err(Error::Unsupported { .. })
        ));
    }

    // -----------------------------------------------------------------
    // Type 4 — Non-contextual substitution.
    // -----------------------------------------------------------------

    /// Builds a morx version-2 table with a single chain containing
    /// a single type-4 subtable. The subtable's body is one AAT
    /// lookup (format 6) that maps `pairs` (gid_in → gid_out).
    fn build_non_contextual_morx(pairs: &[(u16, u16)]) -> Vec<u8> {
        let mut sorted = pairs.to_vec();
        sorted.sort_by_key(|p| p.0);
        let lookup = build_lookup_format6(&sorted);

        let body = lookup;
        let sub_len = 12 + body.len();
        let mut subtable: Vec<u8> = Vec::new();
        subtable.extend_from_slice(&(sub_len as u32).to_be_bytes());
        subtable.extend_from_slice(&0x0000_0004u32.to_be_bytes()); // type 4
        subtable.extend_from_slice(&0x0000_0001u32.to_be_bytes()); // subFeatureFlags
        subtable.extend_from_slice(&body);

        let chain_len = 16 + subtable.len();
        let mut chain: Vec<u8> = Vec::new();
        chain.extend_from_slice(&0x0000_0001u32.to_be_bytes()); // defaultFlags
        chain.extend_from_slice(&(chain_len as u32).to_be_bytes());
        chain.extend_from_slice(&0u32.to_be_bytes()); // featureCount
        chain.extend_from_slice(&1u32.to_be_bytes()); // subtableCount
        chain.extend_from_slice(&subtable);

        let mut table: Vec<u8> = Vec::new();
        table.extend_from_slice(&2u16.to_be_bytes()); // version
        table.extend_from_slice(&0u16.to_be_bytes());
        table.extend_from_slice(&1u32.to_be_bytes()); // nChains
        table.extend_from_slice(&chain);
        table
    }

    #[test]
    fn morx_non_contextual_substitutes_known_glyphs() {
        // gid 5 → gid 50, gid 7 → gid 70. Untouched glyphs pass through.
        let bytes = build_non_contextual_morx(&[(5, 50), (7, 70)]);
        let m = Morx::parse(&bytes).unwrap();
        let (out, _) = m.apply(&[5, 9, 7]);
        assert_eq!(out, &[50, 9, 70]);
    }

    #[test]
    fn morx_non_contextual_leaves_unmapped_glyphs_alone() {
        let bytes = build_non_contextual_morx(&[(5, 50)]);
        let m = Morx::parse(&bytes).unwrap();
        let (out, _) = m.apply(&[1, 2, 3]);
        assert_eq!(out, &[1, 2, 3]);
    }

    #[test]
    fn morx_non_contextual_handles_empty_input() {
        let bytes = build_non_contextual_morx(&[(5, 50)]);
        let m = Morx::parse(&bytes).unwrap();
        let (out, _) = m.apply(&[]);
        assert!(out.is_empty());
    }

    // -----------------------------------------------------------------
    // Type 5 — Insertion.
    // -----------------------------------------------------------------

    /// Builds a morx version-2 table with one chain that carries a
    /// single type-5 (insertion) subtable wired to inject `marker_gid`
    /// after every `trigger_gid` it sees.
    ///
    /// State machine:
    ///   class 4 = trigger_gid; everything else falls through.
    ///   State 0 (only state):
    ///     class 4 → entry 1 (currentInsertCount=1, inserts the
    ///                        single-glyph table starting at index 0).
    ///     other classes → entry 0 (noop).
    fn build_insertion_morx_after_trigger(trigger_gid: u16, marker_gid: u16) -> Vec<u8> {
        let class_lookup = build_lookup_format6(&[(trigger_gid, 4)]);

        // Body layout (relative to body start):
        //   0..16   state-table header
        //  16..20   insertionGlyphTable offset (u32)
        //  20..     class lookup (aligned to 2)
        //  ..       state array (1 state × 5 classes × u16) = 10 B
        //  ..       entry array (2 entries × 8 B) = 16 B
        //  ..       insertion glyph table (one u16 = marker_gid)
        let n_classes: u32 = 5;
        let n_states: u32 = 1;
        let n_entries: usize = 2;

        let header_len = 20;
        let class_off = header_len;
        let class_end = class_off + class_lookup.len();
        let state_off = class_end + (class_end % 2);
        let state_bytes = (n_states * n_classes) as usize * 2;
        let entry_off = state_off + state_bytes;
        let entry_bytes = n_entries * 8;
        let ins_off = entry_off + entry_bytes;
        let ins_bytes = 2usize;

        let body_len = ins_off + ins_bytes;

        let mut body: Vec<u8> = Vec::with_capacity(body_len);
        body.extend_from_slice(&n_classes.to_be_bytes());
        body.extend_from_slice(&(class_off as u32).to_be_bytes());
        body.extend_from_slice(&(state_off as u32).to_be_bytes());
        body.extend_from_slice(&(entry_off as u32).to_be_bytes());
        body.extend_from_slice(&(ins_off as u32).to_be_bytes());
        body.extend_from_slice(&class_lookup);
        if body.len() < state_off {
            body.resize(state_off, 0);
        }
        // State 0:
        let s0: [u16; 5] = [0, 0, 0, 0, 1];
        for v in &s0 {
            body.extend_from_slice(&v.to_be_bytes());
        }
        // Entries (newState, flags, currentInsertIndex, markedInsertIndex)
        // #0 noop
        body.extend_from_slice(&0u16.to_be_bytes());
        body.extend_from_slice(&0u16.to_be_bytes()); // flags
        body.extend_from_slice(&0xFFFFu16.to_be_bytes()); // cur idx
        body.extend_from_slice(&0xFFFFu16.to_be_bytes()); // mark idx
        // #1 insert 1 glyph after current (CurrentInsertCount=1, no
        // before-flag → after, list at index 0).
        // Flags: count=1 in bits 5..9 → 1 << 5 = 0x0020.
        let entry1_flags: u16 = 1 << FLAG_INS_CURRENT_COUNT_SHIFT;
        body.extend_from_slice(&0u16.to_be_bytes()); // newState
        body.extend_from_slice(&entry1_flags.to_be_bytes());
        body.extend_from_slice(&0u16.to_be_bytes()); // currentInsertIndex = 0
        body.extend_from_slice(&0xFFFFu16.to_be_bytes()); // markedInsertIndex
        // Insertion glyph table.
        body.extend_from_slice(&marker_gid.to_be_bytes());

        let sub_len = 12 + body.len();
        let mut subtable: Vec<u8> = Vec::new();
        subtable.extend_from_slice(&(sub_len as u32).to_be_bytes());
        subtable.extend_from_slice(&0x0000_0005u32.to_be_bytes()); // type 5
        subtable.extend_from_slice(&0x0000_0001u32.to_be_bytes()); // subFeatureFlags
        subtable.extend_from_slice(&body);

        let chain_len = 16 + subtable.len();
        let mut chain: Vec<u8> = Vec::new();
        chain.extend_from_slice(&0x0000_0001u32.to_be_bytes()); // defaultFlags
        chain.extend_from_slice(&(chain_len as u32).to_be_bytes());
        chain.extend_from_slice(&0u32.to_be_bytes());
        chain.extend_from_slice(&1u32.to_be_bytes());
        chain.extend_from_slice(&subtable);

        let mut table: Vec<u8> = Vec::new();
        table.extend_from_slice(&2u16.to_be_bytes());
        table.extend_from_slice(&0u16.to_be_bytes());
        table.extend_from_slice(&1u32.to_be_bytes());
        table.extend_from_slice(&chain);
        table
    }

    #[test]
    fn morx_insertion_appends_marker_after_trigger() {
        // trigger gid 7, marker gid 99.
        let bytes = build_insertion_morx_after_trigger(7, 99);
        let m = Morx::parse(&bytes).unwrap();
        let (out, origins) = m.apply(&[1, 7, 2]);
        // Trigger lands at index 1; marker is inserted *after* it.
        assert_eq!(out, &[1, 7, 99, 2]);
        // Inserted glyph has no originating input — marked with
        // usize::MAX.
        assert_eq!(origins, &[0, 1, usize::MAX, 2]);
    }

    #[test]
    fn morx_insertion_handles_no_trigger() {
        let bytes = build_insertion_morx_after_trigger(7, 99);
        let m = Morx::parse(&bytes).unwrap();
        let (out, _) = m.apply(&[1, 2, 3]);
        assert_eq!(out, &[1, 2, 3], "no insertion when trigger absent");
    }

    #[test]
    fn morx_insertion_fires_for_each_trigger() {
        let bytes = build_insertion_morx_after_trigger(7, 99);
        let m = Morx::parse(&bytes).unwrap();
        let (out, _) = m.apply(&[7, 7]);
        assert_eq!(out, &[7, 99, 7, 99]);
    }

    #[test]
    fn morx_insertion_handles_empty_input() {
        let bytes = build_insertion_morx_after_trigger(7, 99);
        let m = Morx::parse(&bytes).unwrap();
        let (out, _) = m.apply(&[]);
        assert!(out.is_empty());
    }

    #[test]
    fn morx_skips_subtable_with_disabled_feature() {
        // Build a normal morx and then clobber the chain's
        // defaultFlags to zero — the subtable's sub_feature_flags &
        // default_flags = 0, so apply should be a noop.
        let mut bytes = build_ligature_morx(10, 20, 99);
        // table header 8 bytes, then chain defaultFlags is the next
        // u32 at offset 8.
        bytes[8..12].copy_from_slice(&0u32.to_be_bytes());
        let m = Morx::parse(&bytes).unwrap();
        let (out, _) = m.apply(&[10, 20]);
        assert_eq!(out, &[10, 20]);
    }
}
