//! `name` — naming table.
//!
//! The `name` table holds human-readable strings — the family name,
//! subfamily, version, copyright, designer credit, and so on — keyed
//! by a 16-bit Name ID, a platform/encoding pair, and a language ID.
//! sigilbuzz parses the directory eagerly (it is small — typically a
//! few dozen records) and decodes individual strings on demand.
//!
//! # Format
//!
//! ```text
//!   offset  type             field
//!     0     u16              version          0 or 1
//!     2     u16              count            number of NameRecords
//!     4     u16              storageOffset    relative to table start
//!     6     NameRecord[count]
//!    ...    (v1 only) u16    langTagCount
//!           LangTagRecord[langTagCount]
//!    ...    string storage   raw bytes
//! ```
//!
//! `NameRecord` is 12 bytes:
//!
//! ```text
//!    0  u16  platformID
//!    2  u16  encodingID
//!    4  u16  languageID
//!    6  u16  nameID
//!    8  u16  length          bytes
//!   10  u16  stringOffset    relative to storageOffset
//! ```
//!
//! `LangTagRecord` is `(u16 length, u16 offset)` for an additional
//! BCP-47 language tag. Sigilbuzz parses the v1 header so the storage
//! pointer lands in the right place but does not currently surface the
//! langTag mapping — modern fonts rarely use it.

use alloc::string::String;
use alloc::vec::Vec;

use crate::error::{Error, Result};
use crate::tables::parse::Reader;

/// One entry in the `name` table directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NameRecord {
    /// Platform identifier (0 = Unicode, 1 = Macintosh, 3 = Windows).
    pub platform_id: u16,
    /// Platform-specific encoding (e.g. `(3, 1)` is Windows UCS-2).
    pub encoding_id: u16,
    /// Platform-specific language identifier.
    pub language_id: u16,
    /// Name identifier (1 = family, 2 = subfamily, 4 = full name, …).
    pub name_id: u16,
    /// Length of the string in bytes.
    pub length: u16,
    /// Offset to the string, relative to the start of the storage area.
    pub string_offset: u16,
}

/// The parsed `name` table.
///
/// Holds the parsed [`NameRecord`] directory plus a borrowed slice of
/// the string storage area; [`Name::get`] and the convenience accessors
/// decode bytes on demand and never allocate beyond the returned
/// [`String`].
#[derive(Debug, Clone)]
pub struct Name<'a> {
    records: Vec<NameRecord>,
    storage: &'a [u8],
}

// -----------------------------------------------------------------------
// Well-known Name IDs the convenience accessors target.
// -----------------------------------------------------------------------

/// Family name. (`(3, 1, *, 1)` for Windows).
const NAME_ID_FAMILY: u16 = 1;
/// Subfamily / style name (e.g. "Regular", "Bold Italic").
const NAME_ID_SUBFAMILY: u16 = 2;
/// Unique identifier.
const NAME_ID_UNIQUE: u16 = 3;
/// Full name (typically "Family Subfamily").
const NAME_ID_FULL: u16 = 4;
/// Version string ("Version 1.234;…").
const NAME_ID_VERSION: u16 = 5;
/// PostScript name.
const NAME_ID_POSTSCRIPT: u16 = 6;
/// Typographic family — preferred over ID 1 when present (added in v4).
const NAME_ID_TYPOGRAPHIC_FAMILY: u16 = 16;
/// Typographic subfamily — preferred over ID 2 when present.
const NAME_ID_TYPOGRAPHIC_SUBFAMILY: u16 = 17;

impl<'a> Name<'a> {
    /// Parses the `name` table directory.
    ///
    /// Validates the header (version 0 or 1, storage offset within the
    /// table) and reads every [`NameRecord`]. Per-record offset/length
    /// validity is checked lazily by [`Name::get`] so a single bad
    /// record does not poison the rest of the table.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let version = r.read_u16()?;
        if version != 0 && version != 1 {
            return Err(Error::Malformed {
                offset: 0,
                context: "name version must be 0 or 1",
            });
        }
        let count = r.read_u16()? as usize;
        let storage_offset = r.read_u16()? as usize;

        let mut records = Vec::with_capacity(count);
        for _ in 0..count {
            let platform_id = r.read_u16()?;
            let encoding_id = r.read_u16()?;
            let language_id = r.read_u16()?;
            let name_id = r.read_u16()?;
            let length = r.read_u16()?;
            let string_offset = r.read_u16()?;
            records.push(NameRecord {
                platform_id,
                encoding_id,
                language_id,
                name_id,
                length,
                string_offset,
            });
        }

        // Version 1 inserts a langTagCount + langTagRecord array
        // between the NameRecord array and the storage area. We don't
        // surface the langTag entries today, but we still walk past
        // them so the storage pointer lands in the right place.
        if version == 1 {
            let lang_tag_count = r.read_u16()? as usize;
            // Each LangTagRecord is 4 bytes (u16 length, u16 offset).
            r.skip(lang_tag_count.saturating_mul(4))?;
        }

        if storage_offset > data.len() {
            return Err(Error::Malformed {
                offset: 4,
                context: "name storageOffset past end of table",
            });
        }
        let storage = &data[storage_offset..];

        Ok(Self { records, storage })
    }

    /// Returns the parsed name records.
    #[must_use]
    pub fn records(&self) -> &[NameRecord] {
        &self.records
    }

    /// Returns the best-encoded string for the given Name ID.
    ///
    /// Walks every record carrying `name_id` and ranks each by its
    /// `(platformID, encodingID)` tuple under the OpenType-conventional
    /// preference order:
    ///
    /// 1. `(3, 1)`  — Windows Unicode BMP (UTF-16BE)
    /// 2. `(3, 10)` — Windows Unicode full repertoire (UTF-16BE)
    /// 3. `(0, *)`  — Unicode platform (UTF-16BE)
    /// 4. `(1, 0)`  — Macintosh Roman (single-byte)
    ///
    /// Records that decode cleanly under the highest available rank
    /// win; malformed ones (out-of-bounds offset, truncated string,
    /// invalid surrogate stream) are skipped silently so a single bad
    /// record never starves the caller of an otherwise-decodable name.
    #[must_use]
    pub fn get(&self, name_id: u16) -> Option<String> {
        let mut best: Option<(u32, String)> = None;
        for rec in self.records.iter().filter(|r| r.name_id == name_id) {
            let Some(rank) = encoding_rank(rec.platform_id, rec.encoding_id) else {
                continue;
            };
            let Some(decoded) = self.decode_record(rec) else {
                continue;
            };
            match &best {
                Some((best_rank, _)) if *best_rank <= rank => {}
                _ => best = Some((rank, decoded)),
            }
        }
        best.map(|(_, s)| s)
    }

    /// Family name (Name ID 1). Falls through to the typographic
    /// family (Name ID 16) when ID 1 is absent.
    #[must_use]
    pub fn family_name(&self) -> Option<String> {
        self.get(NAME_ID_TYPOGRAPHIC_FAMILY)
            .or_else(|| self.get(NAME_ID_FAMILY))
    }

    /// Subfamily / style name (Name ID 2). Falls through to the
    /// typographic subfamily (Name ID 17) when ID 2 is absent.
    #[must_use]
    pub fn subfamily_name(&self) -> Option<String> {
        self.get(NAME_ID_TYPOGRAPHIC_SUBFAMILY)
            .or_else(|| self.get(NAME_ID_SUBFAMILY))
    }

    /// Unique font identifier (Name ID 3).
    #[must_use]
    pub fn unique_id(&self) -> Option<String> {
        self.get(NAME_ID_UNIQUE)
    }

    /// Full font name (Name ID 4).
    #[must_use]
    pub fn full_name(&self) -> Option<String> {
        self.get(NAME_ID_FULL)
    }

    /// Version string (Name ID 5).
    #[must_use]
    pub fn version(&self) -> Option<String> {
        self.get(NAME_ID_VERSION)
    }

    /// PostScript name (Name ID 6).
    #[must_use]
    pub fn postscript_name(&self) -> Option<String> {
        self.get(NAME_ID_POSTSCRIPT)
    }

    /// Carves out the bytes for a single record and decodes them
    /// according to its (platform, encoding) pair. Returns `None` for
    /// truncated storage, invalid encodings, or undecodable bytes.
    fn decode_record(&self, rec: &NameRecord) -> Option<String> {
        let start = rec.string_offset as usize;
        let end = start.checked_add(rec.length as usize)?;
        if end > self.storage.len() {
            return None;
        }
        let bytes = &self.storage[start..end];
        decode_string(rec.platform_id, rec.encoding_id, bytes)
    }
}

/// Lower is better. `None` means "we don't know how to decode this,
/// skip it".
fn encoding_rank(platform_id: u16, encoding_id: u16) -> Option<u32> {
    match (platform_id, encoding_id) {
        (3, 1) => Some(0),
        (3, 10) => Some(1),
        (0, _) => Some(2),
        (1, 0) => Some(3),
        _ => None,
    }
}

/// Top-level decode: dispatch on (platform, encoding) and return a
/// `String` if the bytes round-trip cleanly.
fn decode_string(platform_id: u16, encoding_id: u16, bytes: &[u8]) -> Option<String> {
    match (platform_id, encoding_id) {
        // UTF-16BE: Windows Unicode platforms and the Unicode platform.
        (3, 1 | 10) | (0, _) => decode_utf16_be(bytes),
        (1, 0) => Some(decode_mac_roman(bytes)),
        _ => None,
    }
}

/// Decodes a UTF-16BE byte stream. Returns `None` if the byte count is
/// odd (no half code units allowed) or any surrogate pair is malformed.
fn decode_utf16_be(bytes: &[u8]) -> Option<String> {
    if bytes.len() % 2 != 0 {
        return None;
    }
    let units = bytes
        .chunks_exact(2)
        .map(|c| u16::from_be_bytes([c[0], c[1]]));
    let mut out = String::with_capacity(bytes.len());
    for ch in core::char::decode_utf16(units) {
        out.push(ch.ok()?);
    }
    Some(out)
}

/// Decodes a Macintosh Roman byte stream. Bytes 0x00..=0x7F are plain
/// ASCII; 0x80..=0xFF map through [`MAC_ROMAN_HIGH`] to Unicode
/// codepoints (per the documented Mac OS Roman charset).
fn decode_mac_roman(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    for &b in bytes {
        if b < 0x80 {
            out.push(b as char);
        } else {
            // The lookup table is sized exactly for 0x80..=0xFF and
            // every entry is a valid Unicode scalar value, so the
            // index is in-bounds and `from_u32` will always succeed.
            let cp = MAC_ROMAN_HIGH[(b - 0x80) as usize];
            if let Some(ch) = char::from_u32(cp) {
                out.push(ch);
            }
        }
    }
    out
}

/// Mac OS Roman → Unicode mapping for bytes `0x80..=0xFF`. Drawn
/// directly from Apple's published `ROMAN.TXT` (unicode.org mirror).
/// Indexed by `byte - 0x80`.
#[rustfmt::skip]
const MAC_ROMAN_HIGH: [u32; 128] = [
    // 0x80..=0x8F
    0x00C4, 0x00C5, 0x00C7, 0x00C9, 0x00D1, 0x00D6, 0x00DC, 0x00E1,
    0x00E0, 0x00E2, 0x00E4, 0x00E3, 0x00E5, 0x00E7, 0x00E9, 0x00E8,
    // 0x90..=0x9F
    0x00EA, 0x00EB, 0x00ED, 0x00EC, 0x00EE, 0x00EF, 0x00F1, 0x00F3,
    0x00F2, 0x00F4, 0x00F6, 0x00F5, 0x00FA, 0x00F9, 0x00FB, 0x00FC,
    // 0xA0..=0xAF
    0x2020, 0x00B0, 0x00A2, 0x00A3, 0x00A7, 0x2022, 0x00B6, 0x00DF,
    0x00AE, 0x00A9, 0x2122, 0x00B4, 0x00A8, 0x2260, 0x00C6, 0x00D8,
    // 0xB0..=0xBF
    0x221E, 0x00B1, 0x2264, 0x2265, 0x00A5, 0x00B5, 0x2202, 0x2211,
    0x220F, 0x03C0, 0x222B, 0x00AA, 0x00BA, 0x03A9, 0x00E6, 0x00F8,
    // 0xC0..=0xCF
    0x00BF, 0x00A1, 0x00AC, 0x221A, 0x0192, 0x2248, 0x2206, 0x00AB,
    0x00BB, 0x2026, 0x00A0, 0x00C0, 0x00C3, 0x00D5, 0x0152, 0x0153,
    // 0xD0..=0xDF
    0x2013, 0x2014, 0x201C, 0x201D, 0x2018, 0x2019, 0x00F7, 0x25CA,
    0x00FF, 0x0178, 0x2044, 0x20AC, 0x2039, 0x203A, 0xFB01, 0xFB02,
    // 0xE0..=0xEF
    0x2021, 0x00B7, 0x201A, 0x201E, 0x2030, 0x00C2, 0x00CA, 0x00C1,
    0x00CB, 0x00C8, 0x00CD, 0x00CE, 0x00CF, 0x00CC, 0x00D3, 0x00D4,
    // 0xF0..=0xFF
    0xF8FF, 0x00D2, 0x00DA, 0x00DB, 0x00D9, 0x0131, 0x02C6, 0x02DC,
    0x00AF, 0x02D8, 0x02D9, 0x02DA, 0x00B8, 0x02DD, 0x02DB, 0x02C7,
];

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    /// Builds a synthetic name table for testing. Each `(platform_id,
    /// encoding_id, language_id, name_id, payload)` tuple becomes one
    /// record + payload entry in the storage area.
    fn build_name(version: u16, records: &[(u16, u16, u16, u16, Vec<u8>)]) -> Vec<u8> {
        let header_len = 6 + records.len() * 12;
        let storage_offset = header_len as u16;
        let mut header = Vec::new();
        header.extend_from_slice(&version.to_be_bytes());
        header.extend_from_slice(&(records.len() as u16).to_be_bytes());
        header.extend_from_slice(&storage_offset.to_be_bytes());

        let mut storage = Vec::new();
        for (plat, enc, lang, nid, bytes) in records {
            let off = storage.len() as u16;
            let len = bytes.len() as u16;
            header.extend_from_slice(&plat.to_be_bytes());
            header.extend_from_slice(&enc.to_be_bytes());
            header.extend_from_slice(&lang.to_be_bytes());
            header.extend_from_slice(&nid.to_be_bytes());
            header.extend_from_slice(&len.to_be_bytes());
            header.extend_from_slice(&off.to_be_bytes());
            storage.extend_from_slice(bytes);
        }
        header.extend_from_slice(&storage);
        header
    }

    /// Encodes a `&str` as UTF-16BE bytes for fixture construction.
    fn utf16be(s: &str) -> Vec<u8> {
        let mut out = Vec::with_capacity(s.len() * 2);
        for u in s.encode_utf16() {
            out.extend_from_slice(&u.to_be_bytes());
        }
        out
    }

    #[test]
    fn parses_empty_table() {
        let bytes = build_name(0, &[]);
        let name = Name::parse(&bytes).unwrap();
        assert!(name.records().is_empty());
        assert!(name.family_name().is_none());
    }

    #[test]
    fn parses_record_directory_in_order() {
        let bytes = build_name(
            0,
            &[
                (3, 1, 0x0409, 1, utf16be("Family")),
                (3, 1, 0x0409, 2, utf16be("Regular")),
            ],
        );
        let name = Name::parse(&bytes).unwrap();
        let recs = name.records();
        assert_eq!(recs.len(), 2);
        assert_eq!(recs[0].name_id, 1);
        assert_eq!(recs[0].platform_id, 3);
        assert_eq!(recs[0].encoding_id, 1);
        assert_eq!(recs[1].name_id, 2);
        assert_eq!(recs[1].length as usize, "Regular".len() * 2);
    }

    #[test]
    fn rejects_unknown_version() {
        let bytes = build_name(7, &[]);
        assert!(matches!(Name::parse(&bytes), Err(Error::Malformed { .. })));
    }

    #[test]
    fn rejects_truncated_header() {
        let bytes = vec![0u8; 3];
        assert!(matches!(Name::parse(&bytes), Err(Error::Truncated { .. })));
    }

    #[test]
    fn parses_v1_header_and_skips_lang_tags() {
        // v1 layout: 6-byte header, NameRecord array, then
        // langTagCount (u16) + langTagRecord[langTagCount]. We craft
        // a v1 table with one (3,1) record for Name ID 1 and a single
        // langTag entry; the parser must still locate storage past
        // the langTag records.
        let payload = utf16be("VOne");
        let lang_tag_count: u16 = 1;
        let header_len = 6 + 12 + 2 + (lang_tag_count as usize) * 4;
        let storage_offset = header_len as u16;

        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1u16.to_be_bytes()); // version
        bytes.extend_from_slice(&1u16.to_be_bytes()); // count
        bytes.extend_from_slice(&storage_offset.to_be_bytes());
        // NameRecord
        bytes.extend_from_slice(&3u16.to_be_bytes());
        bytes.extend_from_slice(&1u16.to_be_bytes());
        bytes.extend_from_slice(&0x0409u16.to_be_bytes());
        bytes.extend_from_slice(&1u16.to_be_bytes()); // name_id
        bytes.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        bytes.extend_from_slice(&0u16.to_be_bytes()); // string_offset
                                                      // langTagCount + 1 record (length 0, offset 0).
        bytes.extend_from_slice(&lang_tag_count.to_be_bytes());
        bytes.extend_from_slice(&0u16.to_be_bytes());
        bytes.extend_from_slice(&0u16.to_be_bytes());
        // storage
        bytes.extend_from_slice(&payload);

        let name = Name::parse(&bytes).unwrap();
        assert_eq!(name.family_name().as_deref(), Some("VOne"));
    }

    #[test]
    fn rejects_storage_offset_past_end() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0u16.to_be_bytes()); // version
        bytes.extend_from_slice(&0u16.to_be_bytes()); // count
        bytes.extend_from_slice(&0xFFFFu16.to_be_bytes()); // storage_offset
        assert!(matches!(Name::parse(&bytes), Err(Error::Malformed { .. })));
    }

    #[test]
    fn decodes_windows_unicode_record_for_each_accessor() {
        let bytes = build_name(
            0,
            &[
                (3, 1, 0x0409, 1, utf16be("Tester")),
                (3, 1, 0x0409, 2, utf16be("Bold")),
                (3, 1, 0x0409, 3, utf16be("uniq-1")),
                (3, 1, 0x0409, 4, utf16be("Tester Bold")),
                (3, 1, 0x0409, 5, utf16be("Version 1.0")),
                (3, 1, 0x0409, 6, utf16be("Tester-Bold")),
            ],
        );
        let name = Name::parse(&bytes).unwrap();
        assert_eq!(name.family_name().as_deref(), Some("Tester"));
        assert_eq!(name.subfamily_name().as_deref(), Some("Bold"));
        assert_eq!(name.unique_id().as_deref(), Some("uniq-1"));
        assert_eq!(name.full_name().as_deref(), Some("Tester Bold"));
        assert_eq!(name.version().as_deref(), Some("Version 1.0"));
        assert_eq!(name.postscript_name().as_deref(), Some("Tester-Bold"));
    }

    #[test]
    fn unknown_name_id_returns_none() {
        let bytes = build_name(0, &[(3, 1, 0x0409, 1, utf16be("X"))]);
        let name = Name::parse(&bytes).unwrap();
        assert!(name.get(99).is_none());
    }

    #[test]
    fn picks_highest_ranked_encoding_when_multiple_present() {
        // Same Name ID 1 with three records; the (3, 1) record should
        // beat both (3, 10) and (1, 0).
        let bytes = build_name(
            0,
            &[
                (1, 0, 0, 1, b"MacRomanFamily".to_vec()),
                (3, 10, 0x0409, 1, utf16be("FullRepertoire")),
                (3, 1, 0x0409, 1, utf16be("WinBmp")),
            ],
        );
        let name = Name::parse(&bytes).unwrap();
        assert_eq!(name.family_name().as_deref(), Some("WinBmp"));
    }

    #[test]
    fn typographic_family_overrides_legacy_family() {
        let bytes = build_name(
            0,
            &[
                (3, 1, 0x0409, 1, utf16be("Family Bold")),
                (3, 1, 0x0409, 16, utf16be("Family")),
                (3, 1, 0x0409, 2, utf16be("Bold")),
                (3, 1, 0x0409, 17, utf16be("Bold")),
            ],
        );
        let name = Name::parse(&bytes).unwrap();
        assert_eq!(name.family_name().as_deref(), Some("Family"));
        assert_eq!(name.subfamily_name().as_deref(), Some("Bold"));
    }

    #[test]
    fn skips_unknown_platform_records() {
        // Platform ID 2 (ISO) is recognised by the spec but sigilbuzz
        // does not support it; the only other record (3, 1) should
        // still surface.
        let bytes = build_name(
            0,
            &[
                (2, 0, 0, 1, b"weird".to_vec()),
                (3, 1, 0x0409, 1, utf16be("Real")),
            ],
        );
        let name = Name::parse(&bytes).unwrap();
        assert_eq!(name.family_name().as_deref(), Some("Real"));
    }

    #[test]
    fn skips_record_with_offset_past_storage_end() {
        // Build a table by hand where the single record's stringOffset
        // points outside the storage area. The accessor should return
        // None rather than blow up.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0u16.to_be_bytes()); // version
        bytes.extend_from_slice(&1u16.to_be_bytes()); // count
        let storage_offset: u16 = 6 + 12;
        bytes.extend_from_slice(&storage_offset.to_be_bytes());
        bytes.extend_from_slice(&3u16.to_be_bytes()); // platform
        bytes.extend_from_slice(&1u16.to_be_bytes()); // encoding
        bytes.extend_from_slice(&0u16.to_be_bytes()); // language
        bytes.extend_from_slice(&1u16.to_be_bytes()); // name_id
        bytes.extend_from_slice(&4u16.to_be_bytes()); // length
        bytes.extend_from_slice(&0xFFFFu16.to_be_bytes()); // bogus offset
                                                           // No storage.
        let name = Name::parse(&bytes).unwrap();
        assert!(name.family_name().is_none());
    }

    #[test]
    fn decodes_utf16_surrogate_pair() {
        // U+1D4DE (𝓞) encodes as the surrogate pair 0xD835 0xDCDE.
        let payload = vec![0xD8, 0x35, 0xDC, 0xDE];
        let bytes = build_name(0, &[(3, 1, 0x0409, 1, payload)]);
        let name = Name::parse(&bytes).unwrap();
        assert_eq!(name.family_name().as_deref(), Some("\u{1D4DE}"));
    }

    #[test]
    fn rejects_odd_length_utf16_payload_and_falls_back() {
        // Single (3, 1) record with an odd byte count cannot be
        // decoded as UTF-16BE; with no other records present the
        // accessor returns None.
        let bytes = build_name(0, &[(3, 1, 0x0409, 1, vec![0x00, 0x41, 0x00])]);
        let name = Name::parse(&bytes).unwrap();
        assert!(name.family_name().is_none());
    }

    #[test]
    fn decodes_mac_roman_high_bytes() {
        // 0xA9 → © (U+00A9), 0xC3 → √ (U+221A), 0x41 → 'A'.
        let payload = vec![0x41, 0xA9, 0xC3];
        let bytes = build_name(0, &[(1, 0, 0, 1, payload)]);
        let name = Name::parse(&bytes).unwrap();
        assert_eq!(name.family_name().as_deref(), Some("A\u{00A9}\u{221A}"));
    }

    #[test]
    fn decodes_mac_roman_full_low_ascii() {
        let bytes = build_name(0, &[(1, 0, 0, 1, b"ASCII-only".to_vec())]);
        let name = Name::parse(&bytes).unwrap();
        assert_eq!(name.family_name().as_deref(), Some("ASCII-only"));
    }
}
