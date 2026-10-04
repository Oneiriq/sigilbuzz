//! What a `Font` keeps for its GSUB and GPOS is bounded by the tables'
//! size, and keeping it never changes the output, on tables built to
//! make the caches work hard: one lookup listed under many indices,
//! lookups that overlap in the table, and one subtable repeated
//! thousands of times in a lookup.
//!
//! Each test swaps Open Sans's GSUB or GPOS for a synthetic one, shapes
//! with one `Font` three times (its caches are built from the second
//! call on) and with a fresh `Font`, and counts the heap the used font
//! keeps with a per-thread counting allocator.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use sigilbuzz::{shape, Blob, Buffer, Face, Font};

const OPEN_SANS: &[u8] = include_bytes!("fixtures/opensans_regular.ttf");

/// Counts the bytes each thread has live, so tests running at once do
/// not see each other's allocations.
struct Counting;

thread_local! {
    static LIVE: Cell<isize> = const { Cell::new(0) };
}

fn add(bytes: isize) {
    let _ = LIVE.try_with(|live| live.set(live.get() + bytes));
}

// SAFETY: every method forwards to `System` with the same arguments; the
// counter only reads the layouts.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        add(layout.size() as isize);
        // SAFETY: forwarded as is.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        add(layout.size() as isize);
        // SAFETY: forwarded as is.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        add(-(layout.size() as isize));
        // SAFETY: forwarded as is.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        add(new_size as isize - layout.size() as isize);
        // SAFETY: forwarded as is.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

fn live() -> isize {
    LIVE.with(Cell::get)
}

fn push16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_be_bytes());
}

fn u16s(values: &[u16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(2 * values.len());
    for &v in values {
        push16(&mut out, v);
    }
    out
}

fn offset16(v: usize) -> u16 {
    u16::try_from(v).expect("offset fits in 16 bits")
}

/// `font` with table `tag` replaced by `table`.
fn with_table(font: &[u8], tag: [u8; 4], table: &[u8]) -> Vec<u8> {
    let be16 = |at: usize| usize::from(u16::from_be_bytes([font[at], font[at + 1]]));
    let be32 = |at: usize| {
        u32::from_be_bytes([font[at], font[at + 1], font[at + 2], font[at + 3]]) as usize
    };
    let mut tables: Vec<([u8; 4], &[u8])> = (0..be16(4))
        .map(|i| 12 + 16 * i)
        .map(|at| {
            let t = [font[at], font[at + 1], font[at + 2], font[at + 3]];
            (t, &font[be32(at + 8)..be32(at + 8) + be32(at + 12)])
        })
        .filter(|(t, _)| *t != tag)
        .collect();
    tables.push((tag, table));
    tables.sort_by_key(|(t, _)| *t);
    let count = tables.len() as u16;
    let selector = 15 - count.leading_zeros() as u16;
    let range = (1u16 << selector) * 16;
    let mut out = font[..4].to_vec();
    for v in [count, range, selector, count * 16 - range] {
        push16(&mut out, v);
    }
    let mut data = Vec::new();
    let data_at = 12 + 16 * tables.len();
    for (t, bytes) in &tables {
        out.extend_from_slice(t);
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&((data_at + data.len()) as u32).to_be_bytes());
        out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
        data.extend_from_slice(bytes);
        while data.len() % 4 != 0 {
            data.push(0);
        }
    }
    out.extend(data);
    out
}

/// A GSUB or GPOS whose DFLT and latn default language systems list one
/// feature `tag` with `lookup_indices`, over `lookup_list`.
fn layout_table(tag: [u8; 4], lookup_indices: &[u16], lookup_list: &[u8]) -> Vec<u8> {
    let mut script_list = u16s(&[2]);
    script_list.extend_from_slice(b"DFLT");
    push16(&mut script_list, 14);
    script_list.extend_from_slice(b"latn");
    push16(&mut script_list, 14);
    // Script: default LangSys at 4, no others; LangSys: no required
    // feature, feature 0.
    script_list.extend(u16s(&[4, 0, 0, 0xFFFF, 1, 0]));
    let mut feature_list = u16s(&[1]);
    feature_list.extend_from_slice(&tag);
    push16(&mut feature_list, 8);
    feature_list.extend(u16s(&[0, offset16(lookup_indices.len())]));
    feature_list.extend(u16s(lookup_indices));
    let script_at: usize = 10;
    let feature_at = script_at + script_list.len();
    let lookups_at = feature_at + feature_list.len();
    let mut out = u16s(&[
        1,
        0,
        offset16(script_at),
        offset16(feature_at),
        offset16(lookups_at),
    ]);
    out.extend(script_list);
    out.extend(feature_list);
    out.extend_from_slice(lookup_list);
    out
}

/// A lookup of `lookup_type` whose `count` subtable offsets all point
/// at `subtable`.
fn repeated_lookup(lookup_type: u16, count: u16, subtable: &[u8]) -> Vec<u8> {
    let body = 6 + 2 * usize::from(count);
    let mut out = u16s(&[lookup_type, 0, count]);
    out.extend(u16s(&vec![offset16(body); usize::from(count)]));
    out.extend_from_slice(subtable);
    out
}

/// A LookupList of `indices` indices that all name `lookup`.
fn shared_lookup_list(indices: u16, lookup: &[u8]) -> Vec<u8> {
    let at = offset16(2 + 2 * usize::from(indices));
    let mut out = u16s(&[indices]);
    out.extend(u16s(&vec![at; usize::from(indices)]));
    out.extend_from_slice(lookup);
    out
}

/// A chained context format 3 subtable whose input is `first` then
/// glyph 1, which never follows it: it reads its coverages and never
/// matches.
fn chain_context3(first: u16) -> Vec<u8> {
    // No backtrack, two input coverages (at 14 and 20), no lookahead,
    // no records.
    let mut out = u16s(&[3, 0, 2, 14, 20, 0, 0]);
    out.extend(u16s(&[1, 1, first]));
    out.extend(u16s(&[1, 1, 1]));
    out
}

/// A chained context format 2 subtable covering `first`, with no class
/// sets: class-based, so its matching reads the subtable cache rank.
fn chain_context2(first: u16) -> Vec<u8> {
    let mut out = u16s(&[2, 12, 18, 18, 18, 0]);
    out.extend(u16s(&[1, 1, first]));
    // ClassDef format 2 with no ranges.
    out.extend(u16s(&[2, 0]));
    out
}

fn glyph(font: &[u8], c: char) -> u16 {
    let face = Face::parse_bytes(font, 0).unwrap();
    face.cmap().unwrap().glyph_id(c).unwrap()
}

type Shaped = Vec<(u32, u32, i32, i32, i32, i32)>;

fn run(font: &Font<'_>, text: &str) -> Shaped {
    let mut buffer = Buffer::new();
    buffer.push_str(text);
    buffer.guess_segment_properties();
    shape(font, &buffer, &[])
        .unwrap()
        .glyphs
        .iter()
        .map(|g| {
            (
                g.glyph_id,
                g.cluster,
                g.x_advance,
                g.y_advance,
                g.x_offset,
                g.y_offset,
            )
        })
        .collect()
}

/// Shapes `text` three times with one font of `data`, checks each
/// result against a fresh font's, and returns the heap the used font
/// keeps.
fn kept_bytes(data: &[u8], text: &str) -> usize {
    let blob = Blob::new(data);
    let fresh = run(&Font::new(Face::parse(&blob, 0).unwrap(), 1000.0), text);
    let before = live();
    let font = Font::new(Face::parse(&blob, 0).unwrap(), 1000.0);
    for call in 0..3 {
        assert_eq!(run(&font, text), fresh, "call {call}");
    }
    let kept = live() - before;
    drop(font);
    usize::try_from(kept).unwrap_or(0)
}

#[test]
fn indices_of_one_lookup_keep_its_digests_once() {
    // 64 lookup indices name one lookup of 2,000 subtables. Keeping
    // 24 bytes per subtable for each index took 3 MB.
    let a = glyph(OPEN_SANS, 'a');
    let lookup = repeated_lookup(6, 2000, &chain_context3(a));
    let indices: Vec<u16> = (0..64).collect();
    let gsub = layout_table(*b"ccmp", &indices, &shared_lookup_list(64, &lookup));
    let font = with_table(OPEN_SANS, *b"GSUB", &gsub);
    let kept = kept_bytes(&font, "a");
    assert!(
        kept < 64 << 10,
        "{kept} bytes kept for a {}-byte GSUB",
        gsub.len()
    );

    // The same in GPOS, with chained context positioning.
    let lookup = repeated_lookup(8, 2000, &chain_context3(a));
    let gpos = layout_table(*b"kern", &indices, &shared_lookup_list(64, &lookup));
    let font = with_table(OPEN_SANS, *b"GPOS", &gpos);
    let kept = kept_bytes(&font, "a");
    assert!(
        kept < 64 << 10,
        "{kept} bytes kept for a {}-byte GPOS",
        gpos.len()
    );
}

#[test]
fn overlapping_lookups_keep_what_the_table_size_allows() {
    // 400 lookups at distinct offsets that overlap: the words repeat
    // [1, 0, N], so a lookup at every third word reads type 1 (single
    // substitution), flag 0, and N subtables at offsets [1, 0, N, ...]
    // from it, some of which parse (with an empty coverage) and some
    // of which do not. Index sharing cannot help; keeping every
    // subtable's digest took 19 MB for this 7 KB table.
    let (lookups, n) = (400usize, 2000u16);
    let words = 3 * lookups + usize::from(n) + 3;
    let pattern: Vec<u16> = [1, 0, n].into_iter().cycle().take(words).collect();
    let first = 2 + 2 * lookups;
    let mut lookup_list = u16s(&[offset16(lookups)]);
    lookup_list.extend(u16s(
        &(0..lookups)
            .map(|k| offset16(first + 6 * k))
            .collect::<Vec<_>>(),
    ));
    lookup_list.extend(u16s(&pattern));
    let indices: Vec<u16> = (0..offset16(lookups)).collect();
    let gsub = layout_table(*b"ccmp", &indices, &lookup_list);
    let font = with_table(OPEN_SANS, *b"GSUB", &gsub);
    let kept = kept_bytes(&font, "a");
    // 24 bytes a digest for at most twice the table's length plus
    // 64 Ki digests, and a few dozen bytes per lookup.
    let bound = 24 * (2 * gsub.len() + (1 << 16)) + 64 * lookups;
    assert!(kept < bound, "{kept} bytes kept, bound {bound}");
}

#[test]
fn many_class_based_subtables_shape_as_a_fresh_font_does() {
    // One lookup of 3,000 offsets to a class-based chained context:
    // every subtable past the eighth asks for its cache rank at every
    // glyph. The cache rank of `lazy.rs` has a unit test that counts
    // the work; this one checks the output on real tables.
    let a = glyph(OPEN_SANS, 'a');
    let lookup = repeated_lookup(6, 3000, &chain_context2(a));
    let gsub = layout_table(*b"ccmp", &[0], &shared_lookup_list(1, &lookup));
    let font = with_table(OPEN_SANS, *b"GSUB", &gsub);
    kept_bytes(&font, "aaaa");
    let lookup = repeated_lookup(8, 3000, &chain_context2(a));
    let gpos = layout_table(*b"kern", &[0], &shared_lookup_list(1, &lookup));
    let font = with_table(OPEN_SANS, *b"GPOS", &gpos);
    kept_bytes(&font, "aaaa");
}
