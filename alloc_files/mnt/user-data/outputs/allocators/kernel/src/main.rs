//! `main.rs` — Kernel crate root.
//!
//! # Entry point ownership
//!
//! The `kernel_main` symbol (the actual CPU entry point called by the
//! assembly trampoline) is defined in the active boot module:
//!
//! | Feature           | Entry point defined in       |
//! |-------------------|------------------------------|
//! | `boot-multiboot2` | `boot::multiboot2::kernel_main` |
//! | `boot-limine`     | `boot::limine::_start`       |
//! | `boot-rboot`      | `boot::rboot::kernel_main`   |
//!
//! This file defines two functions the boot modules call after parsing
//! their respective information structures:
//!
//! * `allocator_init(&KernelBootInfo)` — feeds usable regions to the buddy
//!   allocator, then carves a TLSF pool.  After this returns, `alloc` is live.
//! * `kernel_main_post_heap()` — kernel logic that may use `Box`, `Vec`, etc.
//!
//! # Allocator initialisation order
//!
//! ```text
//! boot module
//!   └─ allocator_init()
//!        ├─ buddy::BUDDY.add_region()  for each usable physical region
//!        └─ tlsf::TLSF.init()         carves TLSF_POOL_ORDER pages from buddy
//!             └─ #[global_allocator] is now live
//!                  └─ kernel_main_post_heap()
//!                       └─ Box / Vec / SlabCache<T> all available
//! ```

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use core::panic::PanicInfo;
use shared::{MemoryRegionKind, PAGE_SIZE};

mod buddy;
mod slab;
mod tlsf;
mod boot;

// ── Global allocator ──────────────────────────────────────────────────────────

#[global_allocator]
static GLOBAL: tlsf::TlsfAllocator = tlsf::TLSF;

// ── Example typed slab caches ─────────────────────────────────────────────────

/// Hypothetical task control block — replace with your real struct.
#[repr(C, align(16))]
struct TaskControlBlock {
    pid:        u64,
    stack_ptr:  u64,
    page_table: u64,
    state:      u32,
    priority:   u16,
    _pad:       u16,
}

const _: () = slab::assert_slab_compatible::<TaskControlBlock>();

static TASK_CACHE: slab::SlabCache<TaskControlBlock> = slab::SlabCache::new(0);

// ── TLSF pool size ────────────────────────────────────────────────────────────

/// Buddy pages allocated to the TLSF general heap.
/// Order 8 = 256 × 4 KiB = 1 MiB.
///
/// # Important constraint for the Multiboot2 boot path
///
/// At the time `allocator_init` runs, only the first 2 MB of physical RAM
/// is mapped (one huge page established by `boot.asm`).  The buddy allocator
/// must source its pages from within that 2 MB window, which it will do
/// naturally as long as GRUB places usable RAM starting at 1 MiB and the
/// TLSF pool (1 MiB) plus buddy metadata fit within the remaining 1 MiB gap.
///
/// Once the kernel installs full page tables (VMM work, outside this module),
/// this constraint is lifted and the pool can be grown.
const TLSF_POOL_ORDER: usize = 8; // 1 MiB

// ── Allocator initialisation ──────────────────────────────────────────────────

/// Feed the physical memory map to the buddy allocator, then initialise TLSF.
///
/// Called by the active boot module after it has parsed the bootloader's
/// memory map into a `KernelBootInfo`.
///
/// # Safety
///
/// * Must be called exactly once, before any heap allocation.
/// * Must be called on the bootstrap processor before SMP is started.
/// * `boot_info.memory_regions` must accurately describe physical memory —
///   handing usable-marked pages that are in fact reserved to the buddy
///   allocator will corrupt firmware structures or ACPI tables.
pub unsafe fn allocator_init(boot_info: &boot::KernelBootInfo) {
    {
        let mut buddy = buddy::BUDDY.lock();

        for region in boot_info.regions_of_kind(MemoryRegionKind::Usable) {
            // Under the Multiboot2 boot path, skip physical pages above
            // BOOT_MAPPED_PHYS — they are not yet accessible via HHDM.
            // The VMM will call buddy.add_region() again after extending
            // the page tables to cover all RAM.
            #[cfg(feature = "boot-multiboot2")]
            {
                use boot::multiboot2::BOOT_MAPPED_PHYS;
                if region.base >= BOOT_MAPPED_PHYS {
                    continue;
                }
                // Clamp regions that straddle the 2 MB boundary.
                let clamped_end    = region.end().min(BOOT_MAPPED_PHYS);
                let clamped_length = clamped_end - region.base;
                let page_count     = (clamped_length as usize) / PAGE_SIZE;
                if page_count == 0 { continue; }
                let virt_base = boot_info.phys_to_virt(region.base);
                // SAFETY: region is usable RAM mapped at HHDM_OFFSET,
                // not aliased by any other reference at this point in boot.
                buddy.add_region(virt_base, page_count);
                continue;
            }

            // Limine and rboot paths: all usable regions are already fully
            // mapped by the bootloader before Rust is entered.
            #[allow(unreachable_code)]
            {
                let page_count = (region.length as usize) / PAGE_SIZE;
                if page_count == 0 { continue; }
                let virt_base = boot_info.phys_to_virt(region.base);
                // SAFETY: as above.
                buddy.add_region(virt_base, page_count);
            }
        }
    } // buddy lock released before TLSF init (TLSF init also takes the lock)

    // SAFETY: called exactly once here, buddy is populated above.
    tlsf::TLSF.init(TLSF_POOL_ORDER);
}

// ── Post-heap kernel entry ────────────────────────────────────────────────────

/// Kernel logic that runs after the heap is live.
///
/// `Box`, `Vec`, `String`, `alloc::collections::*`, and `SlabCache<T>` are
/// all available here.  This is where the rest of your kernel initialises:
/// interrupt tables, scheduler, device drivers, etc.
pub fn kernel_main_post_heap() -> ! {
    // Example: allocate a task from the slab cache.
    let task_slot = TASK_CACHE.alloc().expect("TCB slab OOM");

    // SAFETY: slot is unaliased and uninitialised; we write before any read.
    unsafe {
        task_slot.as_ptr().write(TaskControlBlock {
            pid:        1,
            stack_ptr:  0,
            page_table: 0,
            state:      0,
            priority:   0,
            _pad:       0,
        });
    }

    // Retrieve stats from each allocator layer.
    let buddy_stats = buddy::BUDDY.lock().stats();
    let slab_stats  = TASK_CACHE.stats();

    // In a real kernel: write these to a serial port.
    // For now, suppress the unused-variable lint.
    let _ = (buddy_stats, slab_stats);

    // SAFETY: task_slot came from TASK_CACHE.alloc(); all fields are Copy,
    // so no custom Drop logic is needed before returning to the cache.
    unsafe { TASK_CACHE.dealloc(task_slot); }

    loop {
        // SAFETY: `hlt` is always safe in kernel context.
        unsafe {
            core::arch::asm!("hlt", options(nomem, nostack, preserves_flags));
        }
    }
}

// ── Panic + OOM handlers ──────────────────────────────────────────────────────

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    // Production: write to serial, halt other CPUs via IPI, dump register state.
    loop {
        unsafe { core::arch::asm!("hlt", options(nomem, nostack)) };
    }
}

#[alloc_error_handler]
fn alloc_error(_layout: core::alloc::Layout) -> ! {
    panic!("kernel OOM — alloc_error_handler invoked");
}
