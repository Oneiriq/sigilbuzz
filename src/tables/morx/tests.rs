//! Tests for the `morx` parser and its subtable types.

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
    // State table: 3 states * 6 classes * u16.
    //   State 0 (start):
    //     class 4 (f) -> entry 1 (newState=1, SetComponent)
    //     everything else -> entry 0 (newState=0, noop)
    //   State 1 (seen f):
    //     class 5 (i) -> entry 2 (newState=0,
    //                             SetComponent | PerformAction)
    //     everything else -> entry 0 (noop, reset)
    //
    // Entries (3 * 6 bytes):
    //   #0: newState=0, flags=0,              actionIdx=0
    //   #1: newState=1, flags=0x8000 (SetComp), actionIdx=0
    //   #2: newState=0, flags=0xA000 (SetComp|Perform), actionIdx=0
    //
    // LigAction array (1 * u32):
    //   #0: LAST | STORE | offset=0        -> 0xC000_0000
    //
    // Components (f_gid entry): the sum of offsets accumulated
    // into ligature-table index; we want the accumulated
    // offset to be 0, i.e. components[f_gid] + components[i_gid]
    // = 0. Simplest: both contribute 0. But we must index by
    // glyph + signed_action_offset. With signed_offset = 0 and
    // glyph in {f_gid, i_gid} we read components[f_gid] and
    // components[i_gid]. Size the components table generously,
    // zero everywhere except: we want ligatures[0] = lig_gid.
    //
    // So: components is size max(f_gid, i_gid)+1, all zero.
    //     ligatures is size 1, ligatures[0] = lig_gid.
    //
    // NB: With one action word using LAST|STORE, both the 'f'
    // and the 'i' push pops one action read, but the state
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

    // Ligature actions: two words, one per component. Walked in
    // reverse pop order, so the first word corresponds to the
    // last-pushed glyph (i_gid here) and the second (with LAST |
    // STORE) to the first-pushed (f_gid). Both contribute zero
    // to the accumulated offset so the emitted ligature index is
    // 0, which maps to `ligatures[0] = lig_gid`.
    let lig_action_off = body.len();
    // Offset = -i_gid so glyph + offset = 0, picking
    // components[0] = 0. Use negative sign encoding.
    let neg_i: u32 = LIG_ACTION_OFFSET_SIGN | ((-(i_gid as i32)) as u32 & LIG_ACTION_OFFSET_MASK);
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
// Type 4: Non-contextual substitution.
// -----------------------------------------------------------------

/// Builds a morx version-2 table with a single chain containing
/// a single type-4 subtable. The subtable's body is one AAT
/// lookup (format 6) that maps `pairs` (gid_in -> gid_out).
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
    // gid 5 -> gid 50, gid 7 -> gid 70. Untouched glyphs pass through.
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
// Type 5: Insertion.
// -----------------------------------------------------------------

/// Builds a morx version-2 table with one chain that carries a
/// single type-5 (insertion) subtable wired to inject `marker_gid`
/// after every `trigger_gid` it sees.
///
/// State machine:
///   class 4 = trigger_gid; everything else falls through.
///   State 0 (only state):
///     class 4 -> entry 1 (currentInsertCount=1, inserts the
///                        single-glyph table starting at index 0).
///     other classes -> entry 0 (noop).
fn build_insertion_morx_after_trigger(trigger_gid: u16, marker_gid: u16) -> Vec<u8> {
    let class_lookup = build_lookup_format6(&[(trigger_gid, 4)]);

    // Body layout (relative to body start):
    //   0..16   state-table header
    //  16..20   insertionGlyphTable offset (u32)
    //  20..     class lookup (aligned to 2)
    //  ..       state array (1 state * 5 classes * u16) = 10 B
    //  ..       entry array (2 entries * 8 B) = 16 B
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
                                                      // before-flag -> after, list at index 0).
                                                      // Flags: count=1 in bits 5..9 -> 1 << 5 = 0x0020.
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
    // Inserted glyph has no originating input, marked with
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
    // defaultFlags to zero. The subtable's sub_feature_flags &
    // default_flags = 0, so apply should be a noop.
    let mut bytes = build_ligature_morx(10, 20, 99);
    // table header 8 bytes, then chain defaultFlags is the next
    // u32 at offset 8.
    bytes[8..12].copy_from_slice(&0u32.to_be_bytes());
    let m = Morx::parse(&bytes).unwrap();
    let (out, _) = m.apply(&[10, 20]);
    assert_eq!(out, &[10, 20]);
}

// -----------------------------------------------------------------
// Malformed input.
// -----------------------------------------------------------------

/// Wraps pre-built subtables (each with its 12-byte header) in a
/// version-2 morx table with one chain whose default flags are 1.
fn wrap_in_chain(subtables: &[&[u8]]) -> Vec<u8> {
    let body_len: usize = subtables.iter().map(|s| s.len()).sum();
    let mut table: Vec<u8> = Vec::new();
    table.extend_from_slice(&2u16.to_be_bytes()); // version
    table.extend_from_slice(&0u16.to_be_bytes()); // pad
    table.extend_from_slice(&1u32.to_be_bytes()); // nChains
    table.extend_from_slice(&1u32.to_be_bytes()); // defaultFlags
    table.extend_from_slice(&((16 + body_len) as u32).to_be_bytes());
    table.extend_from_slice(&0u32.to_be_bytes()); // featureCount
    table.extend_from_slice(&(subtables.len() as u32).to_be_bytes());
    for s in subtables {
        table.extend_from_slice(s);
    }
    table
}

/// Prefixes `body` with a subtable header of type `sub_type` and
/// subFeatureFlags 1.
fn subtable(sub_type: u8, body: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    out.extend_from_slice(&((12 + body.len()) as u32).to_be_bytes());
    out.extend_from_slice(&u32::from(sub_type).to_be_bytes());
    out.extend_from_slice(&1u32.to_be_bytes());
    out.extend_from_slice(body);
    out
}

/// Builds a state-table subtable body with four classes, where
/// every glyph falls in class 1 (out of bounds). `ext_words` is the
/// number of type-specific u32 offsets after the 16-byte header.
/// Offset `k` points at `tail[k]`. The rest stay zero.
fn state_body(ext_words: usize, states: &[[u16; 4]], entries: &[&[u8]], tail: &[&[u8]]) -> Vec<u8> {
    let class_off = 16 + 4 * ext_words;
    // Format-6 lookup with no records: every glyph is out of bounds.
    let lookup = build_lookup_format6(&[]);
    let state_off = class_off + lookup.len();
    let entry_off = state_off + states.len() * 8;
    let mut body: Vec<u8> = Vec::new();
    body.extend_from_slice(&4u32.to_be_bytes()); // nClasses
    body.extend_from_slice(&(class_off as u32).to_be_bytes());
    body.extend_from_slice(&(state_off as u32).to_be_bytes());
    body.extend_from_slice(&(entry_off as u32).to_be_bytes());
    let ext_start = body.len();
    body.resize(ext_start + 4 * ext_words, 0);
    body.extend_from_slice(&lookup);
    for row in states {
        for v in row {
            body.extend_from_slice(&v.to_be_bytes());
        }
    }
    for e in entries {
        body.extend_from_slice(e);
    }
    for (k, part) in tail.iter().enumerate() {
        let off = body.len() as u32;
        body[ext_start + 4 * k..ext_start + 4 * k + 4].copy_from_slice(&off.to_be_bytes());
        body.extend_from_slice(part);
    }
    body
}

#[test]
fn morx_huge_chain_count_does_not_reserve_memory() {
    // nChains = u32::MAX with no chain data. The parser used to
    // reserve room for four billion chains up front.
    let mut bytes: Vec<u8> = Vec::new();
    bytes.extend_from_slice(&2u16.to_be_bytes());
    bytes.extend_from_slice(&0u16.to_be_bytes());
    bytes.extend_from_slice(&u32::MAX.to_be_bytes());
    assert!(Morx::parse(&bytes).is_err());
}

#[test]
fn morx_huge_subtable_count_does_not_reserve_memory() {
    // One chain claims u32::MAX subtables but carries none.
    let mut bytes = wrap_in_chain(&[]);
    bytes[20..24].copy_from_slice(&u32::MAX.to_be_bytes());
    assert!(Morx::parse(&bytes).is_err());
}

#[test]
fn morx_zero_length_chain_does_not_reread_itself() {
    // A chain whose chainLength is 0 used to send the cursor back
    // to its own start, so every one of the u32::MAX declared
    // chains re-read the same header.
    let mut bytes = wrap_in_chain(&[]);
    bytes[4..8].copy_from_slice(&u32::MAX.to_be_bytes()); // nChains
    bytes[12..16].copy_from_slice(&0u32.to_be_bytes()); // chainLength
    assert!(Morx::parse(&bytes).is_err());
}

#[test]
fn morx_subtable_shorter_than_header_is_skipped() {
    // First subtable declares length 0, which used to panic on a
    // reversed slice range. It is dropped and the next one still
    // applies.
    let mut short = subtable(TYPE_NON_CONTEXTUAL, &[]);
    short[0..4].copy_from_slice(&0u32.to_be_bytes());
    let good = subtable(TYPE_NON_CONTEXTUAL, &build_lookup_format6(&[(5, 50)]));
    let bytes = wrap_in_chain(&[&short, &good]);
    let m = Morx::parse(&bytes).unwrap();
    let (out, _) = m.apply(&[5, 6]);
    assert_eq!(out, &[50, 6]);
}

/// Entry that keeps the state machine on the same glyph forever.
const STAY: u16 = FLAG_DONT_ADVANCE;

#[test]
fn morx_rearrangement_dont_advance_loop_terminates() {
    let entry = [0u16.to_be_bytes(), STAY.to_be_bytes()].concat();
    let body = state_body(0, &[[0, 0, 0, 0]], &[&entry], &[]);
    let bytes = wrap_in_chain(&[&subtable(TYPE_REARRANGEMENT, &body)]);
    let m = Morx::parse(&bytes).unwrap();
    let (out, _) = m.apply(&[1, 2, 3]);
    assert_eq!(out, &[1, 2, 3]);
}

#[test]
fn morx_contextual_dont_advance_loop_terminates() {
    let entry = [
        0u16.to_be_bytes(),
        STAY.to_be_bytes(),
        0xFFFFu16.to_be_bytes(),
        0xFFFFu16.to_be_bytes(),
    ]
    .concat();
    let body = state_body(1, &[[0, 0, 0, 0]], &[&entry], &[]);
    let bytes = wrap_in_chain(&[&subtable(TYPE_CONTEXTUAL, &body)]);
    let m = Morx::parse(&bytes).unwrap();
    let (out, _) = m.apply(&[1, 2, 3]);
    assert_eq!(out, &[1, 2, 3]);
}

#[test]
fn morx_ligature_dont_advance_loop_terminates() {
    // Each step also pushes a component, so the old unbounded walk
    // grew the component stack without limit.
    let flags = STAY | FLAG_LIG_SET_COMPONENT;
    let entry = [0u16.to_be_bytes(), flags.to_be_bytes(), 0u16.to_be_bytes()].concat();
    let body = state_body(3, &[[0, 0, 0, 0]], &[&entry], &[]);
    let bytes = wrap_in_chain(&[&subtable(TYPE_LIGATURE, &body)]);
    let m = Morx::parse(&bytes).unwrap();
    let (out, _) = m.apply(&[1, 2, 3]);
    assert_eq!(out, &[1, 2, 3]);
}

/// Ligature entry: `(newState, flags, actionIndex = 0)`.
fn lig_entry(new_state: u16, flags: u16) -> Vec<u8> {
    [
        new_state.to_be_bytes(),
        flags.to_be_bytes(),
        0u16.to_be_bytes(),
    ]
    .concat()
}

#[test]
fn morx_ligature_duplicate_components_do_not_panic() {
    // The glyph at index 1 is pushed three times (DontAdvance),
    // then one action consumes all three pushes. Removing the
    // duplicate slots used to call `Vec::remove` past the end.
    let push_stay = FLAG_LIG_SET_COMPONENT | STAY;
    let push_act = FLAG_LIG_SET_COMPONENT | FLAG_LIG_PERFORM_ACTION;
    let entries = [
        lig_entry(0, 0),
        lig_entry(1, 0),
        lig_entry(2, push_stay),
        lig_entry(3, push_stay),
        lig_entry(0, push_act),
    ];
    let entry_refs: Vec<&[u8]> = entries.iter().map(Vec::as_slice).collect();
    let states = [[0, 1, 0, 0], [0, 2, 0, 0], [0, 3, 0, 0], [0, 4, 0, 0]];
    let actions = [0u32, 0, LIG_ACTION_LAST | LIG_ACTION_STORE]
        .iter()
        .flat_map(|a| a.to_be_bytes())
        .collect::<Vec<u8>>();
    let components = [0u8; 12];
    let ligatures = 99u16.to_be_bytes();
    let body = state_body(
        3,
        &states,
        &entry_refs,
        &[&actions, &components, &ligatures],
    );
    let bytes = wrap_in_chain(&[&subtable(TYPE_LIGATURE, &body)]);
    let m = Morx::parse(&bytes).unwrap();
    let (out, origins) = m.apply(&[5, 5]);
    assert_eq!(out.len(), origins.len());
    assert!(out.len() <= 2);
}

#[test]
fn morx_ligature_negative_component_index_is_ignored() {
    // The action offset is -10, so glyph 5 maps to component -5.
    // Turning that into a byte offset used to overflow.
    let minus_ten = 0u32.wrapping_sub(10);
    let action = LIG_ACTION_LAST | LIG_ACTION_STORE | (minus_ten & LIG_ACTION_OFFSET_MASK);
    let entry = lig_entry(0, FLAG_LIG_SET_COMPONENT | FLAG_LIG_PERFORM_ACTION);
    let body = state_body(
        3,
        &[[0, 0, 0, 0]],
        &[&entry],
        &[&action.to_be_bytes(), &[0u8; 12], &99u16.to_be_bytes()],
    );
    let bytes = wrap_in_chain(&[&subtable(TYPE_LIGATURE, &body)]);
    let m = Morx::parse(&bytes).unwrap();
    let (out, _) = m.apply(&[5]);
    assert_eq!(out, &[5]);
}

#[test]
fn morx_chained_insertions_stay_bounded() {
    // Every subtable inserts glyph 7 after each glyph 7, so each
    // one multiplied the run length by about nine. Eight of them
    // grew one glyph into tens of millions.
    let one = build_insertion_morx_after_trigger(7, 7);
    let sub = &one[24..];
    let bytes = wrap_in_chain(&[sub; 8]);
    let m = Morx::parse(&bytes).unwrap();
    let (out, origins) = m.apply(&[7]);
    assert_eq!(out.len(), origins.len());
    assert!(out.len() <= MAX_LEN_MIN, "run grew to {}", out.len());
}

// -----------------------------------------------------------------
// Type 1: contextual substitution, as HarfBuzz runs it. Every glyph
// of these runs falls in the out-of-bounds class (1).
// -----------------------------------------------------------------

/// Contextual entry `(newState, flags, markIndex, currentIndex)`.
fn ctx_entry(new_state: u16, flags: u16, mark: u16, current: u16) -> Vec<u8> {
    [new_state, flags, mark, current]
        .iter()
        .flat_map(|v| v.to_be_bytes())
        .collect()
}

/// A substitution table: an unsized array of u32 offsets from its
/// start, one per lookup, then the lookups.
fn substitution_table(lookups: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut at = 4 * lookups.len();
    for l in lookups {
        out.extend_from_slice(&(at as u32).to_be_bytes());
        at += l.len();
    }
    for l in lookups {
        out.extend_from_slice(l);
    }
    out
}

fn contextual_morx(states: &[[u16; 4]], entries: &[Vec<u8>], lookups: &[Vec<u8>]) -> Vec<u8> {
    let refs: Vec<&[u8]> = entries.iter().map(Vec::as_slice).collect();
    let table = substitution_table(lookups);
    let body = state_body(1, states, &refs, &[&table]);
    wrap_in_chain(&[&subtable(TYPE_CONTEXTUAL, &body)])
}

#[test]
fn contextual_substitution_table_is_an_unsized_offset_array() {
    // Two lookups. The table starts with their offsets, 8 and 8 plus
    // the first lookup, with no count before them; reading a u16
    // count there found 0 tables and substituted nothing.
    let lookups = [
        build_lookup_format6(&[(5, 50)]),
        build_lookup_format6(&[(5, 51), (6, 61)]),
    ];
    for (index, expected) in [(0u16, [50, 6]), (1, [51, 61])] {
        let entries = [ctx_entry(0, 0, 0xFFFF, index)];
        let bytes = contextual_morx(&[[0, 0, 0, 0]], &entries, &lookups);
        let m = Morx::parse(&bytes).unwrap();
        assert_eq!(m.apply(&[5, 6]).0, expected, "lookup {index}");
    }
    // An index past the offsets substitutes nothing.
    let entries = [ctx_entry(0, 0, 0xFFFF, 9)];
    let bytes = contextual_morx(&[[0, 0, 0, 0]], &entries, &lookups);
    assert_eq!(Morx::parse(&bytes).unwrap().apply(&[5, 6]).0, [5, 6]);
}

#[test]
fn contextual_end_of_text_substitutes_only_after_a_mark() {
    // A glyph takes entry 1 to state 1, whose end-of-text entry (2)
    // substitutes the current glyph. HarfBuzz (after CoreText) does
    // that only when a mark was set, and then on the last glyph.
    let lookups = [build_lookup_format6(&[(5, 50), (6, 60)])];
    let states = [[0, 1, 0, 0], [2, 1, 0, 0]];
    for (mark_flag, expected) in [(0, [5, 6]), (FLAG_CTX_SET_MARK, [5, 60])] {
        let entries = [
            ctx_entry(0, 0, 0xFFFF, 0xFFFF),
            ctx_entry(1, mark_flag, 0xFFFF, 0xFFFF),
            ctx_entry(0, 0, 0xFFFF, 0),
        ];
        let bytes = contextual_morx(&states, &entries, &lookups);
        let m = Morx::parse(&bytes).unwrap();
        assert_eq!(m.apply(&[5, 6]).0, expected, "mark flag {mark_flag:#x}");
    }
}

#[test]
fn contextual_mark_starts_on_the_first_glyph() {
    // A mark substitution before any SetMark replaces glyph 0, as in
    // HarfBuzz, where the mark index starts at zero.
    let lookups = [build_lookup_format6(&[(5, 50)])];
    let entries = [ctx_entry(0, 0, 0, 0xFFFF)];
    let bytes = contextual_morx(&[[0, 0, 0, 0]], &entries, &lookups);
    let m = Morx::parse(&bytes).unwrap();
    assert_eq!(m.apply(&[5, 5, 5]).0, [50, 5, 5]);
}

// -----------------------------------------------------------------
// Type 2: ligatures, as HarfBuzz forms them. Every glyph of these
// runs falls in the out-of-bounds class (1), so the states alone
// drive the walk.
// -----------------------------------------------------------------

/// Ligature entry with an explicit action index.
fn lig_entry_at(new_state: u16, flags: u16, action: u16) -> Vec<u8> {
    [new_state, flags, action]
        .iter()
        .flat_map(|v| v.to_be_bytes())
        .collect()
}

/// A ligature subtable over `states` and `entries`, with action words
/// `actions`, `components[index] = value` for each pair, and the
/// ligature list `ligatures`.
fn ligature_morx(
    states: &[[u16; 4]],
    entries: &[Vec<u8>],
    actions: &[u32],
    components: &[(usize, u16)],
    ligatures: &[u16],
) -> Vec<u8> {
    let refs: Vec<&[u8]> = entries.iter().map(Vec::as_slice).collect();
    let actions: Vec<u8> = actions.iter().flat_map(|a| a.to_be_bytes()).collect();
    let mut comps = vec![0u16; 256];
    for &(i, v) in components {
        comps[i] = v;
    }
    let comps: Vec<u8> = comps.iter().flat_map(|c| c.to_be_bytes()).collect();
    let ligs: Vec<u8> = ligatures.iter().flat_map(|g| g.to_be_bytes()).collect();
    let body = state_body(3, states, &refs, &[&actions, &comps, &ligs]);
    wrap_in_chain(&[&subtable(TYPE_LIGATURE, &body)])
}

const LS: u32 = LIG_ACTION_LAST | LIG_ACTION_STORE;
const PUSH: u16 = FLAG_LIG_SET_COMPONENT;
const ACT: u16 = FLAG_LIG_PERFORM_ACTION;

#[test]
fn ligature_component_set_twice_counts_once() {
    // Glyph 1 is set as a component, kept with DontAdvance, and set
    // again with the action. HarfBuzz never pushes one index twice, so
    // the action pops glyph 1 and glyph 0 and forms the ligature. With
    // the double push it popped glyph 1 twice, put the ligature there,
    // and then removed it as the duplicate: [5, 6] came out as [5].
    let entries = [
        lig_entry_at(0, 0, 0),
        lig_entry_at(1, PUSH, 0),
        lig_entry_at(2, PUSH | STAY, 0),
        lig_entry_at(0, PUSH | ACT, 0),
    ];
    let states = [[0, 1, 0, 0], [0, 2, 0, 0], [0, 3, 0, 0]];
    // comp[6] + comp[5] = 3: ligatures[3].
    let bytes = ligature_morx(&states, &entries, &[0, LS], &[(6, 3)], &[0, 0, 0, 99]);
    let m = Morx::parse(&bytes).unwrap();
    assert_eq!(m.apply(&[5, 6]), (vec![99], vec![0]));
    assert_eq!(m.apply(&[5, 6, 5, 6]), (vec![99, 99], vec![0, 2]));
}

#[test]
fn ligature_stays_on_the_stack_for_the_next_action() {
    // [5, 6] forms ligature 90 at position 0, which stays on the stack.
    // Glyph 7 then joins it into 91. The stack used to lose the
    // ligature, so the second action ran out of components.
    let entries = [
        lig_entry_at(0, 0, 0),
        lig_entry_at(1, PUSH, 0),
        lig_entry_at(2, PUSH | ACT, 0),
        lig_entry_at(0, PUSH | ACT, 2),
    ];
    let states = [[0, 1, 0, 0], [0, 2, 0, 0], [0, 3, 0, 0]];
    // Action 0: comp[6] + comp[5] = 0. Action 2 (offset 0x64 = 100):
    // comp[107] + comp[190] = 1.
    let actions = [0, LS, 0x64, LS | 0x64];
    let bytes = ligature_morx(&states, &entries, &actions, &[(107, 1)], &[90, 91]);
    let m = Morx::parse(&bytes).unwrap();
    assert_eq!(m.apply(&[5, 6, 7]), (vec![91], vec![0]));
}

#[test]
fn ligature_store_without_last_and_running_index() {
    // The first action stores (without Last) ligature 2 in place of
    // the glyph it popped; the second (Last, no Store) adds to the same
    // index and stores ligature 4 over glyph 0, deleting the first
    // ligature. Store used to count only together with Last.
    let entries = [
        lig_entry_at(0, 0, 0),
        lig_entry_at(1, PUSH, 0),
        lig_entry_at(0, PUSH | ACT, 0),
    ];
    let states = [[0, 1, 0, 0], [0, 2, 0, 0]];
    let actions = [LIG_ACTION_STORE, LIG_ACTION_LAST];
    let bytes = ligature_morx(&states, &entries, &actions, &[(5, 2)], &[0, 0, 70, 0, 71]);
    let m = Morx::parse(&bytes).unwrap();
    assert_eq!(m.apply(&[5, 5]).0, [71]);
}

#[test]
fn ligature_action_at_end_of_text_does_nothing() {
    // State 2's end-of-text entry performs the action. HarfBuzz acts
    // only on a glyph, so [5, 6] stays as it is.
    let entries = [
        lig_entry_at(0, 0, 0),
        lig_entry_at(1, PUSH, 0),
        lig_entry_at(2, PUSH, 0),
        lig_entry_at(0, ACT, 0),
    ];
    let states = [[0, 1, 0, 0], [0, 2, 0, 0], [3, 0, 0, 0]];
    let bytes = ligature_morx(&states, &entries, &[0, LS], &[], &[99]);
    let m = Morx::parse(&bytes).unwrap();
    assert_eq!(m.apply(&[5, 6]).0, [5, 6]);
}

#[test]
fn ligature_stack_underflow_clears_the_stack() {
    // The action list wants three components but the stack holds two:
    // nothing forms and the stack is cleared, so the next action, on
    // two new components, underflows too instead of reaching back to
    // the first two.
    let entries = [
        lig_entry_at(0, 0, 0),
        lig_entry_at(1, PUSH, 0),
        lig_entry_at(0, PUSH | ACT, 0),
    ];
    let states = [[0, 1, 0, 0], [0, 2, 0, 0]];
    let bytes = ligature_morx(&states, &entries, &[0, 0, LS], &[], &[99]);
    let m = Morx::parse(&bytes).unwrap();
    assert_eq!(m.apply(&[5, 6, 7, 8]).0, [5, 6, 7, 8]);
}

#[test]
fn deleted_components_stay_until_morx_is_done() {
    // A ligature deletes glyph 1. The contextual subtable after it
    // still sees that glyph, in the deleted-glyph class (2), and marks
    // the ligature for substitution there; morx drops the deleted
    // glyph only at the end, as HarfBuzz does.
    let lig_entries = [
        lig_entry_at(0, 0, 0),
        lig_entry_at(1, PUSH, 0),
        lig_entry_at(0, PUSH | ACT, 0),
    ];
    let lig_refs: Vec<&[u8]> = lig_entries.iter().map(Vec::as_slice).collect();
    let actions: Vec<u8> = [0, LS].iter().flat_map(|a: &u32| a.to_be_bytes()).collect();
    let comps = [0u8; 32];
    let lig = state_body(
        3,
        &[[0, 1, 0, 0], [0, 2, 0, 0]],
        &lig_refs,
        &[&actions, &comps, &90u16.to_be_bytes()],
    );
    let ctx_entries = [
        ctx_entry(0, FLAG_CTX_SET_MARK, 0xFFFF, 0xFFFF),
        ctx_entry(0, 0, 0, 0xFFFF),
    ];
    let ctx_refs: Vec<&[u8]> = ctx_entries.iter().map(Vec::as_slice).collect();
    let table = substitution_table(&[build_lookup_format6(&[(90, 77)])]);
    // Out-of-bounds glyphs set the mark; a deleted glyph substitutes it.
    let ctx = state_body(1, &[[0, 0, 1, 0]], &ctx_refs, &[&table]);
    let bytes = wrap_in_chain(&[
        &subtable(TYPE_LIGATURE, &lig),
        &subtable(TYPE_CONTEXTUAL, &ctx),
    ]);
    let m = Morx::parse(&bytes).unwrap();
    assert_eq!(m.apply(&[5, 6]), (vec![77], vec![0]));
}

#[test]
fn substitutions_to_glyph_1_apply() {
    // Glyph 1 is the same number as the out-of-bounds class, which the
    // lookup reader used to return for "not covered", so a
    // substitution to glyph 1 was dropped.
    let bytes = build_non_contextual_morx(&[(5, 1)]);
    assert_eq!(Morx::parse(&bytes).unwrap().apply(&[5, 6]).0, [1, 6]);
    let lookups = [build_lookup_format6(&[(5, 1)])];
    let entries = [ctx_entry(0, 0, 0xFFFF, 0)];
    let bytes = contextual_morx(&[[0, 0, 0, 0]], &entries, &lookups);
    assert_eq!(Morx::parse(&bytes).unwrap().apply(&[5, 6]).0, [1, 6]);
}
