//! Tests for `kerx` format 1 (state-machine kerning).

use super::*;
use crate::tables::kerx::format1::VALUE_INDEX_NONE;

// -----------------------------------------------------------------
// Format 1: state-machine kerning.
// -----------------------------------------------------------------

/// Builds a one-subtable kerx with format 1 wired for a tiny
/// "kern A before V only when not after space" state machine.
/// The A glyph id, V glyph id and space glyph id are caller
/// inputs so the test can pick non-overlapping gids.
fn build_kerx_format1_av_after_letter(a_gid: u16, v_gid: u16, sp_gid: u16) -> Vec<u8> {
    // Class subtable (format 6): A->4, V->5, space->6. Sorted by
    // glyph id so the binary search keeps working regardless of
    // caller's choice of gids.
    let mut sorted = [(a_gid, 4u16), (v_gid, 5u16), (sp_gid, 6u16)];
    sorted.sort_by_key(|p| p.0);
    let class_lookup = build_lookup_format6(&sorted);

    // Format 1 body layout (everything offset from body start):
    //   0..16   state-table header
    //  16..20   valueTableOffset (u32)
    //  20..     class lookup (aligned to 2)
    //  ..       state array (nStates * nClasses * u16)
    //  ..       entry array (n_entries * 6)
    //  ..       value table
    let n_classes: u32 = 7;
    let n_states: u32 = 2;
    let n_entries: usize = 5;

    let header_len = 20;
    let class_off = header_len;
    let class_end = class_off + class_lookup.len();
    // 2-byte align state array.
    let state_off = class_end + (class_end % 2);
    let state_bytes = (n_states * n_classes) as usize * 2;
    let entry_off = state_off + state_bytes;
    let entry_bytes = n_entries * 6;
    let value_off = entry_off + entry_bytes;
    let value_bytes = 2usize; // single terminator-marked entry

    let body_len = value_off + value_bytes;

    let mut body: Vec<u8> = Vec::with_capacity(body_len);
    // --- State table header ---
    body.extend_from_slice(&n_classes.to_be_bytes());
    body.extend_from_slice(&(class_off as u32).to_be_bytes());
    body.extend_from_slice(&(state_off as u32).to_be_bytes());
    body.extend_from_slice(&(entry_off as u32).to_be_bytes());
    body.extend_from_slice(&(value_off as u32).to_be_bytes());
    // --- Class lookup ---
    body.extend_from_slice(&class_lookup);
    if body.len() < state_off {
        body.resize(state_off, 0);
    }
    // --- State array ---
    // State 0: cells per class.
    //   class 0..3 (reserved)  -> entry 0 (noop)
    //   class 4 (A)            -> entry 1 (push, stay state 0)
    //   class 5 (V)            -> entry 2 (apply value 0, stay state 0)
    //   class 6 (space)        -> entry 3 (no-op, go state 1)
    // State 1: cells per class.
    //   class 4 (A)            -> entry 4 (no-op, go state 0; suppresses push)
    //   class 5 (V)            -> entry 0 (noop, no V kern after solo space)
    //   class 6 (space)        -> entry 3 (stay state 1)
    let s0: [u16; 7] = [0, 0, 0, 0, 1, 2, 3];
    let s1: [u16; 7] = [0, 0, 0, 0, 4, 0, 3];
    for v in s0.iter().chain(s1.iter()) {
        body.extend_from_slice(&v.to_be_bytes());
    }
    // --- Entries (newState, flags, valueIndex) ---
    let push: u16 = 0x8000;
    let entries: [(u16, u16, u16); 5] = [
        (0, 0, VALUE_INDEX_NONE),    // #0 noop
        (0, push, VALUE_INDEX_NONE), // #1 push
        (0, 0, 0),                   // #2 apply value at offset 0
        (1, 0, VALUE_INDEX_NONE),    // #3 -> state 1
        (0, 0, VALUE_INDEX_NONE),    // #4 -> state 0 (clears stale A)
    ];
    for (ns, fl, vi) in entries {
        body.extend_from_slice(&ns.to_be_bytes());
        body.extend_from_slice(&fl.to_be_bytes());
        body.extend_from_slice(&vi.to_be_bytes());
    }
    // --- Value table: one i16 = -50 with terminator bit. ---
    // The spec says: value list terminated by an entry whose bit
    // 0 is set; the kern delta is the value with bit 0 cleared.
    // -50 is even (0xFFCE), so writing 0xFFCF keeps the magnitude
    // and adds the terminator. Reinterpret the bit pattern as i16.
    let raw = 0xFFCFu16 as i16;
    body.extend_from_slice(&raw.to_be_bytes());

    // Wrap in the 12-byte common subtable header + 8-byte kerx
    // table header.
    let sub_len = 12 + body.len();
    let mut bytes: Vec<u8> = Vec::new();
    bytes.extend_from_slice(&2u16.to_be_bytes()); // version
    bytes.extend_from_slice(&0u16.to_be_bytes()); // pad
    bytes.extend_from_slice(&1u32.to_be_bytes()); // nTables
    bytes.extend_from_slice(&(sub_len as u32).to_be_bytes());
    bytes.extend_from_slice(&1u32.to_be_bytes()); // coverage: format 1
    bytes.extend_from_slice(&0u32.to_be_bytes()); // tupleCount
    bytes.extend_from_slice(&body);
    bytes
}

/// Captures `(glyph_index, kern_delta)` callbacks during a
/// state-machine apply pass.
fn collect_kerns(k: &Kerx<'_>, ids: &[u16]) -> Vec<(usize, i16)> {
    let mut out = Vec::new();
    k.apply_state_machines(ids, |idx, delta| out.push((idx, delta)));
    out
}

#[test]
fn format1_parses_and_reports_state_machine() {
    let bytes = build_kerx_format1_av_after_letter(1, 2, 3);
    let k = Kerx::parse(&bytes, 8).unwrap();
    assert_eq!(k.subtable_count(), 1);
    assert!(k.has_state_machine());
    // No pair-list subtable: the legacy kern() lookup must
    // return zero so the apply path doesn't double-count.
    assert_eq!(k.kern(1, 2), 0);
    assert_eq!(k.subtable_pair_kern(0, 1, 2), None);
}

#[test]
fn format1_kerns_av_when_not_after_space() {
    // gid 1 = A, gid 2 = V. A contiguous AV pair should kern.
    let bytes = build_kerx_format1_av_after_letter(1, 2, 3);
    let k = Kerx::parse(&bytes, 8).unwrap();
    let kerns = collect_kerns(&k, &[1, 2]);
    assert_eq!(kerns, alloc::vec![(0, -50)]);
}

#[test]
fn format1_skips_av_after_space() {
    // gid 3 (space) before AV: the state machine's "after space"
    // state suppresses the A push, so no kern lands.
    let bytes = build_kerx_format1_av_after_letter(1, 2, 3);
    let k = Kerx::parse(&bytes, 8).unwrap();
    let kerns = collect_kerns(&k, &[3, 1, 2]);
    assert!(kerns.is_empty(), "no kern after a leading space");
}

#[test]
fn format1_kerns_repeated_av_pairs() {
    // "AVAV" should kern both pairs: the state machine is
    // designed to reset to state 0 after each V.
    let bytes = build_kerx_format1_av_after_letter(1, 2, 3);
    let k = Kerx::parse(&bytes, 8).unwrap();
    let kerns = collect_kerns(&k, &[1, 2, 1, 2]);
    assert_eq!(kerns, alloc::vec![(0, -50), (2, -50)]);
}

#[test]
fn format1_walk_terminates_on_corrupt_value_list() {
    // Build a working machine, then clobber the value-table byte
    // so bit 0 is *not* set. The consume_value_list cap should
    // bail before walking off the end.
    let mut bytes = build_kerx_format1_av_after_letter(1, 2, 3);
    // The value byte sits as the very last 2 bytes of the
    // table. Clear bit 0 so the list never terminates organically.
    let len = bytes.len();
    bytes[len - 1] &= !1;
    let k = Kerx::parse(&bytes, 8).unwrap();
    // Apply must still return without panicking; the kern that
    // *would* have been emitted may or may not land but the
    // shaper must not loop or crash.
    let _ = collect_kerns(&k, &[1, 2, 1, 2]);
}

#[test]
fn format1_handles_empty_input() {
    let bytes = build_kerx_format1_av_after_letter(1, 2, 3);
    let k = Kerx::parse(&bytes, 8).unwrap();
    let kerns = collect_kerns(&k, &[]);
    assert!(kerns.is_empty());
}

#[test]
fn format1_unknown_glyphs_do_not_kern() {
    // Glyph ids that aren't in the class table fall through to
    // the reserved out-of-bounds class, which always lands on
    // entry 0 (noop) in our table.
    let bytes = build_kerx_format1_av_after_letter(1, 2, 3);
    let k = Kerx::parse(&bytes, 8).unwrap();
    let kerns = collect_kerns(&k, &[99, 99, 99]);
    assert!(kerns.is_empty());
}
