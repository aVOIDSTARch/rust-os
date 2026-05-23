//! Typed slab allocator backed by the buddy allocator.
//!
//! A `SlabCache<T>` holds a collection of slabs, where each slab is a single
//! buddy-allocated page block partitioned into `T`-sized slots. Free slots are
//! chained through a per-slab index free list.
//!
//! Empty slabs are returned to the buddy allocator immediately.

use core::{
    marker::PhantomData,
    mem,
    ptr::{self, NonNull},
};
use spin::Mutex;
use framework::{AllocStats, PAGE_SIZE};
use crate::buddy;

// ── Slab header (stored at end of slab to keep start aligned) ─────────────────

#[repr(C)]
struct SlabHeader {
    free_head: u16,
    in_use:    u16,
    capacity:  u16,
    _pad:      u16,
    prev:      *mut SlabHeader,
    next:      *mut SlabHeader,
}

// SAFETY: protected by Mutex<SlabCacheInner<T>>.
unsafe impl Send for SlabHeader {}

impl SlabHeader {
    fn is_full(&self)  -> bool { self.in_use == self.capacity }
    fn is_empty(&self) -> bool { self.in_use == 0 }
}

// ── SlabCacheInner ────────────────────────────────────────────────────────────

struct SlabCacheInner<T> {
    slab_order:    usize,
    objs_per_slab: usize,
    partial:       *mut SlabHeader,
    stats:         AllocStats,
    _marker:       PhantomData<T>,
}

// SAFETY: protected by Mutex.
unsafe impl<T: Send> Send for SlabCacheInner<T> {}

impl<T> SlabCacheInner<T> {
    const fn compute(order: usize) -> (usize, usize) {
        let slab_bytes  = PAGE_SIZE << order;
        let obj_size    = mem::size_of::<T>();
        let obj_align   = mem::align_of::<T>();
        let header_size = mem::size_of::<SlabHeader>();
        let usable      = (slab_bytes - header_size) & !(obj_align - 1);
        let capacity    = if obj_size == 0 { 0 } else { usable / obj_size };
        (capacity, slab_bytes)
    }

    const fn new(slab_order: usize) -> Self {
        let (objs_per_slab, _) = Self::compute(slab_order);
        Self {
            slab_order,
            objs_per_slab,
            partial: ptr::null_mut(),
            stats:   AllocStats {
                total_bytes:   0,
                used_bytes:    0,
                free_bytes:    0,
                alloc_count:   0,
                dealloc_count: 0,
                peak_bytes:    0,
            },
            _marker: PhantomData,
        }
    }

    unsafe fn alloc(&mut self) -> Option<NonNull<T>> {
        if self.partial.is_null() {
            self.grow()?;
        }

        let header   = &mut *self.partial;
        let slab_base = header as *mut SlabHeader as usize
            - (self.objs_per_slab * mem::size_of::<T>());
        let obj_size  = mem::size_of::<T>();
        let slot_idx  = header.free_head as usize;

        let slot_ptr    = (slab_base + slot_idx * obj_size) as *mut u16;
        header.free_head = ptr::read(slot_ptr);
        header.in_use   += 1;

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

    unsafe fn dealloc(&mut self, ptr: NonNull<T>) {
        let obj_ptr  = ptr.as_ptr();
        let obj_size = mem::size_of::<T>();

        let slab_bytes  = PAGE_SIZE << self.slab_order;
        let slab_base   = (obj_ptr as usize) & !(slab_bytes - 1);
        let header_addr = slab_base + self.objs_per_slab * obj_size;
        let header      = &mut *(header_addr as *mut SlabHeader);

        let was_full = header.is_full();
        let slot_idx = (obj_ptr as usize - slab_base) / obj_size;
        ptr::write(obj_ptr as *mut u16, header.free_head);
        header.free_head = slot_idx as u16;
        header.in_use   -= 1;

        if was_full {
            self.link_partial(header as *mut SlabHeader);
        } else if header.is_empty() {
            self.unlink_partial(header as *mut SlabHeader);
            buddy::dealloc_pages(slab_base as *mut u8, self.slab_order);
            self.stats.total_bytes -= slab_bytes as u64;
            self.stats.free_bytes  -= slab_bytes as u64;
        }

        self.stats.dealloc_count += 1;
        self.stats.used_bytes    -= obj_size as u64;
        self.stats.free_bytes    += obj_size as u64;
    }

    fn grow(&mut self) -> Option<()> {
        let slab_bytes = PAGE_SIZE << self.slab_order;
        let obj_size   = mem::size_of::<T>();

        let raw       = buddy::alloc_pages(self.slab_order)?;
        let slab_base = raw as usize;

        for i in 0..self.objs_per_slab {
            let slot = (slab_base + i * obj_size) as *mut u16;
            let next = if i + 1 < self.objs_per_slab { (i + 1) as u16 } else { u16::MAX };
            unsafe { ptr::write(slot, next) };
        }

        let header_addr = slab_base + self.objs_per_slab * obj_size;
        let header      = header_addr as *mut SlabHeader;
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
        if !self.partial.is_null() { (*self.partial).prev = header; }
        self.partial = header;
    }

    unsafe fn unlink_partial(&mut self, header: *mut SlabHeader) {
        let h = &*header;
        if !h.prev.is_null() { (*h.prev).next = h.next; }
        else                  { self.partial = h.next; }
        if !h.next.is_null() { (*h.next).prev = h.prev; }
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

pub struct SlabCache<T: Send> {
    inner: Mutex<SlabCacheInner<T>>,
}

impl<T: Send> SlabCache<T> {
    pub const fn new(slab_order: usize) -> Self {
        Self { inner: Mutex::new(SlabCacheInner::new(slab_order)) }
    }

    pub fn alloc(&self) -> Option<NonNull<T>> {
        unsafe { self.inner.lock().alloc() }
    }

    /// # Safety
    /// `ptr` must originate from `self.alloc()` and the object's destructor
    /// must have been called before invoking this.
    pub unsafe fn dealloc(&self, ptr: NonNull<T>) {
        self.inner.lock().dealloc(ptr);
    }

    pub fn stats(&self) -> AllocStats {
        self.inner.lock().stats
    }
}

// ── Compile-time compatibility check ─────────────────────────────────────────

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
