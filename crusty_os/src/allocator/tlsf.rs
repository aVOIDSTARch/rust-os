//! Two-Level Segregated Fit allocator — O(1) worst-case alloc and dealloc.
//!
//! Backed by the buddy allocator. Registered as `#[global_allocator]` for the
//! boot-multiboot2 and boot-limine builds.

#![allow(clippy::cast_possible_truncation)]

use core::{
    alloc::{GlobalAlloc, Layout},
    ptr,
};
use spin::Mutex;
use framework::PAGE_SIZE;
use super::buddy;

// ── TLSF parameters ───────────────────────────────────────────────────────────

const SL_INDEX_COUNT: usize = 5;
const FL_INDEX_COUNT: usize = 30;
const SL_COUNT:       usize = 1 << SL_INDEX_COUNT;
const BLOCK_MIN:      usize = 32;

// ── Block header ──────────────────────────────────────────────────────────────

const FREE_BIT:      usize = 1;
const PREV_PHYS_BIT: usize = 2;

#[repr(C)]
struct BlockHeader {
    size:      usize,
    prev_phys: *mut BlockHeader,
    next_free: *mut BlockHeader,
    prev_free: *mut BlockHeader,
}

// SAFETY: serialised by Mutex<TlsfAllocator>.
unsafe impl Send for BlockHeader {}

impl BlockHeader {
    #[inline] fn block_size(&self) -> usize { self.size & !(FREE_BIT | PREV_PHYS_BIT) }
    #[inline] fn is_free(&self)   -> bool   { self.size & FREE_BIT != 0 }
    #[inline] fn prev_free(&self) -> bool   { self.size & PREV_PHYS_BIT != 0 }

    #[inline] fn set_size(&mut self, s: usize) {
        self.size = (self.size & (FREE_BIT | PREV_PHYS_BIT)) | s;
    }
    #[inline] fn set_free(&mut self, f: bool) {
        if f { self.size |= FREE_BIT; } else { self.size &= !FREE_BIT; }
    }
    #[inline] fn set_prev_free(&mut self, f: bool) {
        if f { self.size |= PREV_PHYS_BIT; } else { self.size &= !PREV_PHYS_BIT; }
    }

    #[inline]
    unsafe fn payload(&mut self) -> *mut u8 {
        (self as *mut Self as *mut u8).add(core::mem::size_of::<BlockHeader>())
    }

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
    fl_bitmap:  u32,
    sl_bitmap:  [u32; FL_INDEX_COUNT],
    free_lists: [[*mut BlockHeader; SL_COUNT]; FL_INDEX_COUNT],
    pool_base:  *mut u8,
    pool_size:  usize,
}

// SAFETY: protected by Mutex.
unsafe impl Send for TlsfInner {}

impl TlsfInner {
    const fn new() -> Self {
        Self {
            fl_bitmap:  0,
            sl_bitmap:  [0u32; FL_INDEX_COUNT],
            free_lists: [[ptr::null_mut(); SL_COUNT]; FL_INDEX_COUNT],
            pool_base:  ptr::null_mut(),
            pool_size:  0,
        }
    }

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

    #[inline]
    fn mapping_search(size: usize) -> (usize, usize) {
        let round = if size >= (1 << SL_INDEX_COUNT) {
            (1 << (usize::BITS as usize - 1 - size.leading_zeros() as usize - SL_INDEX_COUNT)) - 1
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
        else               { self.free_lists[fl][sl] = next; }
        if !next.is_null() { (*next).prev_free = prev; }
        if self.free_lists[fl][sl].is_null() {
            self.sl_bitmap[fl] &= !(1 << sl);
            if self.sl_bitmap[fl] == 0 { self.fl_bitmap &= !(1 << fl); }
        }
        (*block).set_free(false);
    }

    unsafe fn find_free(&self, size: usize) -> Option<*mut BlockHeader> {
        let (mut fl, mut sl) = Self::mapping_search(size);
        let sl_map = self.sl_bitmap[fl] >> sl;
        if sl_map != 0 {
            sl += sl_map.trailing_zeros() as usize;
        } else {
            let fl_map = self.fl_bitmap >> (fl + 1);
            if fl_map == 0 { return None; }
            fl += 1 + fl_map.trailing_zeros() as usize;
            sl  = self.sl_bitmap[fl].trailing_zeros() as usize;
        }
        Some(self.free_lists[fl][sl])
    }

    unsafe fn alloc(&mut self, size: usize) -> Option<*mut u8> {
        let size = size.max(BLOCK_MIN);
        let size = (size + (core::mem::size_of::<usize>() - 1))
            & !(core::mem::size_of::<usize>() - 1);

        let block      = self.find_free(size)?;
        self.remove_free(block);

        let block_size = (*block).block_size();
        let remainder  = block_size.wrapping_sub(size);

        if remainder >= core::mem::size_of::<BlockHeader>() + BLOCK_MIN {
            (*block).set_size(size);
            let next = (*block).next_phys();
            (*next).set_size(remainder - core::mem::size_of::<BlockHeader>());
            (*next).prev_phys = block;
            (*next).set_prev_free(false);
            self.insert_free(next);
            let after = (*next).next_phys();
            (*after).prev_phys = next;
        }

        let next = (*block).next_phys();
        (*next).set_prev_free(false);

        Some((*block).payload())
    }

    unsafe fn dealloc(&mut self, ptr: *mut u8) {
        let header_size = core::mem::size_of::<BlockHeader>();
        let block       = (ptr as *mut BlockHeader).sub(1);

        let mut merged = block;
        if (*block).prev_free() {
            let prev     = (*block).prev_phys;
            self.remove_free(prev);
            let combined = (*prev).block_size() + header_size + (*block).block_size();
            (*prev).set_size(combined);
            merged = prev;
        }

        let next = (*merged).next_phys();
        if (*next).is_free() {
            self.remove_free(next);
            let combined = (*merged).block_size() + header_size + (*next).block_size();
            (*merged).set_size(combined);
        }

        self.insert_free(merged);
        let after = (*merged).next_phys();
        (*after).set_prev_free(true);
    }

    unsafe fn add_pool(&mut self, mem: *mut u8, size: usize) {
        assert!(
            size > 2 * core::mem::size_of::<BlockHeader>() + BLOCK_MIN,
            "pool too small"
        );

        self.pool_base = mem;
        self.pool_size = size;

        let sentinel_addr = mem.add(size - core::mem::size_of::<BlockHeader>());
        let sentinel      = sentinel_addr as *mut BlockHeader;
        ptr::write(sentinel, BlockHeader {
            size:      0,
            prev_phys: ptr::null_mut(),
            next_free: ptr::null_mut(),
            prev_free: ptr::null_mut(),
        });

        let block        = mem as *mut BlockHeader;
        let payload_size = size - 2 * core::mem::size_of::<BlockHeader>();
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

// ── Public wrapper ────────────────────────────────────────────────────────────

pub struct TlsfAllocator {
    inner: Mutex<TlsfInner>,
}

impl TlsfAllocator {
    pub const fn new() -> Self {
        Self { inner: Mutex::new(TlsfInner::new()) }
    }

    /// Carve a pool of `2^buddy_order` pages from the buddy and initialise TLSF.
    ///
    /// # Safety
    /// Must be called exactly once, after the buddy is populated.
    pub unsafe fn init(&self, buddy_order: usize) {
        let size = PAGE_SIZE << buddy_order;
        let mem  = buddy::alloc_pages(buddy_order)
            .expect("TLSF init: buddy OOM — reduce buddy_order or add more regions");
        self.inner.lock().add_pool(mem, size);
    }
}

// SAFETY: interior mutability serialised by Mutex.
unsafe impl Send for TlsfAllocator {}
unsafe impl Sync for TlsfAllocator {}

unsafe impl GlobalAlloc for TlsfAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let size  = layout.size().max(BLOCK_MIN);
        let extra = if layout.align() > core::mem::size_of::<usize>() {
            layout.align() - 1
        } else { 0 };

        match self.inner.lock().alloc(size + extra) {
            None      => ptr::null_mut(),
            Some(ptr) => {
                let aligned = (ptr as usize + extra) & !(layout.align() - 1);
                aligned as *mut u8
            }
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
        self.inner.lock().dealloc(ptr);
    }
}

// TLSF instance is defined in the parent module (allocator::TLSF) as the
// #[global_allocator].  Nothing to define here.
