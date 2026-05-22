//! `userspace/app/src/main.rs` — Production application binary using mimalloc.
//!
//! # What this demonstrates
//!
//! * Replacing the system allocator (ptmalloc2 on Linux, malloc on macOS)
//!   with mimalloc via a single `#[global_allocator]` declaration.
//! * Every `Box`, `Vec`, `String`, `Arc`, `HashMap`, and `async` future that
//!   heap-allocates now goes through mimalloc — no other code changes needed.
//! * Practical patterns that benefit most from mimalloc's page-level free lists:
//!   - High-throughput allocation/deallocation of many small objects.
//!   - Multi-threaded workloads (each thread gets its own heap segment).
//!   - Mixed-size allocations without fragmentation spikes.
//! * Integration with the `profiled` crate for jemalloc-backed heap stats
//!   when the `profiled` feature is enabled.
//!
//! # Why mimalloc over ptmalloc2 here?
//!
//! ptmalloc2's per-arena model serialises threads that share an arena.
//! mimalloc gives each OS thread its own *segment* (64 MiB of virtual space)
//! with a thread-local free list — cross-thread frees are deferred and batched.
//! For allocation-heavy Rust binaries (parsers, compilers, servers), this
//! typically yields 15–40% throughput improvement with no code changes.

use mimalloc::MiMalloc;

/// Replace the system allocator.  This single line is the entire change
/// required to switch a Rust binary to mimalloc.
#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    thread,
    time::Instant,
};

use profiled::HeapProfiler;
use shared::AllocStats;

fn main() {
    println!("[app] global allocator: mimalloc (secure mode)");

    // ── Benchmark 1: small object churn ──────────────────────────────────────
    // This is the canonical case where mimalloc dominates ptmalloc2:
    // many short-lived small allocations from multiple threads.
    small_object_churn();

    // ── Benchmark 2: mixed-size HashMap workload ──────────────────────────────
    // Exercises mimalloc's size-class separation across the full range.
    mixed_size_workload();

    // ── Benchmark 3: cross-thread Arc<T> sharing ─────────────────────────────
    // Tests mimalloc's deferred cross-thread free path.
    cross_thread_arc();

    // ── Heap statistics via the profiled crate ────────────────────────────────
    // Note: these stats come from the profiled crate's jemalloc instance,
    // which tracks its *own* sub-heap.  mimalloc does not expose a Rust-native
    // stats API at the time of writing; use MIMALLOC_SHOW_STATS=1 env var
    // or link against mimalloc's C API for richer data.
    let profiler = HeapProfiler::new();
    let stats = profiler.snapshot();
    print_stats("profiled (jemalloc sub-heap)", &stats);

    println!("[app] done.");
}

// ── Small object churn ────────────────────────────────────────────────────────

fn small_object_churn() {
    const THREADS:     usize = 8;
    const ALLOCS_EACH: usize = 500_000;

    let start = Instant::now();

    let handles: Vec<_> = (0..THREADS).map(|t| {
        thread::spawn(move || {
            // Each thread allocates and drops 500k Box<u64>s.
            // mimalloc assigns each thread its own heap segment;
            // no cross-thread contention on the fast path.
            let mut v: Vec<Box<u64>> = Vec::with_capacity(64);
            for i in 0u64..ALLOCS_EACH as u64 {
                v.push(Box::new(i ^ (t as u64)));
                if v.len() == 64 {
                    v.clear();   // drops 64 boxes, returns them to thread cache
                }
            }
            // remaining boxes dropped here
        })
    }).collect();

    for h in handles { h.join().unwrap(); }

    let elapsed = start.elapsed();
    println!(
        "[small_object_churn] {} threads × {} allocs = {} M alloc/s",
        THREADS,
        ALLOCS_EACH,
        (THREADS * ALLOCS_EACH) as f64 / elapsed.as_secs_f64() / 1_000_000.0,
    );
}

// ── Mixed-size HashMap workload ───────────────────────────────────────────────

fn mixed_size_workload() {
    const ENTRIES: usize = 100_000;

    let start = Instant::now();

    // HashMap<String, Vec<u8>> exercises several size classes simultaneously:
    // - HashMap bucket array (power-of-two, large)
    // - String heap (8–256 bytes)
    // - Vec<u8> payload (16–4096 bytes)
    let mut map: HashMap<String, Vec<u8>> = HashMap::with_capacity(ENTRIES);

    for i in 0..ENTRIES {
        let key     = format!("key-{:08x}", i);
        let payload = vec![i as u8; 16 + (i % 240)];
        map.insert(key, payload);
    }

    // Random-access reads (exercises cache-locality of mimalloc's page-local lists).
    let mut checksum = 0u64;
    for i in (0..ENTRIES).step_by(7) {
        let key = format!("key-{:08x}", i);
        if let Some(v) = map.get(&key) {
            checksum ^= v[0] as u64;
        }
    }

    let elapsed = start.elapsed();
    println!(
        "[mixed_size_workload] {} entries, checksum={:#x}, elapsed={:.2?}",
        ENTRIES, checksum, elapsed
    );
}

// ── Cross-thread Arc sharing ──────────────────────────────────────────────────

fn cross_thread_arc() {
    const SHARED_VEC_LEN: usize = 1024;
    const CONSUMERS:      usize = 4;

    let data: Arc<Vec<u64>> = Arc::new((0..SHARED_VEC_LEN as u64).collect());
    let start = Instant::now();

    // The producing thread holds the original Arc.
    // Each consumer receives a clone; when the consumer drops its clone,
    // the deallocation may happen on a different thread than the allocation.
    // mimalloc handles this via its thread-free list (deferred batch free).
    let handles: Vec<_> = (0..CONSUMERS).map(|_| {
        let data = Arc::clone(&data);
        thread::spawn(move || {
            // Read the shared data — forces the Arc to be live across threads.
            data.iter().map(|&x| x).sum::<u64>()
        })
    }).collect();

    let results: Vec<u64> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    drop(data);  // last Arc dropped on the main thread; mimalloc deallocs here

    let elapsed = start.elapsed();
    println!(
        "[cross_thread_arc] {} consumers, sum={}, elapsed={:.2?}",
        CONSUMERS,
        results[0],
        elapsed
    );
}

// ── Stats display ─────────────────────────────────────────────────────────────

fn print_stats(label: &str, stats: &AllocStats) {
    println!(
        "[stats:{}] total={} KiB  used={} KiB  free={} KiB  \
         allocs={}  deallocs={}  peak={} KiB  frag={:.1}%",
        label,
        stats.total_bytes / 1024,
        stats.used_bytes  / 1024,
        stats.free_bytes  / 1024,
        stats.alloc_count,
        stats.dealloc_count,
        stats.peak_bytes  / 1024,
        stats.fragmentation_ratio() * 100.0,
    );
}
