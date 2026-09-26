//! Neutral and bracket resolution: the strong and neutral
//! classifications rules N1 and N2 use, and the N0 paired-bracket
//! rule.

use alloc::vec::Vec;

use super::explicit::IsolatingSequence;
use super::BidiClass;
use crate::unicode::bidi_brackets::{bracket_of, BracketType};

/// "Strong" classification for N1: maps L to L; R / EN / AN to R;
/// anything else to None. (UAX #9 §3.3.4.)
pub(super) const fn n_strong(c: BidiClass) -> Option<BidiClass> {
    match c {
        BidiClass::L => Some(BidiClass::L),
        BidiClass::R | BidiClass::En | BidiClass::An => Some(BidiClass::R),
        _ => None,
    }
}

/// True for "neutral and isolate" types per UAX #9 BD11, the
/// targets of N1 / N2 resolution.
pub(super) const fn is_ni(c: BidiClass) -> bool {
    matches!(
        c,
        BidiClass::B
            | BidiClass::S
            | BidiClass::Ws
            | BidiClass::On
            | BidiClass::Fsi
            | BidiClass::Lri
            | BidiClass::Rli
            | BidiClass::Pdi
    )
}

// ---------------------------------------------------------------------
// N0: paired-bracket resolution.
// ---------------------------------------------------------------------

/// Maximum number of pending bracket openers a single isolating-run
/// sequence may carry per UAX #9 BD16. The spec caps the stack at 63
/// to keep pathological input bounded; we mirror that.
const N0_BRACKET_STACK_MAX: usize = 63;

/// Implements UAX #9 rule N0 against the post-W7 class list of one
/// isolating-run sequence. `classes[i]` is the resolved class for
/// `seq.indices[i]`; `chars[seq.indices[i]]` is the original
/// codepoint, used only for bracket lookup.
///
/// The pass:
///
/// 1. Identifies BD16 bracket pairs by walking the sequence with a
///    stack of pending openers (capped at 63 per the spec).
/// 2. For each matched pair, scans the strongly-typed characters
///    *between* the open and close. If the embedding direction's
///    strong type appears, the pair takes the embedding direction.
///    Otherwise, if the opposite-direction strong appears, the pair
///    takes that direction iff the strong before the opener (or sos)
///    is the opposite direction; if it's the embedding direction
///    (or no strong before), the pair takes the embedding direction.
///    No strong inside -> the pair is left alone (N1 / N2 handle it).
/// 3. When a pair fires, both bracket cells get their class swapped
///    to the resolved strong (L or R), plus any NSMs immediately
///    following each bracket within the sequence per N0's
///    "carry along the NSMs" clause.
pub(super) fn apply_n0(classes: &mut [BidiClass], chars: &[char], seq: &IsolatingSequence) {
    if classes.is_empty() {
        return;
    }
    let embed_strong = if seq.level % 2 == 1 {
        BidiClass::R
    } else {
        BidiClass::L
    };
    let opposite_strong = if embed_strong == BidiClass::L {
        BidiClass::R
    } else {
        BidiClass::L
    };

    // BD16: walk the sequence and pair matched brackets. Each entry
    // on the stack is `(seq_index, opener_codepoint, close_codepoint)`.
    // When a closer matches the top open, pop. When a closer matches
    // an *earlier* opener on the stack, pop that opener and discard
    // every entry above it (BD16's "skip mismatched" rule).
    let mut stack: Vec<(usize, u32, u32)> = Vec::with_capacity(8);
    // Pairs in *opener-position order* (UAX 9 N0 says process in
    // text order, which here means opener position).
    let mut pairs: Vec<(usize, usize)> = Vec::new();
    for (i, &ci) in seq.indices.iter().enumerate() {
        // Brackets must come from the ON neutral class: N0 only
        // touches characters whose post-W class is ON. Skip anything
        // already promoted to a strong / weak class by W1-W7.
        if classes[i] != BidiClass::On {
            continue;
        }
        let cp = chars.get(ci).copied().map_or(0u32, |c| c as u32);
        let Some(entry) = bracket_of(cp) else {
            continue;
        };
        match entry.kind {
            BracketType::Open => {
                if stack.len() < N0_BRACKET_STACK_MAX {
                    stack.push((i, cp, entry.pair));
                }
            }
            BracketType::Close => {
                // Find the topmost opener whose close codepoint equals
                // this closer's codepoint.
                if let Some(pos) = stack.iter().rposition(|&(_, _, close_cp)| close_cp == cp) {
                    let (open_seq_i, _, _) = stack[pos];
                    pairs.push((open_seq_i, i));
                    stack.truncate(pos);
                }
            }
        }
    }

    if pairs.is_empty() {
        return;
    }
    // Process pairs in opener-position order: the UAX 9 algorithm
    // resolves earlier-opened pairs first so a later pair can see the
    // earlier pair's resolution as a strong type.
    pairs.sort_by_key(|&(open, _)| open);

    for &(open, close) in &pairs {
        // Scan strong types strictly between open and close.
        let mut saw_embed = false;
        let mut saw_opposite = false;
        for c in &classes[open + 1..close] {
            match strong_for_n0(*c) {
                Some(s) if s == embed_strong => {
                    saw_embed = true;
                    break;
                }
                Some(s) if s == opposite_strong => {
                    saw_opposite = true;
                }
                _ => {}
            }
        }

        let resolved = if saw_embed {
            Some(embed_strong)
        } else if saw_opposite {
            // Establish the strong context preceding the opener: walk
            // back through the sequence's resolved classes until a
            // strong type or the sos is found.
            let mut k = open;
            let preceding = loop {
                if k == 0 {
                    break strong_for_n0(seq.sos);
                }
                k -= 1;
                if let Some(s) = strong_for_n0(classes[k]) {
                    break Some(s);
                }
            };
            if preceding == Some(opposite_strong) {
                Some(opposite_strong)
            } else {
                Some(embed_strong)
            }
        } else {
            None
        };

        if let Some(r) = resolved {
            classes[open] = r;
            classes[close] = r;
            // N0 §3.1.3: any NSMs that immediately follow either
            // bracket take the same resolved type. Walk forward past
            // the bracket cells until a non-NSM is hit.
            for c in classes.iter_mut().skip(open + 1) {
                if *c == BidiClass::Nsm {
                    *c = r;
                } else {
                    break;
                }
            }
            for c in classes.iter_mut().skip(close + 1) {
                if *c == BidiClass::Nsm {
                    *c = r;
                } else {
                    break;
                }
            }
        }
    }
}

/// Maps a post-W class to its N0 strong category. EN / AN both count
/// as R direction for N0 (per the spec: EN/AN are "weak strong").
const fn strong_for_n0(c: BidiClass) -> Option<BidiClass> {
    match c {
        BidiClass::L => Some(BidiClass::L),
        BidiClass::R | BidiClass::En | BidiClass::An => Some(BidiClass::R),
        _ => None,
    }
}
