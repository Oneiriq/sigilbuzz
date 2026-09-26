//! Tests for `kerx` format 4 (control-point kerning).

use super::*;
use crate::tables::kerx::format4::ACTION_INDEX_NONE;

// -----------------------------------------------------------------
// Format 4: control-point kerning.
//
// Parse-coverage tests assert that a format-4 subtable parses
// cleanly and its presence does not corrupt the surrounding
// kerx (zero pair kerns, no consumption of subsequent fmt 0
// subtables). The apply-coverage tests below drive a synthetic
// state machine through "AB" and assert one [`Kerx4Action`]
// event fires per matched pair with the expected indices and
// anchor record.
// -----------------------------------------------------------------

/// Builds a one-subtable kerx with format 4 wired with an empty
/// state table (one state, two classes, single noop entry) and
/// the action-type-2 (coordinates) flag. The inner action table
/// holds one record of four zeros, enough to validate parsing
/// without driving any glyph offset.
fn build_kerx_format4(action_type: u8) -> Vec<u8> {
    // Format 4 body layout:
    //   0..16   state-table header (nClasses, classOff, stateOff,
    //                                entryOff)
    //  16..20   flags (action_type << 30 | action_off)
    //  20..     class lookup (format 6, empty)
    //  ..       state array (1 state * 4 classes * u16) = 8 B
    //  ..       entry array (1 entry * 6 B)
    //  ..       action table (one 8-B record)
    let n_classes: u32 = 4; // four reserved classes is the AAT minimum
    let header_len = 20;

    // Empty class lookup (format 6, zero entries).
    let mut class_lookup: Vec<u8> = Vec::new();
    class_lookup.extend_from_slice(&6u16.to_be_bytes());
    class_lookup.extend_from_slice(&4u16.to_be_bytes()); // unitSize
    class_lookup.extend_from_slice(&0u16.to_be_bytes()); // nUnits
    class_lookup.extend_from_slice(&[0u8; 6]); // search hints

    let class_off = header_len;
    let class_end = class_off + class_lookup.len();
    let state_off = class_end + (class_end % 2);
    let state_bytes = n_classes as usize * 2;
    let entry_off = state_off + state_bytes;
    let entry_bytes = 6;
    let action_off = entry_off + entry_bytes;
    let action_bytes = 8;

    let body_len = action_off + action_bytes;

    let mut body: Vec<u8> = Vec::with_capacity(body_len);
    // State-table header.
    body.extend_from_slice(&n_classes.to_be_bytes());
    body.extend_from_slice(&(class_off as u32).to_be_bytes());
    body.extend_from_slice(&(state_off as u32).to_be_bytes());
    body.extend_from_slice(&(entry_off as u32).to_be_bytes());
    // Flags: action_type in bits 30-31, action_off in low 30.
    let flags: u32 = (u32::from(action_type) << 30) | (action_off as u32);
    body.extend_from_slice(&flags.to_be_bytes());
    body.extend_from_slice(&class_lookup);
    if body.len() < state_off {
        body.resize(state_off, 0);
    }
    // State row: every cell points at entry 0 (noop).
    for _ in 0..n_classes {
        body.extend_from_slice(&0u16.to_be_bytes());
    }
    // Entry 0: noop.
    body.extend_from_slice(&0u16.to_be_bytes()); // newState
    body.extend_from_slice(&0u16.to_be_bytes()); // flags
    body.extend_from_slice(&0u16.to_be_bytes()); // actionIndex
                                                 // Action record: 8 bytes of zero (four i16s for the
                                                 // coordinates variant. For control-points / anchors the
                                                 // shape happens to overlap, so the same fill works).
    body.extend_from_slice(&[0u8; 8]);

    // Wrap in 12-byte common header + 8-byte kerx table header.
    let sub_len = 12 + body.len();
    let mut bytes: Vec<u8> = Vec::new();
    bytes.extend_from_slice(&2u16.to_be_bytes()); // version
    bytes.extend_from_slice(&0u16.to_be_bytes()); // pad
    bytes.extend_from_slice(&1u32.to_be_bytes()); // nTables
    bytes.extend_from_slice(&(sub_len as u32).to_be_bytes());
    bytes.extend_from_slice(&4u32.to_be_bytes()); // coverage: format 4
    bytes.extend_from_slice(&0u32.to_be_bytes()); // tupleCount
    bytes.extend_from_slice(&body);
    bytes
}

#[test]
fn format4_parses_with_coordinates_action_type() {
    // Action type 2 = coordinates (inline FUnit deltas). Parses
    // cleanly and the subtable is retained.
    let bytes = build_kerx_format4(2);
    let k = Kerx::parse(&bytes, 8).unwrap();
    assert_eq!(k.subtable_count(), 1, "format 4 subtable retained");
    // Format 4 produces no pair kerns. It's stateful and its
    // apply path is deferred.
    assert_eq!(k.kern(1, 2), 0);
}

#[test]
fn format4_parses_with_control_points_action_type() {
    let bytes = build_kerx_format4(0);
    let k = Kerx::parse(&bytes, 8).unwrap();
    assert_eq!(k.subtable_count(), 1);
    assert_eq!(k.kern(1, 2), 0);
}

/// Builds a kerx with one fmt-4 subtable wired with a real
/// state machine: class 4 = "A", class 5 = "B". State 0 entry
/// for class 4 marks the glyph (FLAG_F4_MARK) and goes to state
/// 1; state 1 entry for class 5 fires action 0 and resets.
/// `action_type` is stamped into bits 30-31 of the format-4 flags
/// word. `action_records` is the raw bytes for the action table.
fn build_kerx_format4_with_action(
    a_gid: u16,
    b_gid: u16,
    action_type: u8,
    action_records: &[u8],
) -> Vec<u8> {
    // Class lookup (format 6) maps A to class 4, B to class 5.
    let mut sorted = [(a_gid, 4u16), (b_gid, 5u16)];
    sorted.sort_by_key(|p| p.0);
    let class_lookup = build_lookup_format6(&sorted);

    let n_classes: u32 = 6; // 0..=3 reserved + A class 4 + B class 5
    let n_states: u32 = 2;
    let n_entries: usize = 4;

    let header_len = 20;
    let class_off = header_len;
    let class_end = class_off + class_lookup.len();
    let state_off = class_end + (class_end % 2);
    let state_bytes = (n_states * n_classes) as usize * 2;
    let entry_off = state_off + state_bytes;
    let entry_bytes = n_entries * 6;
    let action_off = entry_off + entry_bytes;
    let action_bytes = action_records.len();
    let body_len = action_off + action_bytes;

    let mut body: Vec<u8> = Vec::with_capacity(body_len);
    // State-table header.
    body.extend_from_slice(&n_classes.to_be_bytes());
    body.extend_from_slice(&(class_off as u32).to_be_bytes());
    body.extend_from_slice(&(state_off as u32).to_be_bytes());
    body.extend_from_slice(&(entry_off as u32).to_be_bytes());
    // Flags: action_type in bits 30-31, action_off (relative to
    // body start) in low 30.
    let flags: u32 = (u32::from(action_type) << 30) | (action_off as u32);
    body.extend_from_slice(&flags.to_be_bytes());
    body.extend_from_slice(&class_lookup);
    if body.len() < state_off {
        body.resize(state_off, 0);
    }
    // State 0: only class 4 (A) is interesting -> entry 1 (mark, ->s1).
    // Other classes -> entry 0 (noop).
    // State 1: only class 5 (B) is interesting -> entry 2 (action, ->s0).
    // Class 4 (A) -> entry 3 (mark, stay s1, handles AAB).
    // Other classes -> entry 0.
    let s0: [u16; 6] = [0, 0, 0, 0, 1, 0];
    let s1: [u16; 6] = [0, 0, 0, 0, 3, 2];
    for v in s0.iter().chain(s1.iter()) {
        body.extend_from_slice(&v.to_be_bytes());
    }
    // Entries (newState, flags, actionIndex).
    let mark: u16 = 0x8000;
    let entries: [(u16, u16, u16); 4] = [
        (0, 0, ACTION_INDEX_NONE),    // #0 noop
        (1, mark, ACTION_INDEX_NONE), // #1 mark A, -> state 1
        (0, 0, 0),                    // #2 fire action 0, -> state 0
        (1, mark, ACTION_INDEX_NONE), // #3 re-mark A, stay in s1
    ];
    for (ns, fl, ai) in entries {
        body.extend_from_slice(&ns.to_be_bytes());
        body.extend_from_slice(&fl.to_be_bytes());
        body.extend_from_slice(&ai.to_be_bytes());
    }
    body.extend_from_slice(action_records);

    let sub_len = 12 + body.len();
    let mut bytes: Vec<u8> = Vec::new();
    bytes.extend_from_slice(&2u16.to_be_bytes());
    bytes.extend_from_slice(&0u16.to_be_bytes());
    bytes.extend_from_slice(&1u32.to_be_bytes());
    bytes.extend_from_slice(&(sub_len as u32).to_be_bytes());
    bytes.extend_from_slice(&4u32.to_be_bytes()); // coverage: format 4
    bytes.extend_from_slice(&0u32.to_be_bytes()); // tupleCount
    bytes.extend_from_slice(&body);
    bytes
}

#[test]
fn format4_apply_action_type_2_emits_inline_coords() {
    // Action type 2: inline coordinates. Build a single record
    // with mark anchor at (100, 0) and current anchor at (50, 0);
    // running "AB" should fire one event with mark_index=0 and
    // current_index=1, and the coords reported back unchanged.
    let mut action: Vec<u8> = Vec::new();
    action.extend_from_slice(&100i16.to_be_bytes()); // mark_x
    action.extend_from_slice(&0i16.to_be_bytes()); // mark_y
    action.extend_from_slice(&50i16.to_be_bytes()); // current_x
    action.extend_from_slice(&0i16.to_be_bytes()); // current_y
    let bytes = build_kerx_format4_with_action(1, 2, 2, &action);
    let k = Kerx::parse(&bytes, 8).unwrap();
    assert!(k.has_format4());
    let mut events: Vec<Kerx4Action> = Vec::new();
    k.apply_format4(&[1, 2], |evt| events.push(evt));
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0],
        Kerx4Action::Coordinates {
            mark_index: 0,
            current_index: 1,
            mark_x: 100,
            mark_y: 0,
            current_x: 50,
            current_y: 0,
        }
    );
}

#[test]
fn format4_apply_action_type_0_emits_control_points() {
    // Action type 0: control points. One record: mark_point=3,
    // current_point=7. Run "AB": one event with both points and
    // the run indices.
    let mut action: Vec<u8> = Vec::new();
    action.extend_from_slice(&3u16.to_be_bytes()); // mark_point
    action.extend_from_slice(&7u16.to_be_bytes()); // current_point
    let bytes = build_kerx_format4_with_action(1, 2, 0, &action);
    let k = Kerx::parse(&bytes, 8).unwrap();
    let mut events: Vec<Kerx4Action> = Vec::new();
    k.apply_format4(&[1, 2], |evt| events.push(evt));
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0],
        Kerx4Action::ControlPoints {
            mark_index: 0,
            current_index: 1,
            mark_point: 3,
            current_point: 7,
        }
    );
}

#[test]
fn format4_apply_skips_when_no_action() {
    // Run with no marked-then-action sequence ("BB"): the state
    // machine never advances past state 0 for class B (entry 0 =
    // noop), so no event fires.
    let mut action: Vec<u8> = Vec::new();
    action.extend_from_slice(&[0u8; 8]);
    let bytes = build_kerx_format4_with_action(1, 2, 2, &action);
    let k = Kerx::parse(&bytes, 8).unwrap();
    let mut events: Vec<Kerx4Action> = Vec::new();
    k.apply_format4(&[2, 2, 2], |evt| events.push(evt));
    assert!(events.is_empty(), "no AB pattern -> no kern");
}

#[test]
fn format4_apply_action_type_1_emits_anchor_indices() {
    // Action type 1: anchor points (ankr lookup indices). One
    // record: mark_anchor=2, current_anchor=5.
    let mut action: Vec<u8> = Vec::new();
    action.extend_from_slice(&2u16.to_be_bytes()); // mark_anchor
    action.extend_from_slice(&5u16.to_be_bytes()); // current_anchor
    let bytes = build_kerx_format4_with_action(1, 2, 1, &action);
    let k = Kerx::parse(&bytes, 8).unwrap();
    let mut events: Vec<Kerx4Action> = Vec::new();
    k.apply_format4(&[1, 2], |evt| events.push(evt));
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0],
        Kerx4Action::AnchorPoints {
            mark_index: 0,
            current_index: 1,
            mark_anchor: 2,
            current_anchor: 5,
        }
    );
}

#[test]
fn format4_apply_repeated_pair_fires_each_time() {
    // "ABAB": two AB pairs, each should emit one event.
    let mut action: Vec<u8> = Vec::new();
    action.extend_from_slice(&10i16.to_be_bytes());
    action.extend_from_slice(&0i16.to_be_bytes());
    action.extend_from_slice(&5i16.to_be_bytes());
    action.extend_from_slice(&0i16.to_be_bytes());
    let bytes = build_kerx_format4_with_action(1, 2, 2, &action);
    let k = Kerx::parse(&bytes, 8).unwrap();
    let mut events: Vec<Kerx4Action> = Vec::new();
    k.apply_format4(&[1, 2, 1, 2], |evt| events.push(evt));
    assert_eq!(events.len(), 2);
    if let Kerx4Action::Coordinates {
        mark_index,
        current_index,
        ..
    } = events[0]
    {
        assert_eq!((mark_index, current_index), (0, 1));
    } else {
        panic!("first event wrong variant");
    }
    if let Kerx4Action::Coordinates {
        mark_index,
        current_index,
        ..
    } = events[1]
    {
        assert_eq!((mark_index, current_index), (2, 3));
    } else {
        panic!("second event wrong variant");
    }
}

#[test]
fn format4_does_not_drop_following_subtables() {
    // Mixed kerx: format 4 first, then format 0 with one pair.
    // The format-4 subtable parses-but-emits-nothing path must
    // not interfere with the format-0 lookup. This guards the
    // "deferred apply path" promise from the module docs.
    let f4 = build_kerx_format4(2);
    // f4 layout: 8 B header + 1 subtable. Strip the kerx header
    // and re-emit with two subtables.
    let f4_sub = &f4[8..];

    // Build a tiny format-0 subtable directly.
    let pairs: &[(u16, u16, i16)] = &[(10, 20, -42)];
    let pair_bytes = pairs.len() * 6;
    let f0_body_len = 16 + pair_bytes;
    let f0_sub_len = 12 + f0_body_len;
    let mut f0_sub: Vec<u8> = Vec::new();
    f0_sub.extend_from_slice(&(f0_sub_len as u32).to_be_bytes());
    f0_sub.extend_from_slice(&0u32.to_be_bytes()); // coverage: fmt 0
    f0_sub.extend_from_slice(&0u32.to_be_bytes()); // tupleCount
    f0_sub.extend_from_slice(&(pairs.len() as u32).to_be_bytes());
    f0_sub.extend_from_slice(&[0u8; 12]);
    for (l, r, v) in pairs {
        f0_sub.extend_from_slice(&l.to_be_bytes());
        f0_sub.extend_from_slice(&r.to_be_bytes());
        f0_sub.extend_from_slice(&v.to_be_bytes());
    }

    let mut bytes: Vec<u8> = Vec::new();
    bytes.extend_from_slice(&2u16.to_be_bytes()); // version
    bytes.extend_from_slice(&0u16.to_be_bytes()); // pad
    bytes.extend_from_slice(&2u32.to_be_bytes()); // nTables = 2
    bytes.extend_from_slice(f4_sub);
    bytes.extend_from_slice(&f0_sub);

    let k = Kerx::parse(&bytes, 256).expect("kerx with mixed fmt4+fmt0 parses");
    assert_eq!(k.subtable_count(), 2);
    assert_eq!(
        k.kern(10, 20),
        -42,
        "format 0 pair survives the format-4 neighbour"
    );
}
