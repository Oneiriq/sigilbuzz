//! Small SFNT helpers shared by the WOFF1 and WOFF2 unwrappers.

/// SFNT search-param triple (`searchRange`, `entrySelector`,
/// `rangeShift`). All deterministic functions of `num_tables`.
///
/// For 4096 or more tables the true values no longer fit the u16
/// header fields. They are only binary-search hints, so the low 16
/// bits are kept.
pub(crate) fn search_params(num_tables: u16) -> (u16, u16, u16) {
    if num_tables == 0 {
        return (0, 0, 0);
    }
    let n = u32::from(num_tables);
    // Largest power of two <= num_tables, scaled by record size 16.
    let entry_selector = n.ilog2();
    let search_range = (1u32 << entry_selector) * 16;
    let range_shift = n * 16 - search_range;
    (
        search_range as u16,
        entry_selector as u16,
        range_shift as u16,
    )
}

#[cfg(test)]
mod tests {
    use super::search_params;

    #[test]
    fn search_params_do_not_overflow_for_large_table_counts() {
        // 4095 is the largest count whose values fit u16. The larger
        // counts used to overflow u16 arithmetic, and 65535 tables
        // also spun forever in release builds.
        assert_eq!(search_params(4095), (32768, 11, 32752));
        assert_eq!(search_params(4096), (0, 12, 0));
        assert_eq!(search_params(32768), (0, 15, 0));
        assert_eq!(search_params(u16::MAX), (0, 15, 0xFFF0));
    }
}
