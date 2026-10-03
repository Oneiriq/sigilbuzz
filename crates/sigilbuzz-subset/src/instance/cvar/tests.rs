use alloc::vec;
use alloc::vec::Vec;

use super::*;

/// A `cvt ` of `values`.
fn cvt_of(values: &[i16]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_be_bytes()).collect()
}

/// The values of a `cvt `.
fn values_of(cvt: &[u8]) -> Vec<i16> {
    cvt.chunks_exact(2)
        .map(|p| i16::from_be_bytes([p[0], p[1]]))
        .collect()
}

/// Packed point numbers for small, ascending `points`.
fn points(points: &[u16]) -> Vec<u8> {
    let mut out = vec![points.len() as u8];
    if !points.is_empty() {
        out.push((points.len() - 1) as u8);
        let mut last = 0;
        for &p in points {
            out.push((p - last) as u8);
            last = p;
        }
    }
    out
}

/// Packed word deltas.
fn deltas(values: &[i16]) -> Vec<u8> {
    let mut out = vec![0x40 | (values.len() - 1) as u8];
    for v in values {
        out.extend_from_slice(&v.to_be_bytes());
    }
    out
}

/// One tuple: its peak, its own points (`None` reads the shared ones,
/// or every value), and its deltas.
struct T<'a>(&'a [f32], Option<&'a [u16]>, &'a [i16]);

/// A `cvar` of `tuples`, with `shared` point numbers when given.
fn cvar_of(tuples: &[T<'_>], shared: Option<&[u16]>) -> Vec<u8> {
    let headers_len: usize = tuples.iter().map(|t| 4 + 2 * t.0.len()).sum();
    let mut count = tuples.len() as u16;
    if shared.is_some() {
        count |= 0x8000;
    }
    let mut out = vec![0, 1, 0, 0];
    out.extend_from_slice(&count.to_be_bytes());
    out.extend_from_slice(&(8 + headers_len as u16).to_be_bytes());
    let mut data = shared.map(points).unwrap_or_default();
    for T(peak, own, values) in tuples {
        let mut payload = own.map(points).unwrap_or_default();
        payload.extend(deltas(values));
        let mut index = 0x8000u16;
        if own.is_some() {
            index |= 0x2000;
        }
        out.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        out.extend_from_slice(&index.to_be_bytes());
        for &p in *peak {
            out.extend_from_slice(&((p * 16384.0) as i16).to_be_bytes());
        }
        data.extend(payload);
    }
    out.extend(data);
    out
}

#[test]
fn a_full_instance_adds_the_deltas_and_drops_the_table() {
    let cvt = cvt_of(&[100, 200, 300, 400]);
    let cvar = cvar_of(
        &[
            T(&[1.0], None, &[10, 0, -5, 3]),
            T(&[-1.0], Some(&[1]), &[7]),
        ],
        None,
    );
    // Halfway up: half of the first tuple, -2.5 rounding up to -2.
    let bake = rebuild(&cvt, &cvar, &[0.5], &[AxisPin::Pin]).unwrap();
    assert_eq!(values_of(&bake.cvt.unwrap()), [105, 200, 298, 402]);
    assert_eq!(bake.cvar, None);
    // A quarter down: a quarter of the second, 1.75 rounding to 2.
    let bake = rebuild(&cvt, &cvar, &[-0.25], &[AxisPin::Pin]).unwrap();
    assert_eq!(values_of(&bake.cvt.unwrap()), [100, 202, 300, 400]);
}

#[test]
fn tuples_read_the_shared_point_numbers() {
    let cvt = cvt_of(&[100, 200, 300]);
    let cvar = cvar_of(&[T(&[1.0], None, &[4, 6])], Some(&[0, 2]));
    let bake = rebuild(&cvt, &cvar, &[1.0], &[AxisPin::Pin]).unwrap();
    assert_eq!(values_of(&bake.cvt.unwrap()), [104, 200, 306]);
}

#[test]
fn a_partial_instance_folds_the_pinned_axes_and_merges_what_matches() {
    let cvt = cvt_of(&[100, 200, 300]);
    let tuples = [
        // On the pinned axis only: into cvt.
        T(&[1.0, 0.0], None, &[10, 20, 30]),
        // On the kept axis only: stays.
        T(&[0.0, 1.0], Some(&[0]), &[8]),
        // On both: half of it, merged with the one above.
        T(&[1.0, 1.0], Some(&[0, 2]), &[4, -6]),
        // Off at the pinned coordinate: gone.
        T(&[-1.0, 1.0], None, &[50, 50, 50]),
    ];
    let cvar = cvar_of(&tuples, None);
    let pins = [AxisPin::Pin, AxisPin::Keep];
    let bake = rebuild(&cvt, &cvar, &[0.5, 0.0], &pins).unwrap();
    let new_cvt = bake.cvt.unwrap();
    let new_cvar = bake.cvar.unwrap();
    assert_eq!(values_of(&new_cvt), [105, 210, 315]);
    // One tuple left, peaking at 1 on the kept axis.
    assert_eq!(&new_cvar[4..6], &[0, 1]);
    assert_eq!(&new_cvar[10..14], &[0xA0, 0x00, 0x40, 0x00]);
    // At the kept axis' peak, the instance of the partial instance is
    // the instance of the source.
    let partial_then_full = rebuild(&new_cvt, &new_cvar, &[1.0], &[AxisPin::Pin]).unwrap();
    let full = rebuild(&cvt, &cvar, &[0.5, 1.0], &[AxisPin::Pin; 2]).unwrap();
    assert_eq!(values_of(&full.cvt.unwrap()), [115, 210, 312]);
    assert_eq!(values_of(&partial_then_full.cvt.unwrap()), [115, 210, 312]);
}

#[test]
fn a_tuple_that_moves_nothing_leaves_no_cvar() {
    let cvt = cvt_of(&[100]);
    let cvar = cvar_of(&[T(&[1.0, 1.0], None, &[1])], None);
    let pins = [AxisPin::Pin, AxisPin::Keep];
    // A quarter of one unit rounds to nothing.
    let bake = rebuild(&cvt, &cvar, &[0.25, 0.0], &pins).unwrap();
    assert_eq!(bake.cvar, None);
    assert_eq!(values_of(&bake.cvt.unwrap()), [100]);
}

#[test]
fn a_malformed_cvar_is_an_error() {
    let cvt = cvt_of(&[100, 200]);
    let mut cvar = cvar_of(&[T(&[1.0], None, &[1, 2])], None);
    let pins = [AxisPin::Pin];
    let mut version_2 = cvar.clone();
    version_2[1] = 2;
    assert!(rebuild(&cvt, &version_2, &[1.0], &pins).is_err());
    // Two deltas for two values, the second cut off.
    cvar.truncate(cvar.len() - 2);
    assert_eq!(rebuild(&cvt, &cvar, &[1.0], &pins), Err(MALFORMED));
    // A data offset past the end.
    let mut far = cvar_of(&[T(&[1.0], None, &[1, 2])], None);
    far[6..8].copy_from_slice(&0xFFF0u16.to_be_bytes());
    assert_eq!(rebuild(&cvt, &far, &[1.0], &pins), Err(MALFORMED));
}

#[test]
fn many_or_far_points_pack_as_words_or_every_value() {
    // 200 points 300 apart: a two-byte count and word runs.
    let rounded: Vec<(usize, i32)> = (0..200).map(|i| (i * 300, i as i32 - 100)).collect();
    let payload = sparse_payload(&rounded).unwrap();
    let used = packed_point_numbers_byte_len(&payload).unwrap();
    let listed = parse_packed_point_numbers(&payload).unwrap();
    let expected: Vec<u16> = rounded.iter().map(|&(i, _)| i as u16).collect();
    assert_eq!(listed, expected);
    let (read, _) = read_packed_deltas_n(&payload[used..], 200).unwrap();
    assert_eq!(read, rounded.iter().map(|&(_, d)| d).collect::<Vec<_>>());
    // An index past what a point number holds packs only as every value.
    let far = [(0, 1), (70_000, -1)];
    assert_eq!(sparse_payload(&far), None);
    let dense = dense_payload(&far, 70_001).unwrap();
    assert_eq!(dense[0], 0);
    let (read, _) = read_packed_deltas_n(&dense[1..], 70_001).unwrap();
    assert_eq!(
        (read[0], read[70_000], read.iter().sum::<i32>()),
        (1, -1, 0)
    );
}

#[test]
fn bake_cvt_reports_a_cvar_it_cannot_read_and_keeps_the_cvt() {
    let cvt = cvt_of(&[100]);
    let font = crate::sfnt::build(
        0x0001_0000,
        &[(CVT, cvt.clone()), (CVAR, vec![0, 1, 0, 0, 0, 1, 0])],
    );
    let face = Face::parse_bytes(&font, 0).unwrap();
    let warnings = Warnings::default();
    assert_eq!(
        bake_cvt(&face, &[1.0], &[], &warnings),
        Some(CvtBake::default())
    );
    let warnings = warnings.into_sorted();
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].table, CVAR);
    // No cvar: nothing to bake. A cvar without a cvt: left out.
    let font = crate::sfnt::build(0x0001_0000, &[(CVT, cvt)]);
    let face = Face::parse_bytes(&font, 0).unwrap();
    assert_eq!(bake_cvt(&face, &[1.0], &[], &Warnings::default()), None);
    let font = crate::sfnt::build(0x0001_0000, &[(CVAR, cvar_of(&[], None))]);
    let face = Face::parse_bytes(&font, 0).unwrap();
    assert_eq!(
        bake_cvt(&face, &[1.0], &[], &Warnings::default()),
        Some(CvtBake::default())
    );
}
