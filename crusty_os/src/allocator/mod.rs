//! Heap allocator stack — thin re-export of the `abalone` allocator crate.
//!
//! All allocator implementations live in `abalone`.  This module:
//!   - Re-exports the sub-modules so `crusty_os::allocator::buddy::…` paths
//!     continue to work from the rest of the kernel and from integration tests.
//!   - Declares the `#[global_allocator]` statics (feature-gated).
//!   - Provides `init_heap` for the `use-bootloader` path (requires x86_64
//!     page-table machinery not available in `abalone`).
//!
//! ## Allocator selection
//!
//! | Feature                           | Global allocator                     |
//! |-----------------------------------|--------------------------------------|
//! | `boot-multiboot2` / `boot-limine` | [`abalone::tlsf::TlsfAllocator`] backed by [`abalone::buddy`] |
//! | `use-bootloader` (legacy)         | [`abalone::bump::BumpAllocator`]     |

pub use abalone::{buddy, slab, tlsf, bump, linked_list, Locked, align_up};

// ── Buddy allocator unit tests ────────────────────────────────────────────────
//
// Tests live here (not in abalone) because they depend on the custom bare-metal
// test framework (bootimage + QEMU exit device) that only crusty_os provides.

#[cfg(test)]
mod buddy_tests {
    use abalone::buddy::BuddyAllocator;
    use framework::PAGE_SIZE;

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
            let a = buddy.alloc_pages(1).expect("first order-1 alloc");
            let b = buddy.alloc_pages(1).expect("second order-1 alloc");
            assert!(buddy.alloc_pages(0).is_none(), "heap should be full");
            buddy.dealloc_pages(a, 1);
            buddy.dealloc_pages(b, 1);
            let full = buddy.alloc_pages(2).expect("order-2 alloc after coalesce");
            buddy.dealloc_pages(full, 2);
        }
    }

    #[test_case]
    fn oom_returns_none() {
        let mut buddy = BuddyAllocator::new();
        let base = test_base();
        unsafe {
            buddy.add_region(base, 1);
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
            assert_eq!(buddy.stats().peak_bytes, peak_after_two);
            buddy.dealloc_pages(p0, 0);
            assert_eq!(buddy.stats().peak_bytes, peak_after_two);
        }
    }
}

// ── Global allocator (boot-multiboot2 / boot-limine) ─────────────────────────

#[cfg(any(feature = "boot-multiboot2", feature = "boot-limine"))]
#[global_allocator]
pub static TLSF: abalone::tlsf::TlsfAllocator = abalone::tlsf::TlsfAllocator::new();

// ── Global allocator (legacy use-bootloader path) ─────────────────────────────

#[cfg(feature = "use-bootloader")]
use x86_64::{
    structures::paging::{
        mapper::MapToError, FrameAllocator, Mapper, Page, PageTableFlags, Size4KiB,
    },
    VirtAddr,
};

#[cfg(feature = "use-bootloader")]
pub const HEAP_START: usize = 0x_4444_4444_0000;
#[cfg(feature = "use-bootloader")]
pub const HEAP_SIZE:  usize = 100 * 1024;

#[cfg(feature = "use-bootloader")]
#[global_allocator]
static ALLOCATOR: Locked<abalone::bump::BumpAllocator> = Locked::new(abalone::bump::BumpAllocator::new());

#[cfg(feature = "use-bootloader")]
pub fn init_heap(
    mapper:          &mut impl Mapper<Size4KiB>,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
) -> Result<(), MapToError<Size4KiB>> {
    let page_range = {
        let heap_start      = VirtAddr::new(HEAP_START as u64);
        let heap_end        = heap_start + HEAP_SIZE - 1u64;
        let heap_start_page = Page::containing_address(heap_start);
        let heap_end_page   = Page::containing_address(heap_end);
        Page::range_inclusive(heap_start_page, heap_end_page)
    };

    for page in page_range {
        let frame = frame_allocator
            .allocate_frame()
            .ok_or(MapToError::FrameAllocationFailed)?;
        let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;
        unsafe { mapper.map_to(page, frame, flags, frame_allocator)?.flush() };
    }
    unsafe { ALLOCATOR.lock().init(HEAP_START, HEAP_SIZE) };
    Ok(())
}
