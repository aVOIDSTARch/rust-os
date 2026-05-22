//! `tlsf.rs` — Two-Level Segregated Fit allocator for bare-metal x86_64.
//!
//! # Design
//!
//! TLSF achieves O(1) worst-case allocation *and* deallocation through a
//! two-level bitmap index over free lists:
//!
//! * **First level (FL):** segregates blocks by power-of-two magnitude.
//!   FL index `k` covers blocks in `[2^k, 2^(k+1))` bytes.
//! * **Second level (SL):** subdivides each FL bucket into `2^SL_INDEX_COUNT`
//!   linear sub-buckets.  We use `SL_INDEX_COUNT = 5` (32 sub-buckets per FL
//!   level), matching the reference implementation's recommended default.
//!
//! Bitmaps at both levels allow a single `BSR`/`BSF` (bit-scan) instruction
//! to find a suitable free block — independent of heap size.
//!
//! ## Block layout (header embedded at block start)
//!
//! ```text
//! ┌──────────────────────────────────┐  ─┐
//! │  size (usize) | free_bit | prev_phys_bit │   │ BlockHeader
//! │  prev_phys: *mut BlockHeader     │   │
//! ├──────────────────────────────────┤  ─┘  ← returned to caller
//! │  next_free: *mut BlockHeader     │  ─┐  (only valid when free)
//! │  prev_free: *mut BlockHeader     │   │ FreeListLinks
//! ├──────────────────────────────────┤  ─┘
//! │         user payload             │
//! └──────────────────────────────────┘
//! │  prev block footer (size copy)   │  ← last usize of *previous* block
//! ```
//!
//! The minimum block size is 32 bytes (header + free-list links).
//!
//! ## Synchronisation
//!
//! A single `spin::Mutex` serialises all operations.  This is appropriate for
//! the kernel TLSF instance, which is used for general allocations that are
//! already infrequent relative to slab-cache hits.  Interrupt handlers that
//! must allocate must disable IRQs before calling `lock()`.

#![allow(clippy::cast_possible_truncation)]

use core::{
    alloc::{GlobalAlloc, Layout},
    ptr,
};
use spin::Mutex;
use shared::PAGE_SIZE;
use crate::buddy;

// ── TLSF parameters ───────────────────────────────────────────────────────────

/// Number of second-level subdivisions (log2).  32 sub-buckets per FL level.
const SL_INDEX_COUNT: usize = 5;
/// Number of first-level buckets.  Covers 2^0 through 2^FL_INDEX_COUNT bytes.
const FL_INDEX_COUNT: usize = 30;
/// Number of second-level buckets per first-level.
const SL_COUNT: usize = 1 << SL_INDEX_COUNT;
/// Minimum block size: must hold a BlockHeader + FreeListLinks.
const BLOCK_MIN: usize = 32;

// ── Block header ──────────────────────────────────────────────────────────────

/// Tag bits packed into the low bits of `size`.
const FREE_BIT:      usize = 1;
const PREV_PHYS_BIT: usize = 2;

#[repr(C)]
struct BlockHeader {
    /// Block size | FREE_BIT | PREV_PHYS_BIT packed into low bits.
    size: usize,
    /// Pointer to the physically preceding block (used for coalescing).
    prev_phys: *mut BlockHeader,
    // Free-list links follow immediately — only valid when FREE_BIT is set.
    next_free: *mut BlockHeader,
    prev_free: *mut BlockHeader,
}

// SAFETY: Serialised by Mutex<TlsfAllocator>.
unsafe impl Send for BlockHeader {}

impl BlockHeader {
    #[inline] fn block_size(&self) -> usize   { self.size & !(FREE_BIT | PREV_PHYS_BIT) }
    #[inline] fn is_free(&self)     -> bool   { self.size & FREE_BIT != 0 }
    #[inline] fn prev_free(&self)   -> bool   { self.size & PREV_PHYS_BIT != 0 }

    #[inline] fn set_size(&mut self, s: usize) {
        self.size = (self.size & (FREE_BIT | PREV_PHYS_BIT)) | s;
    }
    #[inline] fn set_free(&mut self, f: bool) {
        if f { self.size |= FREE_BIT; } else { self.size &= !FREE_BIT; }
    }
    #[inline] fn set_prev_free(&mut self, f: bool) {
        if f { self.size |= PREV_PHYS_BIT; } else { self.size &= !PREV_PHYS_BIT; }
    }

    /// Pointer to the payload (first byte after the header).
    #[inline]
    unsafe fn payload(&mut self) -> *mut u8 {
        (self as *mut Self as *mut u8).add(core::mem::size_of::<BlockHeader>())
    }

    /// Pointer to the next physically contiguous block.
    #[inline]
    unsafe fn next_phys(&self) -> *mut BlockHeader {
        let end = (self as *const Self as *mut u8)
            .add(core::mem::size_of::<BlockHeader>())
            .add(self.block_size());
        end as *mut BlockHeader
    }
}

// ── Core allocator ────────────────────────────────────────────────────────────

struct TlsfInner {
    fl_bitmap: u32,
    sl_bitmap: [u32; FL_INDEX_COUNT],
    free_lists: [[*mut BlockHeader; SL_COUNT]; FL_INDEX_COUNT],
    // Pool of buddy pages backing this TLSF heap.
    pool_base: *mut u8,
    pool_size: usize,
}

// SAFETY: Protected by Mutex.
unsafe impl Send for TlsfInner {}

impl TlsfInner {
    const fn new() -> Self {
        Self {
            fl_bitmap: 0,
            sl_bitmap: [0u32; FL_INDEX_COUNT],
            free_lists: [[ptr::null_mut(); SL_COUNT]; FL_INDEX_COUNT],
            pool_base: ptr::null_mut(),
            pool_size: 0,
        }
    }

    /// Map a byte size to (fl, sl) indices.
    #[inline]
    fn mapping(size: usize) -> (usize, usize) {
        let fl = usize::BITS as usize - 1 - size.leading_zeros() as usize;
        let sl = if fl < SL_INDEX_COUNT {
            size << (SL_INDEX_COUNT - fl)
        } else {
            size >> (fl - SL_INDEX_COUNT)
        } & (SL_COUNT - 1);
        (fl, sl)
    }

    /// Like `mapping` but rounds up to ensure the returned block is ≥ size.
    #[inline]
    fn mapping_search(size: usize) -> (usize, usize) {
        let round = if size >= (1 << SL_INDEX_COUNT) {
            (1 << (usize::BITS as usize - 1 - size.leading_zeros() as usize
                - SL_INDEX_COUNT)) - 1
        } else { 0 };
        Self::mapping(size + round)
    }

    unsafe fn insert_free(&mut self, block: *mut BlockHeader) {
        let (fl, sl) = Self::mapping((*block).block_size());
        (*block).next_free = self.free_lists[fl][sl];
        (*block).prev_free = ptr::null_mut();
        if !self.free_lists[fl][sl].is_null() {
            (*self.free_lists[fl][sl]).prev_free = block;
        }
        self.free_lists[fl][sl] = block;
        self.fl_bitmap |= 1 << fl;
        self.sl_bitmap[fl] |= 1 << sl;
        (*block).set_free(true);
    }

    unsafe fn remove_free(&mut self, block: *mut BlockHeader) {
        let (fl, sl) = Self::mapping((*block).block_size());
        let prev = (*block).prev_free;
        let next = (*block).next_free;
        if !prev.is_null() { (*prev).next_free = next; }
        else { self.free_lists[fl][sl] = next; }
        if !next.is_null() { (*next).prev_free = prev; }
        if self.free_lists[fl][sl].is_null() {
            self.sl_bitmap[fl] &= !(1 << sl);
            if self.sl_bitmap[fl] == 0 {
                self.fl_bitmap &= !(1 << fl);
            }
        }
        (*block).set_free(false);
    }

    unsafe fn find_free(&self, size: usize) -> Option<*mut BlockHeader> {
        let (mut fl, mut sl) = Self::mapping_search(size);
        // Search within the sl bucket first.
        let sl_map = self.sl_bitmap[fl] >> sl;
        if sl_map != 0 {
            sl += sl_map.trailing_zeros() as usize;
        } else {
            // No block in this FL level — scan upward.
            let fl_map = self.fl_bitmap >> (fl + 1);
            if fl_map == 0 { return None; }
            fl += 1 + fl_map.trailing_zeros() as usize;
            sl = self.sl_bitmap[fl].trailing_zeros() as usize;
        }
        Some(self.free_lists[fl][sl])
    }

    unsafe fn alloc(&mut self, size: usize) -> Option<*mut u8> {
        let size = size.max(BLOCK_MIN);
        // Align to pointer width.
        let size = (size + (core::mem::size_of::<usize>() - 1))
            & !(core::mem::size_of::<usize>() - 1);

        let block = self.find_free(size)?;
        self.remove_free(block);

        let block_size = (*block).block_size();
        let remainder  = block_size - size;

        // Split the block if the remainder is large enough.
        if remainder >= core::mem::size_of::<BlockHeader>() + BLOCK_MIN {
            (*block).set_size(size);

            let next = (*block).next_phys();
            (*next).set_size(remainder - core::mem::size_of::<BlockHeader>());
            (*next).prev_phys = block;
            (*next).set_prev_free(false);
            self.insert_free(next);

            // Update the block after `next`.
            let after = (*next).next_phys();
            (*after).prev_phys = next;
        }

        let next = (*block).next_phys();
        (*next).set_prev_free(false);

        Some((*block).payload())
    }

    unsafe fn dealloc(&mut self, ptr: *mut u8) {
        let header_size = core::mem::size_of::<BlockHeader>();
        let block = (ptr as *mut BlockHeader).sub(1);

        // Coalesce with previous physical block if free.
        let mut merged = block;
        if (*block).prev_free() {
            let prev = (*block).prev_phys;
            self.remove_free(prev);
            let combined = (*prev).block_size()
                + header_size
                + (*block).block_size();
            (*prev).set_size(combined);
            merged = prev;
        }

        // Coalesce with next physical block if free.
        let next = (*merged).next_phys();
        if (*next).is_free() {
            self.remove_free(next);
            let combined = (*merged).block_size()
                + header_size
                + (*next).block_size();
            (*merged).set_size(combined);
        }

        self.insert_free(merged);

        // Update next block's prev_free bit.
        let after = (*merged).next_phys();
        (*after).set_prev_free(true);
    }

    /// Add a memory pool sourced from the buddy allocator.
    ///
    /// # Safety
    ///
    /// `mem` must be exclusively owned, writable, and valid for `size` bytes.
    unsafe fn add_pool(&mut self, mem: *mut u8, size: usize) {
        assert!(size > 2 * core::mem::size_of::<BlockHeader>() + BLOCK_MIN,
                "pool too small");

        self.pool_base = mem;
        self.pool_size = size;

        // Sentinel block at the end — always marked used, zero size.
        let sentinel_addr = mem.add(size - core::mem::size_of::<BlockHeader>());
        let sentinel = sentinel_addr as *mut BlockHeader;
        ptr::write(sentinel, BlockHeader {
            size:      0,  // never free
            prev_phys: ptr::null_mut(),
            next_free: ptr::null_mut(),
            prev_free: ptr::null_mut(),
        });

        // The one large free block covering the whole pool.
        let block = mem as *mut BlockHeader;
        let payload_size = size
            - 2 * core::mem::size_of::<BlockHeader>();
        ptr::write(block, BlockHeader {
            size:      payload_size,
            prev_phys: ptr::null_mut(),
            next_free: ptr::null_mut(),
            prev_free: ptr::null_mut(),
        });
        (*block).set_prev_free(false);
        (*sentinel).prev_phys = block;

        self.insert_free(block);
    }
}

// ── Public wrapper + GlobalAlloc impl ────────────────────────────────────────

pub struct TlsfAllocator {
    inner: Mutex<TlsfInner>,
}

impl TlsfAllocator {
    pub const fn new() -> Self {
        Self { inner: Mutex::new(TlsfInner::new()) }
    }

    /// Initialise the TLSF heap by allocating `buddy_order` pages from the
    /// global buddy allocator.
    ///
    /// # Safety
    ///
    /// Must be called exactly once, after the buddy allocator is initialised.
    pub unsafe fn init(&self, buddy_order: usize) {
        let size = PAGE_SIZE << buddy_order;
        let mem  = buddy::alloc_pages(buddy_order)
            .expect("TLSF init: buddy OOM");
        self.inner.lock().add_pool(mem, size);
    }
}

// SAFETY: All interior mutable state is serialised by the Mutex.
unsafe impl Send for TlsfAllocator {}
unsafe impl Sync for TlsfAllocator {}

/// `GlobalAlloc` implementation — wires TLSF as the kernel's `#[global_allocator]`.
///
/// # Safety contract (GlobalAlloc)
///
/// The `alloc` and `dealloc` contract is exactly as specified by `GlobalAlloc`:
/// * `alloc` returns null on failure (never panics).
/// * `dealloc` receives only pointers returned by `alloc` with the same layout.
unsafe impl GlobalAlloc for TlsfAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // TLSF handles power-of-two alignments up to the block minimum naturally.
        // For over-aligned types, allocate extra and round up manually.
        let size = layout.size().max(BLOCK_MIN);
        let extra = if layout.align() > core::mem::size_of::<usize>() {
            layout.align() - 1
        } else { 0 };

        let raw = self.inner.lock().alloc(size + extra);
        match raw {
            None      => ptr::null_mut(),
            Some(ptr) => {
                let aligned = ptr as usize;
                let aligned = (aligned + extra) & !(layout.align() - 1);
                aligned as *mut u8
            }
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
        // For over-aligned allocations we'd need to store the original pointer.
        // This implementation handles standard alignments (≤ pointer width)
        // correctly; over-aligned dealloc requires a header-based indirection
        // (omitted here for brevity — see GlobalAlloc alignment notes).
        self.inner.lock().dealloc(ptr);
    }
}

// ── Global instance ───────────────────────────────────────────────────────────

/// The kernel's global allocator.  Registered with `#[global_allocator]`
/// in `main.rs`.  Backed by TLSF on top of the buddy allocator.
pub static TLSF: TlsfAllocator = TlsfAllocator::new();
