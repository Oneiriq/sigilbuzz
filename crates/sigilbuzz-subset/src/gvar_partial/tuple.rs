//! Tuple variation header parsing plus the packed point number and
//! packed delta codecs used by the partial `gvar` rewrite.

use alloc::vec::Vec;

use super::{
    DELTA_ALL_ZERO, DELTA_COUNT_MASK, DELTA_WORDS, FLAG_EMBEDDED_PEAK, FLAG_INTERMEDIATE_REGION,
    FLAG_PRIVATE_POINT_NUMBERS,
};
use crate::SubsetError;

#[derive(Debug, Clone)]
pub(super) struct ParsedTupleHeader {
    pub(super) variation_data_size: u16,
    pub(super) tuple_index: u16,
    pub(super) embedded_peak: Option<Vec<f32>>,
    pub(super) intermediate_start: Option<Vec<f32>>,
    pub(super) intermediate_end: Option<Vec<f32>>,
    pub(super) private_point_numbers: bool,
}

pub(super) fn parse_tuple_header(
    data: &[u8],
    axis_count: u16,
) -> Result<(ParsedTupleHeader, usize), SubsetError> {
    if data.len() < 4 {
        return Err(SubsetError::Unsupported("gvar partial: tuple header"));
    }
    let variation_data_size = u16::from_be_bytes([data[0], data[1]]);
    let tuple_index = u16::from_be_bytes([data[2], data[3]]);
    let mut cursor = 4usize;

    let embedded_peak = if tuple_index & FLAG_EMBEDDED_PEAK != 0 {
        let mut v = Vec::with_capacity(axis_count as usize);
        for _ in 0..axis_count {
            if cursor + 2 > data.len() {
                return Err(SubsetError::Unsupported("gvar partial: peak tuple"));
            }
            let raw = i16::from_be_bytes([data[cursor], data[cursor + 1]]);
            cursor += 2;
            v.push(f32::from(raw) / 16384.0);
        }
        Some(v)
    } else {
        None
    };
    let (intermediate_start, intermediate_end) = if tuple_index & FLAG_INTERMEDIATE_REGION != 0 {
        let mut s = Vec::with_capacity(axis_count as usize);
        for _ in 0..axis_count {
            if cursor + 2 > data.len() {
                return Err(SubsetError::Unsupported("gvar partial: intermediate start"));
            }
            let raw = i16::from_be_bytes([data[cursor], data[cursor + 1]]);
            cursor += 2;
            s.push(f32::from(raw) / 16384.0);
        }
        let mut e = Vec::with_capacity(axis_count as usize);
        for _ in 0..axis_count {
            if cursor + 2 > data.len() {
                return Err(SubsetError::Unsupported("gvar partial: intermediate end"));
            }
            let raw = i16::from_be_bytes([data[cursor], data[cursor + 1]]);
            cursor += 2;
            e.push(f32::from(raw) / 16384.0);
        }
        (Some(s), Some(e))
    } else {
        (None, None)
    };
    let private_point_numbers = tuple_index & FLAG_PRIVATE_POINT_NUMBERS != 0;

    Ok((
        ParsedTupleHeader {
            variation_data_size,
            tuple_index,
            embedded_peak,
            intermediate_start,
            intermediate_end,
            private_point_numbers,
        },
        cursor,
    ))
}

pub(super) fn write_f2dot14(out: &mut Vec<u8>, v: f32) {
    let raw = (v * 16384.0)
        .round()
        .clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i16;
    out.extend_from_slice(&raw.to_be_bytes());
}

// ---------------------------------------------------------------------------
// Packed point numbers (parser only; we emit private/shared blocks
// verbatim).
// ---------------------------------------------------------------------------

pub(super) fn packed_point_numbers_byte_len(data: &[u8]) -> Result<usize, SubsetError> {
    if data.is_empty() {
        return Err(SubsetError::Unsupported("gvar partial: empty point block"));
    }
    let first = data[0];
    let (count, mut cursor) = if first & 0x80 == 0 {
        (u16::from(first), 1usize)
    } else {
        if data.len() < 2 {
            return Err(SubsetError::Unsupported(
                "gvar partial: missing second count byte",
            ));
        }
        let high = (u16::from(first) & 0x7F) << 8;
        (high | u16::from(data[1]), 2usize)
    };
    if count == 0 {
        // All-points shortcut.
        return Ok(cursor);
    }
    let mut emitted = 0usize;
    while emitted < count as usize {
        if cursor >= data.len() {
            return Err(SubsetError::Unsupported(
                "gvar partial: packed-points control",
            ));
        }
        let control = data[cursor];
        cursor += 1;
        let words = control & 0x80 != 0;
        let run = (control & 0x7F) as usize + 1;
        let remaining = count as usize - emitted;
        let take = run.min(remaining);
        let bytes_per = if words { 2 } else { 1 };
        let consume = take * bytes_per;
        if cursor + consume > data.len() {
            return Err(SubsetError::Unsupported(
                "gvar partial: packed-points payload",
            ));
        }
        cursor += consume;
        emitted += take;
    }
    Ok(cursor)
}

pub(super) fn parse_packed_point_numbers(data: &[u8]) -> Result<Vec<u16>, SubsetError> {
    if data.is_empty() {
        return Err(SubsetError::Unsupported("gvar partial: empty point block"));
    }
    let first = data[0];
    let (count, mut cursor) = if first & 0x80 == 0 {
        (u16::from(first), 1usize)
    } else {
        if data.len() < 2 {
            return Err(SubsetError::Unsupported(
                "gvar partial: missing second count byte",
            ));
        }
        let high = (u16::from(first) & 0x7F) << 8;
        (high | u16::from(data[1]), 2usize)
    };
    if count == 0 {
        return Ok(Vec::new());
    }
    let mut out: Vec<u16> = Vec::with_capacity(count as usize);
    let mut last: u32 = 0;
    while out.len() < count as usize {
        if cursor >= data.len() {
            return Err(SubsetError::Unsupported(
                "gvar partial: packed-points control",
            ));
        }
        let control = data[cursor];
        cursor += 1;
        let words = control & 0x80 != 0;
        let run = (control & 0x7F) as usize + 1;
        let remaining = count as usize - out.len();
        let take = run.min(remaining);
        for _ in 0..take {
            let delta: u32 = if words {
                if cursor + 2 > data.len() {
                    return Err(SubsetError::Unsupported("gvar partial: packed-points u16"));
                }
                let v = u16::from_be_bytes([data[cursor], data[cursor + 1]]);
                cursor += 2;
                u32::from(v)
            } else {
                if cursor >= data.len() {
                    return Err(SubsetError::Unsupported("gvar partial: packed-points u8"));
                }
                let v = data[cursor];
                cursor += 1;
                u32::from(v)
            };
            last = last.saturating_add(delta);
            out.push(last.min(u32::from(u16::MAX)) as u16);
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Packed deltas (parser + emitter).
// ---------------------------------------------------------------------------

pub(super) fn read_packed_deltas_n(
    data: &[u8],
    n: usize,
) -> Result<(Vec<i32>, usize), SubsetError> {
    let mut out = Vec::with_capacity(n);
    let mut cursor = 0usize;
    while out.len() < n {
        if cursor >= data.len() {
            return Err(SubsetError::Unsupported("gvar partial: deltas truncated"));
        }
        let control = data[cursor];
        cursor += 1;
        let run = (control & DELTA_COUNT_MASK) as usize + 1;
        let remaining = n - out.len();
        let take = run.min(remaining);
        if control & DELTA_ALL_ZERO != 0 {
            out.resize(out.len() + take, 0);
        } else if control & DELTA_WORDS != 0 {
            if cursor + take * 2 > data.len() {
                return Err(SubsetError::Unsupported("gvar partial: deltas i16"));
            }
            for _ in 0..take {
                let v = i16::from_be_bytes([data[cursor], data[cursor + 1]]);
                cursor += 2;
                out.push(i32::from(v));
            }
            let unused = run - take;
            if unused > 0 {
                if cursor + unused * 2 > data.len() {
                    return Err(SubsetError::Unsupported(
                        "gvar partial: deltas i16 unused tail",
                    ));
                }
                cursor += unused * 2;
            }
        } else {
            if cursor + take > data.len() {
                return Err(SubsetError::Unsupported("gvar partial: deltas i8"));
            }
            for _ in 0..take {
                let v = data[cursor] as i8;
                cursor += 1;
                out.push(i32::from(v));
            }
            let unused = run - take;
            if unused > 0 {
                if cursor + unused > data.len() {
                    return Err(SubsetError::Unsupported(
                        "gvar partial: deltas i8 unused tail",
                    ));
                }
                cursor += unused;
            }
        }
    }
    Ok((out, cursor))
}

/// Counts how many packed deltas live in `data` (consumes the whole
/// stream). Used to recover `n` for the all-points shortcut where
/// the count comes from the outline rather than a point list.
pub(super) fn count_packed_deltas(data: &[u8]) -> Result<usize, SubsetError> {
    let mut total = 0usize;
    let mut cursor = 0usize;
    // The all-points stream covers x then y deltas concatenated;
    // we only see "x stream + y stream" at the call site, but the
    // shape is: each stream covers exactly num_points values. The
    // count function computes the *total* values across both
    // streams and the caller divides by 2. The control bytes are
    // self-describing: we consume runs until the cursor is out
    // of bytes.
    while cursor < data.len() {
        let control = data[cursor];
        cursor += 1;
        let run = (control & DELTA_COUNT_MASK) as usize + 1;
        if control & DELTA_ALL_ZERO != 0 {
            total += run;
        } else if control & DELTA_WORDS != 0 {
            if cursor + run * 2 > data.len() {
                return Err(SubsetError::Unsupported("gvar partial: count deltas i16"));
            }
            cursor += run * 2;
            total += run;
        } else {
            if cursor + run > data.len() {
                return Err(SubsetError::Unsupported("gvar partial: count deltas i8"));
            }
            cursor += run;
            total += run;
        }
    }
    // Stream covers x then y deltas, same count each.
    if total % 2 != 0 {
        return Err(SubsetError::Unsupported(
            "gvar partial: all-points deltas not paired",
        ));
    }
    Ok(total / 2)
}

/// Encodes a slice of i32 deltas as a packed stream. Picks the
/// smallest run encoding per chunk: ALL_ZERO for runs of zeros,
/// i8 when every value fits in `[-128, 127]`, i16 otherwise. Each
/// run covers up to 64 values (the spec's `DELTA_COUNT_MASK + 1`).
pub(super) fn encode_packed_deltas(values: &[i32], out: &mut Vec<u8>) {
    let mut i = 0usize;
    while i < values.len() {
        let v = values[i];
        if v == 0 {
            // Zero run.
            let mut run = 1usize;
            while i + run < values.len() && values[i + run] == 0 && run < 64 {
                run += 1;
            }
            // Control byte: ALL_ZERO | (run - 1).
            let control: u8 = DELTA_ALL_ZERO | ((run - 1) as u8 & DELTA_COUNT_MASK);
            out.push(control);
            i += run;
        } else if (-128..=127).contains(&v) {
            // i8 run: collect as long as values fit and aren't zero
            // (zero runs are more compact via ALL_ZERO).
            let mut run = 1usize;
            while i + run < values.len()
                && values[i + run] != 0
                && (-128..=127).contains(&values[i + run])
                && run < 64
            {
                run += 1;
            }
            let control: u8 = (run - 1) as u8 & DELTA_COUNT_MASK; // i8 run
            out.push(control);
            for k in 0..run {
                let b = values[i + k] as i8 as u8;
                out.push(b);
            }
            i += run;
        } else {
            // i16 run: values that don't fit in i8.
            let mut run = 1usize;
            while i + run < values.len()
                && values[i + run] != 0
                && !(-128..=127).contains(&values[i + run])
                && run < 64
            {
                run += 1;
            }
            let control: u8 = DELTA_WORDS | ((run - 1) as u8 & DELTA_COUNT_MASK);
            out.push(control);
            for k in 0..run {
                let v = values[i + k];
                let clamped = v.clamp(i32::from(i16::MIN), i32::from(i16::MAX));
                let v16 = clamped as i16;
                out.extend_from_slice(&v16.to_be_bytes());
            }
            i += run;
        }
    }
}
