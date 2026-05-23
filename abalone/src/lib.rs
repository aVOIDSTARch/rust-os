//! abalone — bare-metal x86_64 allocator library for the crusty_os workspace.
//!
//! Provides the full allocator stack used by `crusty_os`:
//!
//! ```text
//! GlobalAlloc  (Box / Vec / alloc::*)
//!   └── TlsfAllocator  — O(1) alloc/dealloc, sub-page granularity
//!         └── BuddyAllocator — page-granularity, binary buddy system
//! ```
//!
//! [`SlabCache<T>`] provides a typed per-object cache on top of the buddy
//! allocator for high-frequency fixed-size allocations.
//!
//! The legacy [`bump`] and [`linked_list`] modules are retained as reference
//! implementations used by the `use-bootloader` kernel path.

#![no_std]

pub mod buddy;
pub mod slab;
pub mod tlsf;
pub mod bump;
pub mod linked_list;

// ── Shared utilities ──────────────────────────────────────────────────────────

/// Spin-mutex wrapper used by [`bump::BumpAllocator`] as a `GlobalAlloc` target.
pub struct Locked<A> {
    inner: spin::Mutex<A>,
}

impl<A> Locked<A> {
    pub const fn new(inner: A) -> Self {
        Locked { inner: spin::Mutex::new(inner) }
    }

    pub fn lock(&self) -> spin::MutexGuard<'_, A> {
        self.inner.lock()
    }
}

/// Align `addr` up to the nearest multiple of `align` (must be a power of two).
#[inline]
pub fn align_up(addr: usize, align: usize) -> usize {
    (addr + align - 1) & !(align - 1)
}
