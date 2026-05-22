//! `userspace/profiled/src/lib.rs` — jemalloc heap profiling and introspection.
//!
//! # Role of this crate
//!
//! The application binary uses **mimalloc** as its `#[global_allocator]` for
//! throughput.  This crate provides a *secondary* jemalloc instance used for:
//!
//! 1. **Heap statistics** — live counters for allocated/freed bytes, thread
//!    cache state, arena utilisation, fragmentation ratios.
//! 2. **Heap profiling** — sampled allocation backtraces written to `.heap`
//!    files, analysable with `jeprof`/`pprof`.
//! 3. **Introspection API** — a `HeapProfiler` handle that wraps `mallctl`
//!    calls into a safe, ergonomic Rust interface.
//!
//! Nothing in this crate changes the application's global allocator.  The
//! jemalloc instance here manages its own arena pool, independent of mimalloc.
//!
//! # Why jemalloc specifically for profiling?
//!
//! mimalloc's profiling support is minimal at the Rust API level (as of 2025).
//! jemalloc's `mallctl` API is the most mature production heap profiling
//! interface available — used at Meta, PostgreSQL, and Redis for years.
//! `tikv-jemalloc-ctl` provides safe Rust bindings without FFI boilerplate.
//!
//! # Usage
//!
//! ```rust
//! use profiled::HeapProfiler;
//!
//! let prof = HeapProfiler::new();
//! prof.enable_sampling(512 * 1024);   // sample every 512 KiB allocated
//!
//! // ... run your workload ...
//!
//! prof.dump("my_workload.heap").unwrap(); // write to file
//! let stats = prof.snapshot();
//! println!("peak: {} MiB", stats.peak_bytes / (1024 * 1024));
//! ```

use std::{
    io,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

use tikv_jemalloc_ctl::{epoch, stats};
use shared::AllocStats;

// ── One-time initialisation guard ────────────────────────────────────────────

static INIT: AtomicBool = AtomicBool::new(false);

fn ensure_init() {
    if INIT.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
        // Force jemalloc to initialise its stats counters by advancing the epoch.
        // Without this the first snapshot returns zeroes.
        let e = epoch::mib().expect("jemalloc epoch mib");
        e.advance().expect("jemalloc epoch advance");
    }
}

// ── HeapProfiler ─────────────────────────────────────────────────────────────

/// A handle to jemalloc's introspection and heap-profiling subsystem.
///
/// Multiple `HeapProfiler` instances are safe — they all refer to the same
/// jemalloc arena pool and the same `mallctl` interface.
pub struct HeapProfiler {
    /// Cached MIB (Management Information Base) paths for hot mallctl queries.
    epoch_mib:              tikv_jemalloc_ctl::epoch_mib,
    allocated_mib:          stats::allocated_mib,
    active_mib:             stats::active_mib,
    resident_mib:           stats::resident_mib,
    metadata_mib:           stats::metadata_mib,
    mapped_mib:             stats::mapped_mib,
    retained_mib:           stats::retained_mib,
    /// Monotonic allocation counter at construction time (for delta tracking).
    baseline_alloc:         u64,
    created_at:             Instant,
}

impl HeapProfiler {
    /// Construct a profiler handle.  Cheap — only resolves MIB paths once.
    ///
    /// MIB resolution (converting a dotted `mallctl` path string to a numeric
    /// key) costs a hash lookup; caching the MIB avoids repeating it on every
    /// snapshot.
    pub fn new() -> Self {
        ensure_init();
        let e = epoch::mib().expect("jemalloc epoch mib");
        e.advance().expect("jemalloc epoch advance");

        let allocated_mib = stats::allocated::mib().expect("stats::allocated mib");
        let baseline = allocated_mib.read().unwrap_or(0) as u64;

        Self {
            epoch_mib:     e,
            allocated_mib,
            active_mib:    stats::active::mib().expect("stats::active mib"),
            resident_mib:  stats::resident::mib().expect("stats::resident mib"),
            metadata_mib:  stats::metadata::mib().expect("stats::metadata mib"),
            mapped_mib:    stats::mapped::mib().expect("stats::mapped mib"),
            retained_mib:  stats::retained::mib().expect("stats::retained mib"),
            baseline_alloc: baseline,
            created_at:    Instant::now(),
        }
    }

    /// Advance the jemalloc epoch and take a fresh statistics snapshot.
    ///
    /// jemalloc defers stat updates to epoch advances for performance.
    /// Always call this before reading stats; stale reads return old data.
    pub fn snapshot(&self) -> AllocStats {
        self.epoch_mib.advance().unwrap_or(0);

        let allocated = self.allocated_mib.read().unwrap_or(0) as u64;
        let active    = self.active_mib.read().unwrap_or(0)    as u64;
        let resident  = self.resident_mib.read().unwrap_or(0)  as u64;
        let metadata  = self.metadata_mib.read().unwrap_or(0)  as u64;
        let mapped    = self.mapped_mib.read().unwrap_or(0)    as u64;
        let retained  = self.retained_mib.read().unwrap_or(0)  as u64;

        // `active` = bytes in pages committed to the OS but potentially partially
        //            used.  It's the most useful "live footprint" metric.
        // `allocated` = bytes handed to the application (sum of live alloc sizes).
        // `resident`  = physical pages resident in RAM (RSS analogue).
        // `mapped`    = total virtual pages mapped from the OS.
        // `metadata`  = bytes used by jemalloc's own bookkeeping.
        // `retained`  = virtual memory retained but not mapped (ready to reuse).

        AllocStats {
            total_bytes:   mapped,
            used_bytes:    allocated,
            free_bytes:    active.saturating_sub(allocated),
            alloc_count:   0,       // jemalloc mallctl doesn't expose alloc count
            dealloc_count: 0,       // without a custom allocator hook
            peak_bytes:    resident,
        }
    }

    /// Enable heap profiling with the given sampling interval.
    ///
    /// `sample_bytes` — jemalloc will record a backtrace approximately every
    /// `sample_bytes` bytes allocated.  Lower values give finer resolution at
    /// higher overhead; 512 KiB is a reasonable production default.
    ///
    /// # Prerequisites
    ///
    /// The binary must be built with `MALLOC_CONF=prof:true` or linked against
    /// a jemalloc built with `--enable-prof`.  The `tikv-jemallocator` crate
    /// enables this via the `profiling` feature flag in Cargo.toml.
    pub fn enable_sampling(&self, sample_bytes: usize) -> Result<(), &'static str> {
        // Set lg_prof_sample = log2(sample_bytes).
        let lg = (usize::BITS - 1 - sample_bytes.leading_zeros()) as u64;
        tikv_jemalloc_ctl::raw::write(b"prof.lg_sample\0", lg)
            .map_err(|_| "failed to set prof.lg_sample — is jemalloc built with --enable-prof?")?;
        tikv_jemalloc_ctl::raw::write(b"prof.active\0", true)
            .map_err(|_| "failed to activate profiler")?;
        Ok(())
    }

    /// Dump the current heap profile to `path`.
    ///
    /// The resulting file is in jemalloc's `.heap` format, readable by:
    /// * `jeprof --text <binary> <file>.heap`
    /// * `pprof --text <binary> <file>.heap`
    /// * Flamegraph tools that accept pprof format after conversion.
    pub fn dump(&self, path: &str) -> io::Result<()> {
        // mallctl "prof.dump" expects a null-terminated path.
        let mut buf = path.as_bytes().to_vec();
        buf.push(0);
        tikv_jemalloc_ctl::raw::write(b"prof.dump\0", buf.as_ptr())
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("jeprof dump: {e}")))?;
        Ok(())
    }

    /// Reset the profiler's accumulated stats (does not affect jemalloc internals).
    pub fn elapsed(&self) -> Duration {
        self.created_at.elapsed()
    }

    /// Bytes allocated since this `HeapProfiler` was constructed.
    pub fn bytes_allocated_since_new(&self) -> u64 {
        self.epoch_mib.advance().unwrap_or(0);
        let current = self.allocated_mib.read().unwrap_or(0) as u64;
        current.saturating_sub(self.baseline_alloc)
    }
}

impl Default for HeapProfiler {
    fn default() -> Self { Self::new() }
}

// ── Detailed arena statistics ─────────────────────────────────────────────────

/// Per-arena statistics snapshot.
#[derive(Debug, Clone)]
pub struct ArenaStats {
    pub arena_index: usize,
    pub small_allocated: u64,
    pub large_allocated: u64,
    pub dirty_pages: u64,
    pub muzzy_pages: u64,
}

/// Query per-arena stats for all active jemalloc arenas.
///
/// Returns an empty Vec if stats are unavailable (e.g., non-profiling build).
pub fn arena_stats() -> Vec<ArenaStats> {
    // Advance epoch first.
    if let Ok(e) = epoch::mib() { let _ = e.advance(); }

    // Read the number of arenas via the narenas mallctl.
    let narenas: u32 = tikv_jemalloc_ctl::raw::read(b"opt.narenas\0")
        .unwrap_or(0);

    (0..narenas as usize).filter_map(|i| {
        // Construct the per-arena mallctl path dynamically.
        // tikv-jemalloc-ctl doesn't provide per-arena typed access,
        // so we fall back to raw mallctl with manual path construction.
        let small_key = format!("stats.arenas.{}.small.allocated\0", i);
        let large_key = format!("stats.arenas.{}.large.allocated\0", i);
        let dirty_key = format!("stats.arenas.{}.pdirty\0", i);
        let muzzy_key = format!("stats.arenas.{}.pmuzzy\0", i);

        let small: u64 = tikv_jemalloc_ctl::raw::read(small_key.as_bytes()).ok()?;
        let large: u64 = tikv_jemalloc_ctl::raw::read(large_key.as_bytes()).ok()?;
        let dirty: u64 = tikv_jemalloc_ctl::raw::read(dirty_key.as_bytes()).ok()?;
        let muzzy: u64 = tikv_jemalloc_ctl::raw::read(muzzy_key.as_bytes()).ok()?;

        Some(ArenaStats {
            arena_index:     i,
            small_allocated: small,
            large_allocated: large,
            dirty_pages:     dirty,
            muzzy_pages:     muzzy,
        })
    }).collect()
}

// ── Continuous monitoring ─────────────────────────────────────────────────────

/// Spawn a background thread that logs heap stats every `interval`.
///
/// The thread runs until the returned `JoinHandle` is dropped (or the process
/// exits).  Useful for long-running services where you want periodic RSS
/// and fragmentation data in logs without halting the application.
pub fn spawn_monitor(interval: Duration) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let profiler = HeapProfiler::new();
        loop {
            std::thread::sleep(interval);
            let s = profiler.snapshot();
            eprintln!(
                "[heap-monitor] used={:.1}MiB  active={:.1}MiB  \
                 resident(peak)={:.1}MiB  frag={:.1}%",
                s.used_bytes  as f64 / (1024.0 * 1024.0),
                s.free_bytes  as f64 / (1024.0 * 1024.0),
                s.peak_bytes  as f64 / (1024.0 * 1024.0),
                s.fragmentation_ratio() * 100.0,
            );
        }
    })
}

// ── Scoped profiling helper ───────────────────────────────────────────────────

/// RAII guard that prints allocation delta on drop.
///
/// ```rust
/// let _scope = ScopedAlloc::new("parse_config");
/// parse_config(&data);   // allocations here are attributed to the scope
/// // drop prints: [scope:parse_config] allocated 48 KiB in 2.3ms
/// ```
pub struct ScopedAlloc {
    name:    &'static str,
    start:   u64,
    timer:   Instant,
    // Store the profiler so it isn't dropped before the scope ends.
    profiler: HeapProfiler,
}

impl ScopedAlloc {
    pub fn new(name: &'static str) -> Self {
        let p = HeapProfiler::new();
        let start = p.bytes_allocated_since_new();
        Self { name, start, timer: Instant::now(), profiler: p }
    }
}

impl Drop for ScopedAlloc {
    fn drop(&mut self) {
        let delta = self.profiler.bytes_allocated_since_new()
            .saturating_sub(self.start);
        eprintln!(
            "[scope:{}] allocated {:.1} KiB in {:.2?}",
            self.name,
            delta as f64 / 1024.0,
            self.timer.elapsed(),
        );
    }
}
