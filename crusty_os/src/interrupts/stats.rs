// interrupts/stats.rs
//
// Per-vector interrupt statistics using lock-free atomic counters.
//
// Each of the 256 possible interrupt vectors has its own AtomicU64 counter.
// These are incremented from interrupt handlers (which cannot block) and
// read from any context.
//
// Design choices:
//   - AtomicU64 with Relaxed ordering is sufficient for monotonic counters
//     observed only after an appropriate synchronization point (e.g., a
//     SeqCst fence in the reader).
//   - A flat array of 256 atomics avoids any heap allocation and has
//     O(1) access by vector index.
//   - The array lives in a static, so it is zero-initialized before any
//     interrupt can fire.

use core::sync::atomic::{AtomicU64, Ordering};

// 256-entry flat table, one counter per vector.
// `AtomicU64` is not `Copy`, so we cannot use array literal shorthand;
// instead we use a const-fn workaround.
static COUNTERS: [AtomicU64; 256] = {
    // AtomicU64::new(0) is a const fn, so this is legal.
    #[allow(clippy::declare_interior_mutable_const)]
    const ZERO: AtomicU64 = AtomicU64::new(0);
    [ZERO; 256]
};

/// Increment the counter for `vector`.  Call once per interrupt delivery.
///
/// Uses `Relaxed` ordering: the increment itself is atomic, and the
/// happens-before relationship with the handler body is guaranteed by the
/// CPU's interrupt delivery mechanism.
#[inline(always)]
pub fn record(vector: u8) {
    COUNTERS[vector as usize].fetch_add(1, Ordering::Relaxed);
}

/// Return the total delivery count for `vector` since boot.
///
/// Uses `Acquire` so the caller sees all stores from handlers that completed
/// before the last increment on any CPU.
#[inline]
pub fn count(vector: u8) -> u64 {
    COUNTERS[vector as usize].load(Ordering::Acquire)
}

/// Return a snapshot of all 256 counters as an array of `(vector, count)`
/// pairs, filtered to only those with at least one delivery.
///
/// Intended for diagnostic output; not called from interrupt context.
pub fn snapshot_active() -> alloc::vec::Vec<(u8, u64)> {
    COUNTERS
        .iter()
        .enumerate()
        .filter_map(|(v, c)| {
            let n = c.load(Ordering::Acquire);
            if n > 0 { Some((v as u8, n)) } else { None }
        })
        .collect()
}

/// Reset all counters to zero.  Intended for test harnesses only.
///
/// # Safety
/// Must not be called while interrupt handlers that call `record()` may fire,
/// unless the caller can tolerate a transient undercount.
pub unsafe fn reset_all() {
    for c in COUNTERS.iter() {
        c.store(0, Ordering::Relaxed);
    }
}
