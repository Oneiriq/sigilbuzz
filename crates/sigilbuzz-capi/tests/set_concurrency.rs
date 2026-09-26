//! `hb_set_t` under concurrent access.
//!
//! HarfBuzz lets several threads read one set at once, for example two
//! `hb_subset_or_fail` calls sharing a subset input. The set used to sit
//! in a `RefCell`, whose non-atomic borrow counter races under that load
//! and can leave the set looking permanently borrowed. The set is now
//! behind a lock; these tests hammer one set from several threads and
//! check every thread sees consistent contents.

use std::thread;

use sigilbuzz_capi::set::{
    hb_set_add, hb_set_create, hb_set_del, hb_set_destroy, hb_set_get_population, hb_set_has,
    hb_set_t,
};

const THREADS: usize = 8;
const ROUNDS: u32 = 20_000;

/// Raw set pointers are not `Send`; carry the address across threads
/// and turn it back into a pointer on the other side.
fn addr(set: *mut hb_set_t) -> usize {
    set as usize
}

fn ptr(addr: usize) -> *mut hb_set_t {
    addr as *mut hb_set_t
}

#[test]
fn concurrent_readers_see_a_stable_set() {
    let set = hb_set_create();
    // SAFETY: every pointer passed here is null, a live handle created
    // in this test, or data that outlives the call.
    unsafe {
        for cp in 0..64u32 {
            hb_set_add(set, cp * 2);
        }
    }
    let shared = addr(set);
    let handles: Vec<_> = (0..THREADS)
        .map(|_| {
            thread::spawn(move || {
                let set = ptr(shared);
                let mut hits = 0u32;
                for round in 0..ROUNDS {
                    let cp = round % 128;
                    // SAFETY: every pointer passed here is null, a live handle created
                    // in this test, or data that outlives the call.
                    if unsafe { hb_set_has(set, cp) } != 0 {
                        hits += 1;
                    }
                    // SAFETY: every pointer passed here is null, a live handle created
                    // in this test, or data that outlives the call.
                    assert_eq!(unsafe { hb_set_get_population(set) }, 64);
                }
                hits
            })
        })
        .collect();
    for handle in handles {
        // Even code points below 128 are present: half of every
        // 128-round cycle hits.
        assert_eq!(handle.join().expect("reader thread"), ROUNDS / 2);
    }
    // SAFETY: every pointer passed here is null, a live handle created
    // in this test, or data that outlives the call.
    unsafe { hb_set_destroy(set) };
}

#[test]
fn concurrent_writers_do_not_lose_updates() {
    let set = hb_set_create();
    let shared = addr(set);
    let handles: Vec<_> = (0..THREADS)
        .map(|t| {
            thread::spawn(move || {
                let set = ptr(shared);
                let base = (t as u32) * ROUNDS;
                for i in 0..ROUNDS {
                    // SAFETY: every pointer passed here is null, a live handle created
                    // in this test, or data that outlives the call.
                    unsafe { hb_set_add(set, base + i) };
                }
                // Remove the odd half again so adds and deletes interleave
                // with the other threads' adds.
                for i in (1..ROUNDS).step_by(2) {
                    // SAFETY: every pointer passed here is null, a live handle created
                    // in this test, or data that outlives the call.
                    unsafe { hb_set_del(set, base + i) };
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().expect("writer thread");
    }
    let expected = (THREADS as u32) * ROUNDS / 2;
    // SAFETY: every pointer passed here is null, a live handle created
    // in this test, or data that outlives the call.
    assert_eq!(unsafe { hb_set_get_population(set) }, expected);
    // SAFETY: every pointer passed here is null, a live handle created
    // in this test, or data that outlives the call.
    unsafe { hb_set_destroy(set) };
}

#[test]
fn set_handle_is_send_and_sync_without_unsafe_impls() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<hb_set_t>();
}
