//! `buddy.rs` — Binary buddy allocator for x86_64 bare-metal.
//!
//! # Architecture
//!
//! Manages a contiguous address space (physical *or* virtual) as a binary
//! buddy system.  Blocks are always 2^k pages in size (k = 0..=MAX_ORDER-1).
//! Each order maintains a doubly-linked free-list threaded directly through
//! the blocks themselves (no external node storage), plus a companion bitmap
//! where bit `i` tracks whether block pair `i` has *one* free buddy.
//!
//! ## Layout within a managed block
//!
//! When a block is on the free list we overwrite its first 16 bytes with
//! `FreeBlock { prev, next }` pointers.  This is safe because:
//! 1. The block is not visible to any allocator client.
//! 2. We restore the memory to a zeroed state before returning it to callers
//!    (caller responsibility — see `alloc_pages` contract).
//!
//! ## Invariants
//!
//! * All addresses stored are *physical* (or contiguous virtual) offsets from
//!   `base`.
//! * `base` is page-aligned; `total_pages` is a power-of-two multiple.
//! * Bitmap bit `i` at order `k` is set iff exactly one of the pair
//!   `(i*2, i*2+1)` at order `k-1` is free — the standard buddy XOR trick.
//! * A `spin::Mutex` guards the entire structure; interrupt handlers that
//!   allocate pages must disable interrupts before acquiring the lock to
//!   prevent deadlock (the call-site's responsibility).

use core::ptr;
use spin::Mutex;
use shared::{AllocStats, BUDDY_MAX_ORDER, PAGE_SIZE};

// ── Free-list node threaded through free blocks ───────────────────────────────

/// Overlay written into the first 16 bytes of every free page-block.
/// Both fields are raw virtual addresses (kernel identity-map or HHDM).
#[repr(C)]
struct FreeBlock {
    prev: *mut FreeBlock,
    next: *mut FreeBlock,
}

// SAFETY: The buddy allocator is the sole owner of all free blocks;
// access is serialised by the outer Mutex<BuddyAllocator>.
unsafe impl Send for FreeBlock {}

// ── Bitmap helpers ────────────────────────────────────────────────────────────

/// Bitmap storing one bit per buddy pair per order.
/// Maximum: 2^(MAX_ORDER-1) pairs = 1024 bits = 128 bytes per order.
struct Bitmap {
    words: [u64; (1 << (BUDDY_MAX_ORDER - 1)) / 64 + 1],
}

impl Bitmap {
    const fn new() -> Self {
        Self { words: [0u64; (1 << (BUDDY_MAX_ORDER - 1)) / 64 + 1] }
    }

    #[inline]
    fn toggle(&mut self, bit: usize) {
        self.words[bit / 64] ^= 1u64 << (bit % 64);
    }

    #[inline]
    fn test(&self, bit: usize) -> bool {
        (self.words[bit / 64] >> (bit % 64)) & 1 == 1
    }
}

// ── Core allocator structure ──────────────────────────────────────────────────

/// Maximum pages the bitmap can track at order 0.
const MAX_PAGES: usize = 1 << BUDDY_MAX_ORDER;

pub struct BuddyAllocator {
    /// Base *virtual* address of the managed region (HHDM or identity-mapped).
    base: usize,
    /// Total pages under management (must be a power of two for clean buddies;
    /// non-power-of-two regions are handled by initialising multiple aligned
    /// sub-regions — see `add_region`).
    total_pages: usize,
    /// Sentinel heads for each order's doubly-linked free list.
    /// `free_lists[k]` is the head of order-k blocks (each block is 2^k pages).
    free_lists: [*mut FreeBlock; BUDDY_MAX_ORDER],
    /// Per-order buddy bitmaps.
    bitmaps: [Bitmap; BUDDY_MAX_ORDER],
    /// Live statistics.
    stats: AllocStats,
}

// SAFETY: Guarded by Mutex<BuddyAllocator> at the call site.
unsafe impl Send for BuddyAllocator {}

impl BuddyAllocator {
    /// Construct an uninitialised allocator.  Call `add_region` before use.
    pub const fn new() -> Self {
        Self {
            base: 0,
            total_pages: 0,
            free_lists: [ptr::null_mut(); BUDDY_MAX_ORDER],
            bitmaps: [const { Bitmap::new() }; BUDDY_MAX_ORDER],
            stats: AllocStats {
                total_bytes: 0,
                used_bytes: 0,
                free_bytes: 0,
                alloc_count: 0,
                dealloc_count: 0,
                peak_bytes: 0,
            },
        }
    }

    /// Register a physical memory region, expressed as a virtual address range
    /// under the higher-half direct map (HHDM).
    ///
    /// # Safety
    ///
    /// * `virt_base` must be page-aligned and point to genuinely free,
    ///   writable physical RAM mapped into the kernel address space.
    /// * `page_count` pages starting at `virt_base` must not be aliased by
    ///   any other live reference.
    /// * Must be called before any allocation from this region.
    pub unsafe fn add_region(&mut self, virt_base: usize, page_count: usize) {
        assert!(virt_base % PAGE_SIZE == 0, "region base must be page-aligned");
        assert!(page_count > 0, "empty region");

        if self.base == 0 {
            self.base = virt_base;
        }
        // For simplicity we require all regions to share the same base; a
        // production implementation would maintain a region list.
        assert!(
            virt_base >= self.base,
            "region base precedes allocator base"
        );

        let mut remaining = page_count;
        let mut offset_pages = (virt_base - self.base) / PAGE_SIZE;

        while remaining > 0 {
            // Find the largest order that is naturally aligned and fits.
            let order = (remaining.next_power_of_two().trailing_zeros() as usize)
                .min(BUDDY_MAX_ORDER - 1);
            let block_pages = 1usize << order;

            if block_pages > remaining || (offset_pages & (block_pages - 1)) != 0 {
                // Block would straddle alignment boundary — take order 0.
                self.free_block(offset_pages, 0);
                offset_pages += 1;
                remaining -= 1;
            } else {
                self.free_block(offset_pages, order);
                offset_pages += block_pages;
                remaining -= block_pages;
            }
        }

        self.total_pages += page_count;
        self.stats.total_bytes = (self.total_pages * PAGE_SIZE) as u64;
        self.stats.free_bytes  = self.stats.total_bytes;
    }

    /// Allocate `2^order` contiguous pages.  Returns a virtual address pointer
    /// into the HHDM, or `None` if the heap is exhausted.
    ///
    /// The returned memory is *not* zeroed — callers must zero if required.
    pub fn alloc_pages(&mut self, order: usize) -> Option<*mut u8> {
        assert!(order < BUDDY_MAX_ORDER, "order exceeds BUDDY_MAX_ORDER");

        // Walk upward from the requested order until we find a non-empty list.
        let found_order = (order..BUDDY_MAX_ORDER)
            .find(|&k| !self.free_lists[k].is_null())?;

        // Pop the head block from found_order's free list.
        let block_ptr = self.free_lists[found_order];
        // SAFETY: block_ptr is non-null (checked above) and points to a free
        // block whose first 16 bytes we own as FreeBlock metadata.
        let block = unsafe { &*block_ptr };
        self.free_lists[found_order] = block.next;
        if !block.next.is_null() {
            // SAFETY: next is a valid free block we placed on the list.
            unsafe { (*block.next).prev = ptr::null_mut() };
        }

        let page_idx = (block_ptr as usize - self.base) / PAGE_SIZE;
        self.bitmaps[found_order].toggle(page_idx >> (found_order + 1));

        // Split the block down to the requested order, returning the upper
        // half of each split to the free list.
        let mut current_order = found_order;
        let mut current_page  = page_idx;
        while current_order > order {
            current_order -= 1;
            let buddy_page = current_page + (1 << current_order);
            // SAFETY: buddy_page is within the managed region (it was split
            // from a block we just popped from a valid free list).
            unsafe { self.free_block(buddy_page, current_order) };
        }

        let bytes = (1usize << order) * PAGE_SIZE;
        self.stats.used_bytes  += bytes as u64;
        self.stats.free_bytes  -= bytes as u64;
        self.stats.alloc_count += 1;
        if self.stats.used_bytes > self.stats.peak_bytes {
            self.stats.peak_bytes = self.stats.used_bytes;
        }

        Some((self.base + current_page * PAGE_SIZE) as *mut u8)
    }

    /// Deallocate a block previously returned by `alloc_pages`.
    ///
    /// # Safety
    ///
    /// * `ptr` must have been returned by `alloc_pages` with the same `order`.
    /// * The memory must not be accessed after this call.
    pub unsafe fn dealloc_pages(&mut self, ptr: *mut u8, order: usize) {
        let addr = ptr as usize;
        assert!(
            addr >= self.base && (addr - self.base) % PAGE_SIZE == 0,
            "dealloc_pages: address not from this allocator"
        );
        assert!(order < BUDDY_MAX_ORDER, "order exceeds BUDDY_MAX_ORDER");

        let mut page_idx = (addr - self.base) / PAGE_SIZE;
        let mut ord      = order;

        // Coalesce with free buddies, ascending through orders.
        while ord < BUDDY_MAX_ORDER - 1 {
            let buddy_idx = page_idx ^ (1 << ord);    // XOR to find buddy
            let pair_bit  = page_idx >> (ord + 1);

            if self.bitmaps[ord].test(pair_bit) {
                // Buddy is free — remove it from the free list and coalesce.
                let buddy_addr = self.base + buddy_idx * PAGE_SIZE;
                let buddy_ptr  = buddy_addr as *mut FreeBlock;
                // SAFETY: buddy_ptr is a free block we placed on the list.
                let buddy = &*buddy_ptr;
                if !buddy.prev.is_null() {
                    (*buddy.prev).next = buddy.next;
                } else {
                    self.free_lists[ord] = buddy.next;
                }
                if !buddy.next.is_null() {
                    (*buddy.next).prev = buddy.prev;
                }
                self.bitmaps[ord].toggle(pair_bit);
                page_idx = page_idx.min(buddy_idx);   // coalesced block base
                ord += 1;
            } else {
                self.bitmaps[ord].toggle(pair_bit);
                break;
            }
        }

        self.free_block(page_idx, ord);

        let bytes = (1usize << order) * PAGE_SIZE;
        self.stats.used_bytes  -= bytes as u64;
        self.stats.free_bytes  += bytes as u64;
        self.stats.dealloc_count += 1;
    }

    /// Insert a page block into the free list at `order`.
    ///
    /// # Safety
    ///
    /// `page_idx` must refer to `2^order` pages within the managed region
    /// that are genuinely unused and writable.
    unsafe fn free_block(&mut self, page_idx: usize, order: usize) {
        let addr = self.base + page_idx * PAGE_SIZE;
        let node = addr as *mut FreeBlock;
        // Write the free-list node into the first 16 bytes of the block.
        (*node).prev = ptr::null_mut();
        (*node).next = self.free_lists[order];
        if !self.free_lists[order].is_null() {
            (*self.free_lists[order]).prev = node;
        }
        self.free_lists[order] = node;
        self.bitmaps[order].toggle(page_idx >> (order + 1));
    }

    pub fn stats(&self) -> AllocStats { self.stats }
}

// ── Global instance ───────────────────────────────────────────────────────────

/// Kernel-global buddy allocator.  All external callers go through this.
///
/// Interrupt handlers must disable IRQs before calling `lock()` to avoid
/// a scenario where the allocator lock is held on a thread when an IRQ fires
/// and the IRQ handler also tries to allocate — a deadlock.
pub static BUDDY: Mutex<BuddyAllocator> = Mutex::new(BuddyAllocator::new());

/// Convenience: allocate `2^order` contiguous pages from the global buddy.
///
/// # Safety
///
/// See `BuddyAllocator::alloc_pages`.
#[inline]
pub fn alloc_pages(order: usize) -> Option<*mut u8> {
    BUDDY.lock().alloc_pages(order)
}

/// Convenience: deallocate pages back to the global buddy.
///
/// # Safety
///
/// See `BuddyAllocator::dealloc_pages`.
#[inline]
pub unsafe fn dealloc_pages(ptr: *mut u8, order: usize) {
    BUDDY.lock().dealloc_pages(ptr, order);
}
