//! `slab.rs` — Typed slab allocator for bare-metal x86_64.
//!
//! # Design
//!
//! A `SlabCache<T>` holds a collection of *slabs*, where each slab is a
//! single buddy-allocated page (or contiguous page block) partitioned into
//! `T`-sized slots.  Free slots are chained through a per-slab free list.
//! Each `SlabCache<T>` is `Send + Sync` behind a `Mutex`.
//!
//! ## Object size constraints
//!
//! * `size_of::<T>()` must be ≥ `size_of::<usize>()` — we store the next-free
//!   pointer *inside* the free slot, requiring at least pointer width.
//! * Alignment of `T` must be ≤ `PAGE_SIZE` (4 KiB).
//!
//! ## Slab lifecycle
//!
//! ```text
//!  empty ──alloc──► partial ──alloc (last slot)──► full
//!  full  ──free──►  partial ──free  (last live)──► empty ──(reclaim)──► buddy
//! ```
//!
//! Empty slabs are returned to the buddy allocator immediately to avoid
//! long-term memory hoarding — a deliberate policy choice that trades minor
//! allocation cost for a tighter RSS profile.
//!
//! ## Constructor / Destructor caching
//!
//! Unlike Linux's slab (which can cache initialised objects), this
//! implementation does *not* cache constructors.  That feature requires either
//! a vtable or const-generic fn pointers, neither of which is yet stable in
//! `no_std` contexts.  A `SlabCache<T>` with a constructor can be built on
//! top by the caller.

use core::{
    alloc::Layout,
    marker::PhantomData,
    mem,
    ptr::{self, NonNull},
};
use spin::Mutex;
use shared::{AllocStats, PAGE_SIZE};
use crate::buddy;

// ── Slab header ───────────────────────────────────────────────────────────────

/// Stored at the *end* of each slab page (to keep the start object-aligned).
#[repr(C)]
struct SlabHeader {
    /// Next free object index within this slab (u16::MAX = no free objects).
    free_head: u16,
    /// Number of live (allocated) objects in this slab.
    in_use: u16,
    /// Total object capacity of this slab.
    capacity: u16,
    _pad: u16,
    /// Intrusive linked-list pointers: previous and next slab in the cache.
    prev: *mut SlabHeader,
    next: *mut SlabHeader,
}

// SAFETY: Protected by the outer Mutex<SlabCacheInner<T>>.
unsafe impl Send for SlabHeader {}

impl SlabHeader {
    fn is_full(&self)  -> bool { self.in_use == self.capacity }
    fn is_empty(&self) -> bool { self.in_use == 0 }
}

// ── SlabCacheInner ────────────────────────────────────────────────────────────

struct SlabCacheInner<T> {
    /// Order of pages allocated from the buddy per slab.
    slab_order: usize,
    /// Number of `T` slots per slab.
    objs_per_slab: usize,
    /// Head of the partial-slab list.
    partial: *mut SlabHeader,
    /// Statistics.
    stats: AllocStats,
    _marker: PhantomData<T>,
}

// SAFETY: Protected by Mutex.
unsafe impl<T: Send> Send for SlabCacheInner<T> {}

impl<T> SlabCacheInner<T> {
    const fn compute(order: usize) -> (usize, usize) {
        let slab_bytes  = PAGE_SIZE << order;
        let obj_size    = mem::size_of::<T>();
        let obj_align   = mem::align_of::<T>();
        // Header lives at the end; round the usable prefix down to obj_align.
        let header_size = mem::size_of::<SlabHeader>();
        let usable      = (slab_bytes - header_size) & !(obj_align - 1);
        let capacity    = usable / obj_size;
        (capacity, slab_bytes)
    }

    fn new(slab_order: usize) -> Self {
        let (objs_per_slab, _) = Self::compute(slab_order);
        assert!(objs_per_slab > 0, "object too large for slab order");
        assert!(
            mem::size_of::<T>() >= mem::size_of::<u16>(),
            "T must be at least u16-sized to store free-list index"
        );
        Self {
            slab_order,
            objs_per_slab,
            partial: ptr::null_mut(),
            stats: AllocStats::default(),
            _marker: PhantomData,
        }
    }

    /// Allocate a single `T` from the cache.  Returns `None` if OOM.
    ///
    /// # Safety
    ///
    /// The returned pointer is valid for a write of `T` but is uninitialised.
    /// The caller must initialise the object before creating a shared reference.
    unsafe fn alloc(&mut self) -> Option<NonNull<T>> {
        // Ensure we have a partial slab.
        if self.partial.is_null() {
            self.grow()?;
        }

        let header = &mut *self.partial;
        let slab_base = header as *mut SlabHeader as usize
            - (self.objs_per_slab * mem::size_of::<T>());
        let obj_size   = mem::size_of::<T>();
        let slot_idx   = header.free_head as usize;

        // Read the next-free index stored in the slot we're about to return.
        let slot_ptr = (slab_base + slot_idx * obj_size) as *mut u16;
        header.free_head = ptr::read(slot_ptr);
        header.in_use   += 1;

        // Remove from partial list if now full.
        if header.is_full() {
            self.unlink_partial(header as *mut SlabHeader);
        }

        let obj_ptr = slot_ptr as *mut T;

        self.stats.alloc_count += 1;
        self.stats.used_bytes  += obj_size as u64;
        if self.stats.used_bytes > self.stats.peak_bytes {
            self.stats.peak_bytes = self.stats.used_bytes;
        }

        Some(NonNull::new_unchecked(obj_ptr))
    }

    /// Return a previously allocated `T` to the cache.
    ///
    /// # Safety
    ///
    /// * `ptr` must have been returned by `alloc` on this cache.
    /// * The object's destructor (if any) must have been called by the caller
    ///   before invoking this function.
    unsafe fn dealloc(&mut self, ptr: NonNull<T>) {
        let obj_ptr  = ptr.as_ptr();
        let obj_size = mem::size_of::<T>();

        // Locate the slab header from the object address.
        let slab_bytes  = PAGE_SIZE << self.slab_order;
        let slab_base   = (obj_ptr as usize) & !(slab_bytes - 1);
        let header_addr = slab_base + self.objs_per_slab * obj_size;
        let header      = &mut *(header_addr as *mut SlabHeader);

        let was_full = header.is_full();

        // Write this slot's index into the slot as the new free-list head.
        let slot_idx = (obj_ptr as usize - slab_base) / obj_size;
        ptr::write(obj_ptr as *mut u16, header.free_head);
        header.free_head = slot_idx as u16;
        header.in_use   -= 1;

        if was_full {
            self.link_partial(header as *mut SlabHeader);
        } else if header.is_empty() {
            // Return the empty slab to the buddy allocator.
            self.unlink_partial(header as *mut SlabHeader);
            buddy::dealloc_pages(slab_base as *mut u8, self.slab_order);
            self.stats.total_bytes -= slab_bytes as u64;
            self.stats.free_bytes  -= slab_bytes as u64;
        }

        self.stats.dealloc_count += 1;
        self.stats.used_bytes    -= obj_size as u64;
        self.stats.free_bytes    += obj_size as u64;
    }

    /// Allocate a new slab from the buddy and populate its free list.
    fn grow(&mut self) -> Option<()> {
        let slab_bytes = PAGE_SIZE << self.slab_order;
        let obj_size   = mem::size_of::<T>();

        // SAFETY: We're requesting physically-backed pages from the buddy.
        // The returned memory is exclusively ours until we return it.
        let raw = buddy::alloc_pages(self.slab_order)?;
        let slab_base = raw as usize;

        // Initialise the free list: slot[i] -> i+1, last -> u16::MAX (sentinel).
        for i in 0..self.objs_per_slab {
            let slot = (slab_base + i * obj_size) as *mut u16;
            let next = if i + 1 < self.objs_per_slab { (i + 1) as u16 } else { u16::MAX };
            // SAFETY: Within the newly allocated slab, exclusively owned.
            unsafe { ptr::write(slot, next) };
        }

        // Initialise the slab header at the end of the slab.
        let header_addr = slab_base + self.objs_per_slab * obj_size;
        let header = header_addr as *mut SlabHeader;
        // SAFETY: header_addr is within the slab and not aliased by any object.
        unsafe {
            ptr::write(header, SlabHeader {
                free_head: 0,
                in_use:    0,
                capacity:  self.objs_per_slab as u16,
                _pad:      0,
                prev:      ptr::null_mut(),
                next:      ptr::null_mut(),
            });
            self.link_partial(header);
        }

        self.stats.total_bytes += slab_bytes as u64;
        self.stats.free_bytes  += (self.objs_per_slab * obj_size) as u64;

        Some(())
    }

    unsafe fn link_partial(&mut self, header: *mut SlabHeader) {
        (*header).next = self.partial;
        (*header).prev = ptr::null_mut();
        if !self.partial.is_null() {
            (*self.partial).prev = header;
        }
        self.partial = header;
    }

    unsafe fn unlink_partial(&mut self, header: *mut SlabHeader) {
        let h = &*header;
        if !h.prev.is_null() { (*h.prev).next = h.next; }
        else { self.partial = h.next; }
        if !h.next.is_null() { (*h.next).prev = h.prev; }
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// A thread-safe slab cache for objects of type `T`.
///
/// # Example
///
/// ```rust,no_run
/// static TASK_CACHE: SlabCache<TaskStruct> = SlabCache::new(0);
///
/// fn spawn_task() -> *mut TaskStruct {
///     let slot = TASK_CACHE.alloc().expect("OOM");
///     unsafe { slot.as_ptr().write(TaskStruct::default()) };
///     slot.as_ptr()
/// }
/// ```
pub struct SlabCache<T: Send> {
    inner: Mutex<SlabCacheInner<T>>,
}

impl<T: Send> SlabCache<T> {
    /// Create a new cache.  `slab_order` controls how many buddy pages are
    /// allocated per slab (0 = 4 KiB, 1 = 8 KiB, …).
    ///
    /// Higher orders reduce buddy-allocation frequency but waste more memory
    /// for small object types with low population.
    pub const fn new(slab_order: usize) -> Self {
        Self {
            inner: Mutex::new(SlabCacheInner::new(slab_order)),
        }
    }

    /// Allocate one uninitialised `T`.
    pub fn alloc(&self) -> Option<NonNull<T>> {
        // SAFETY: alloc contract documented on SlabCacheInner::alloc.
        unsafe { self.inner.lock().alloc() }
    }

    /// Deallocate a previously allocated `T`.
    ///
    /// # Safety
    ///
    /// `ptr` must originate from `self.alloc()` and the object's destructor
    /// must have been called before this function is invoked.
    pub unsafe fn dealloc(&self, ptr: NonNull<T>) {
        self.inner.lock().dealloc(ptr);
    }

    pub fn stats(&self) -> AllocStats {
        self.inner.lock().stats
    }
}

// ── Layout validation ─────────────────────────────────────────────────────────

/// Verify at compile time that `T` is compatible with slab allocation.
/// Call this in a `const _: () = assert_slab_compatible::<T>();` in your module.
pub const fn assert_slab_compatible<T>() {
    assert!(
        mem::size_of::<T>() >= 2,
        "SlabCache<T>: T must be at least 2 bytes (free-list index storage)"
    );
    assert!(
        mem::align_of::<T>() <= PAGE_SIZE,
        "SlabCache<T>: T alignment exceeds PAGE_SIZE"
    );
}
