//! Multiboot2 / GRUB boot adapter.
//!
//! `barnacle`'s `boot.asm` validates the Multiboot2 magic, sets up minimal
//! page tables (identity-maps and HHDM-maps the first 2 MiB as a 2 MiB huge
//! page), and jumps to `kernel_main` in 64-bit long mode with:
//!
//! - `rdi` = physical address of the Multiboot2 information structure
//! - CPU in 64-bit long mode with interrupts disabled
//! - Stack = bootstrap stack from `boot.asm`
//!
//! # Memory window at boot
//!
//! Only physical `[0, 2 MiB)` is accessible via HHDM at this point.
//! [`HEAP_START_PHYS`] is set conservatively above the kernel image
//! (~1.25 MiB) so the buddy allocator only touches pages that are safely
//! within the mapped window.

use framework::{MemoryRegion, MemoryRegionKind, PAGE_SIZE};
use barnacle::info::MemoryAreaType;
use super::KernelBootInfo;

/// HHDM base — matches PML4[511] / PDPT_HIGH[510] in boot.asm.
pub const HHDM_OFFSET: usize = 0xFFFF_FFFF_8000_0000;

/// Physical RAM accessible via HHDM at boot (one 2 MB huge page).
const BOOT_MAPPED_PHYS: u64 = 2 * 1024 * 1024;

/// Conservative lower bound for buddy-managed physical memory — above the
/// kernel image (~1.25 MB) and any Multiboot2 structures.
const HEAP_START_PHYS: u64 = 0x16_0000; // 1.375 MB

const MAX_REGIONS: usize = 128;

static mut REGIONS: [MemoryRegion; MAX_REGIONS] = [MemoryRegion {
    base:   0,
    length: 0,
    kind:   MemoryRegionKind::Reserved,
}; MAX_REGIONS];

/// Kernel entry point called by `long_mode_start` in boot.asm.
///
/// # Safety
/// Called exactly once on the bootstrap processor with GRUB's valid MB2 ptr.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kernel_main(mbi_phys: u64) -> ! {
    let boot_info = barnacle::init(mbi_phys);

    let mut count = 0usize;

    if let Some(mmap) = boot_info.memory_map() {
        for area in mmap.memory_areas() {
            if count >= MAX_REGIONS { break; }

            let raw_base  = area.start_address();
            let raw_end   = area.end_address();
            let page_base = align_up(raw_base,  PAGE_SIZE as u64);
            let page_end  = align_down(raw_end, PAGE_SIZE as u64);
            if page_end <= page_base { continue; }

            let kind = match area.typ() {
                t if t == MemoryAreaType::Available      => MemoryRegionKind::Usable,
                t if t == MemoryAreaType::AcpiAvailable  => MemoryRegionKind::AcpiReclaimable,
                _                                        => MemoryRegionKind::Reserved,
            };

            REGIONS[count] = MemoryRegion {
                base:   page_base,
                length: page_end - page_base,
                kind,
            };
            count += 1;
        }
    }

    // Conventional Multiboot2 load base for our kernel (barnacle/kernel.ld LMA).
    let kernel_phys_base: u64 = 0x10_0000; // 1 MiB

    let regions: &'static [MemoryRegion] =
        core::slice::from_raw_parts(core::ptr::addr_of!(REGIONS) as *const MemoryRegion, count);

    let kbi = KernelBootInfo {
        memory_regions:   regions,
        hhdm_offset:      HHDM_OFFSET,
        kernel_phys_base,
    };

    crate::allocator_init(&kbi);
    crate::kernel_main_post_heap()
}

// ── Alignment helpers ─────────────────────────────────────────────────────────

#[inline]
const fn align_up(addr: u64, align: u64) -> u64 {
    (addr + align - 1) & !(align - 1)
}

#[inline]
const fn align_down(addr: u64, align: u64) -> u64 {
    addr & !(align - 1)
}

const _HHDM_CHECK: () = {
    // PML4[511] base: sign_extend(0xFF80_0000_0000) = 0xFFFF_FF80_0000_0000
    // PDPT[510] adds: 510 * 2^30 = 0x7F_8000_0000
    let expected: u64 = 0xFFFF_FF80_0000_0000u64 + 510u64 * (1u64 << 30);
    assert!(expected == HHDM_OFFSET as u64, "HHDM_OFFSET does not match boot.asm page tables");
};
