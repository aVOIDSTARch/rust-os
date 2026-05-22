// Heap allocator — bump allocator backed by a spin-locked global.
//
// init_heap        (use-bootloader): maps new pages via OffsetPageTable
// init_heap_barnacle (use-barnacle): uses memory already mapped by boot.asm's 2 MB huge page

use bump::BumpAllocator;

#[cfg(feature = "use-bootloader")]
use x86_64::{
    structures::paging::{
        mapper::MapToError, FrameAllocator, Mapper, Page, PageTableFlags, Size4KiB,
    },
    VirtAddr,
};

pub mod bump;
pub mod linked_list;

#[global_allocator]
static ALLOCATOR: Locked<BumpAllocator> = Locked::new(BumpAllocator::new());

// ── Bootloader path ───────────────────────────────────────────────────────────

#[cfg(feature = "use-bootloader")]
pub const HEAP_START: usize = 0x_4444_4444_0000;
#[cfg(feature = "use-bootloader")]
pub const HEAP_SIZE:  usize = 100 * 1024;

#[cfg(feature = "use-bootloader")]
pub fn init_heap(
    mapper: &mut impl Mapper<Size4KiB>,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
) -> Result<(), MapToError<Size4KiB>> {
    let page_range = {
        let heap_start = VirtAddr::new(HEAP_START as u64);
        let heap_end   = heap_start + HEAP_SIZE - 1u64;
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

// ── Barnacle / Multiboot2 path ────────────────────────────────────────────────
//
// boot.asm maps physical [0, 2 MB) as a single huge page at both:
//   virtual [0, 2 MB)                          — identity map
//   virtual [KERNEL_OFFSET, KERNEL_OFFSET+2 MB) — higher-half
//
// The kernel binary occupies roughly physical [1 MB, 1.25 MB) which maps to
// virtual [KERNEL_OFFSET+1 MB, KERNEL_OFFSET+1.25 MB).
// BARNACLE_HEAP_START sits above that, still within the mapped 2 MB window,
// so no additional page-table manipulation is required.

#[cfg(feature = "use-barnacle")]
const BARNACLE_HEAP_START: usize = 0xFFFF_FFFF_8016_0000;
#[cfg(feature = "use-barnacle")]
const BARNACLE_HEAP_SIZE:  usize = 0x8_0000; // 512 KiB

#[cfg(feature = "use-barnacle")]
pub fn init_heap_barnacle() {
    unsafe { ALLOCATOR.lock().init(BARNACLE_HEAP_START, BARNACLE_HEAP_SIZE) };
}

// ── Shared utilities ──────────────────────────────────────────────────────────

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

pub(super) fn align_up(addr: usize, align: usize) -> usize {
    (addr + align - 1) & !(align - 1)
}
