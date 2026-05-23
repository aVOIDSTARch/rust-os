//! Binary buddy allocator for x86_64 bare-metal.
//!
//! Manages a contiguous address space (physical or virtual) as a binary buddy
//! system. Blocks are always 2^k pages in size (k = 0..BUDDY_MAX_ORDER-1).
//! Each order maintains a doubly-linked free list threaded through the blocks
//! themselves (no external node storage), plus a companion bitmap where bit `i`
//! tracks whether block pair `i` has one free buddy.
//!
//! A `spin::Mutex` guards the entire structure. Interrupt handlers that
//! allocate pages must disable interrupts before acquiring the lock.

use core::ptr;
use spin::Mutex;
use framework::{AllocStats, BUDDY_MAX_ORDER, PAGE_SIZE};

// ── Free-list node ────────────────────────────────────────────────────────────

/// Overlay written into the first 16 bytes of every free page-block.
#[repr(C)]
struct FreeBlock {
    prev: *mut FreeBlock,
    next: *mut FreeBlock,
}

// SAFETY: sole owner of free blocks; access serialised by Mutex<BuddyAllocator>.
unsafe impl Send for FreeBlock {}

// ── Bitmap ────────────────────────────────────────────────────────────────────

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

// ── Core allocator ────────────────────────────────────────────────────────────

const MAX_PAGES: usize = 1 << BUDDY_MAX_ORDER;

pub struct BuddyAllocator {
    base:       usize,
    total_pages: usize,
    free_lists: [*mut FreeBlock; BUDDY_MAX_ORDER],
    bitmaps:    [Bitmap; BUDDY_MAX_ORDER],
    stats:      AllocStats,
}

// SAFETY: guarded by Mutex<BuddyAllocator>.
unsafe impl Send for BuddyAllocator {}

impl BuddyAllocator {
    pub const fn new() -> Self {
        Self {
            base:        0,
            total_pages: 0,
            free_lists:  [ptr::null_mut(); BUDDY_MAX_ORDER],
            bitmaps:     [const { Bitmap::new() }; BUDDY_MAX_ORDER],
            stats:       AllocStats {
                total_bytes:   0,
                used_bytes:    0,
                free_bytes:    0,
                alloc_count:   0,
                dealloc_count: 0,
                peak_bytes:    0,
            },
        }
    }

    /// Register a physical memory region as a virtual address range under HHDM.
    ///
    /// # Safety
    /// `virt_base` must be page-aligned, point to genuinely free writable RAM
    /// mapped into the kernel address space, and not be aliased elsewhere.
    pub unsafe fn add_region(&mut self, virt_base: usize, page_count: usize) {
        assert!(virt_base % PAGE_SIZE == 0, "region base must be page-aligned");
        assert!(page_count > 0, "empty region");

        if self.base == 0 {
            self.base = virt_base;
        }
        assert!(virt_base >= self.base, "region base precedes allocator base");

        let mut remaining    = page_count;
        let mut offset_pages = (virt_base - self.base) / PAGE_SIZE;

        while remaining > 0 {
            let order       = (remaining.next_power_of_two().trailing_zeros() as usize)
                                  .min(BUDDY_MAX_ORDER - 1);
            let block_pages = 1usize << order;

            if block_pages > remaining || (offset_pages & (block_pages - 1)) != 0 {
                self.free_block(offset_pages, 0);
                offset_pages += 1;
                remaining    -= 1;
            } else {
                self.free_block(offset_pages, order);
                offset_pages += block_pages;
                remaining    -= block_pages;
            }
        }

        self.total_pages    += page_count;
        self.stats.total_bytes = (self.total_pages * PAGE_SIZE) as u64;
        self.stats.free_bytes  = self.stats.total_bytes - self.stats.used_bytes;
    }

    /// Allocate `2^order` contiguous pages. Returns a virtual address or `None`.
    pub fn alloc_pages(&mut self, order: usize) -> Option<*mut u8> {
        assert!(order < BUDDY_MAX_ORDER, "order exceeds BUDDY_MAX_ORDER");

        let found_order = (order..BUDDY_MAX_ORDER)
            .find(|&k| !self.free_lists[k].is_null())?;

        let block_ptr = self.free_lists[found_order];
        let block     = unsafe { &*block_ptr };
        self.free_lists[found_order] = block.next;
        if !block.next.is_null() {
            unsafe { (*block.next).prev = ptr::null_mut() };
        }

        let page_idx = (block_ptr as usize - self.base) / PAGE_SIZE;
        self.bitmaps[found_order].toggle(page_idx >> (found_order + 1));

        let mut current_order = found_order;
        let mut current_page  = page_idx;
        while current_order > order {
            current_order -= 1;
            let buddy_page = current_page + (1 << current_order);
            unsafe { self.free_block(buddy_page, current_order) };
        }

        let bytes              = (1usize << order) * PAGE_SIZE;
        self.stats.used_bytes += bytes as u64;
        self.stats.free_bytes -= bytes as u64;
        self.stats.alloc_count += 1;
        if self.stats.used_bytes > self.stats.peak_bytes {
            self.stats.peak_bytes = self.stats.used_bytes;
        }

        Some((self.base + current_page * PAGE_SIZE) as *mut u8)
    }

    /// Deallocate a block previously returned by `alloc_pages`.
    ///
    /// # Safety
    /// `ptr` must originate from `alloc_pages` at the same `order`.
    pub unsafe fn dealloc_pages(&mut self, ptr: *mut u8, order: usize) {
        let addr = ptr as usize;
        assert!(
            addr >= self.base && (addr - self.base) % PAGE_SIZE == 0,
            "dealloc_pages: address not from this allocator"
        );
        assert!(order < BUDDY_MAX_ORDER, "order exceeds BUDDY_MAX_ORDER");

        let mut page_idx = (addr - self.base) / PAGE_SIZE;
        let mut ord      = order;

        while ord < BUDDY_MAX_ORDER - 1 {
            let buddy_idx = page_idx ^ (1 << ord);
            let pair_bit  = page_idx >> (ord + 1);

            if self.bitmaps[ord].test(pair_bit) {
                let buddy_addr = self.base + buddy_idx * PAGE_SIZE;
                let buddy_ptr  = buddy_addr as *mut FreeBlock;
                let buddy      = &*buddy_ptr;
                if !buddy.prev.is_null() { (*buddy.prev).next = buddy.next; }
                else                     { self.free_lists[ord] = buddy.next; }
                if !buddy.next.is_null() { (*buddy.next).prev = buddy.prev; }
                self.bitmaps[ord].toggle(pair_bit);
                page_idx = page_idx.min(buddy_idx);
                ord     += 1;
            } else {
                break;
            }
        }

        self.free_block(page_idx, ord);

        let bytes               = (1usize << order) * PAGE_SIZE;
        self.stats.used_bytes  -= bytes as u64;
        self.stats.free_bytes  += bytes as u64;
        self.stats.dealloc_count += 1;
    }

    unsafe fn free_block(&mut self, page_idx: usize, order: usize) {
        let addr = self.base + page_idx * PAGE_SIZE;
        let node = addr as *mut FreeBlock;
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

pub static BUDDY: Mutex<BuddyAllocator> = Mutex::new(BuddyAllocator::new());

#[inline]
pub fn alloc_pages(order: usize) -> Option<*mut u8> {
    BUDDY.lock().alloc_pages(order)
}

/// # Safety
/// `ptr` must originate from `alloc_pages` at the same `order`.
#[inline]
pub unsafe fn dealloc_pages(ptr: *mut u8, order: usize) {
    BUDDY.lock().dealloc_pages(ptr, order);
}

// ── Unit tests ────────────────────────────────────────────────────────────────
//
// Each test constructs a fresh `BuddyAllocator` (local variable, no global
// state) and feeds it a portion of a page-aligned static array.  Tests run
// sequentially on bare metal; reusing the same backing memory across tests is
// safe because `add_region` overwrites the FreeBlock nodes on entry.

#[cfg(test)]
mod tests {
    use super::*;

    #[repr(align(4096))]
    struct PageAligned([u8; PAGE_SIZE * 16]);

    static mut TEST_MEM: PageAligned = PageAligned([0u8; PAGE_SIZE * 16]);

    fn test_base() -> usize {
        unsafe { core::ptr::addr_of!(TEST_MEM) as usize }
    }

    #[test_case]
    fn alloc_and_dealloc_single_page() {
        let mut buddy = BuddyAllocator::new();
        let base = test_base();
        unsafe {
            buddy.add_region(base, 4);
            let ptr = buddy.alloc_pages(0).expect("order-0 alloc failed");
            assert!(ptr as usize >= base);
            assert_eq!(buddy.stats().alloc_count, 1);
            buddy.dealloc_pages(ptr, 0);
            assert_eq!(buddy.stats().dealloc_count, 1);
            assert_eq!(buddy.stats().used_bytes, 0);
        }
    }

    #[test_case]
    fn alloc_order1_spans_two_pages() {
        let mut buddy = BuddyAllocator::new();
        let base = test_base();
        unsafe {
            buddy.add_region(base, 4);
            let ptr = buddy.alloc_pages(1).expect("order-1 alloc failed");
            assert_eq!(buddy.stats().used_bytes, (2 * PAGE_SIZE) as u64);
            buddy.dealloc_pages(ptr, 1);
            assert_eq!(buddy.stats().used_bytes, 0);
        }
    }

    #[test_case]
    fn two_order0_allocs_do_not_overlap() {
        let mut buddy = BuddyAllocator::new();
        let base = test_base();
        unsafe {
            buddy.add_region(base, 4);
            let p0 = buddy.alloc_pages(0).expect("first order-0 alloc");
            let p1 = buddy.alloc_pages(0).expect("second order-0 alloc");
            // They must be at least PAGE_SIZE apart (non-overlapping).
            let diff = (p0 as isize - p1 as isize).unsigned_abs();
            assert!(diff >= PAGE_SIZE, "allocations overlap: p0={p0:?} p1={p1:?}");
            buddy.dealloc_pages(p0, 0);
            buddy.dealloc_pages(p1, 0);
        }
    }

    #[test_case]
    fn coalescing_restores_higher_order_block() {
        let mut buddy = BuddyAllocator::new();
        let base = test_base();
        unsafe {
            buddy.add_region(base, 4);
            // Alloc two adjacent order-1 blocks, consuming all 4 pages.
            let a = buddy.alloc_pages(1).expect("first order-1 alloc");
            let b = buddy.alloc_pages(1).expect("second order-1 alloc");
            assert!(buddy.alloc_pages(0).is_none(), "heap should be full");

            // Dealloc both; the allocator must coalesce them back.
            buddy.dealloc_pages(a, 1);
            buddy.dealloc_pages(b, 1);

            // After coalescing, an order-2 (4-page) alloc must succeed.
            let full = buddy.alloc_pages(2).expect("order-2 alloc after coalesce");
            buddy.dealloc_pages(full, 2);
        }
    }

    #[test_case]
    fn oom_returns_none() {
        let mut buddy = BuddyAllocator::new();
        let base = test_base();
        unsafe {
            buddy.add_region(base, 1); // exactly one page
            let _p = buddy.alloc_pages(0).expect("first alloc must succeed");
            assert!(buddy.alloc_pages(0).is_none(), "second alloc must fail (OOM)");
        }
    }

    #[test_case]
    fn stats_track_used_and_free_bytes() {
        let mut buddy = BuddyAllocator::new();
        let base = test_base();
        unsafe {
            buddy.add_region(base, 4);
            let initial_free = buddy.stats().free_bytes;
            assert_eq!(buddy.stats().used_bytes, 0);

            let p = buddy.alloc_pages(0).expect("alloc");
            assert_eq!(buddy.stats().used_bytes, PAGE_SIZE as u64);
            assert_eq!(buddy.stats().free_bytes, initial_free - PAGE_SIZE as u64);
            assert_eq!(buddy.stats().alloc_count, 1);

            buddy.dealloc_pages(p, 0);
            assert_eq!(buddy.stats().used_bytes, 0);
            assert_eq!(buddy.stats().free_bytes, initial_free);
            assert_eq!(buddy.stats().dealloc_count, 1);
        }
    }

    #[test_case]
    fn peak_bytes_tracks_high_water_mark() {
        let mut buddy = BuddyAllocator::new();
        let base = test_base();
        unsafe {
            buddy.add_region(base, 4);
            let p0 = buddy.alloc_pages(0).expect("alloc p0");
            let p1 = buddy.alloc_pages(0).expect("alloc p1");
            let peak_after_two = buddy.stats().peak_bytes;
            assert_eq!(peak_after_two, 2 * PAGE_SIZE as u64);

            buddy.dealloc_pages(p1, 0);
            // Peak must not decrease after dealloc.
            assert_eq!(buddy.stats().peak_bytes, peak_after_two);

            buddy.dealloc_pages(p0, 0);
            assert_eq!(buddy.stats().peak_bytes, peak_after_two);
        }
    }
}
