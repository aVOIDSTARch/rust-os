//! `boot/limine.rs` — Limine boot protocol adapter.
//!
//! Limine transfers control to `_start` already in 64-bit long mode with:
//! * A valid stack (≥ 16 KiB, 16-byte aligned).
//! * A higher-half direct map of all physical memory.
//! * BSS zeroed.
//! * All Limine request structures populated before `_start` is called.
//!
//! Our `_start` is therefore a thin Rust function — no assembly required
//! beyond the `global_asm!` that sets up the Limine magic and calls us.
//!
//! # Memory map kinds (Limine → shared::MemoryRegionKind)
//!
//! | Limine constant              | Our kind               |
//! |------------------------------|------------------------|
//! | USABLE                       | Usable                 |
//! | RESERVED                     | Reserved               |
//! | ACPI_RECLAIMABLE             | AcpiReclaimable        |
//! | ACPI_NVS                     | Reserved               |
//! | BAD_MEMORY                   | Reserved               |
//! | BOOTLOADER_RECLAIMABLE       | BootloaderReclaimable  |
//! | KERNEL_AND_MODULES           | KernelAndModules       |
//! | FRAMEBUFFER                  | Framebuffer            |

use core::arch::global_asm;
use limine::{
    request::{HhdmRequest, KernelAddressRequest, MemoryMapRequest},
    BaseRevision,
};
use shared::{MemoryRegion, MemoryRegionKind, PAGE_SIZE};
use super::KernelBootInfo;

// ── Limine requests (static, read by the bootloader before entry) ─────────────

/// Declare the Limine base revision we target.  Limine refuses to boot if
/// the kernel requests a revision the bootloader does not support.
#[used]
static BASE_REVISION: BaseRevision = BaseRevision::new();

#[used]
static MEMMAP_REQUEST: MemoryMapRequest = MemoryMapRequest::new();

#[used]
static HHDM_REQUEST: HhdmRequest = HhdmRequest::new();

#[used]
static KERNEL_ADDR_REQUEST: KernelAddressRequest = KernelAddressRequest::new();

// ── Entry point ───────────────────────────────────────────────────────────────

/// Limine entry point.  Called by the bootloader in 64-bit long mode.
///
/// # Safety
///
/// Called exactly once by the bootloader on the bootstrap processor.
/// All Limine request responses are populated before this is called.
#[no_mangle]
pub unsafe extern "C" fn _start() -> ! {
    // Verify the bootloader honoured our base revision.
    assert!(BASE_REVISION.is_supported(), "Limine: unsupported base revision");

    let hhdm_offset = HHDM_REQUEST
        .get_response()
        .expect("Limine: no HHDM response")
        .offset() as usize;

    let kernel_phys_base = KERNEL_ADDR_REQUEST
        .get_response()
        .expect("Limine: no kernel address response")
        .physical_base();

    let memmap_response = MEMMAP_REQUEST
        .get_response()
        .expect("Limine: no memory map response");

    // Translate Limine entries into our protocol-agnostic MemoryRegion slice.
    // We write into a static buffer so the slice outlives this stack frame.
    static mut REGIONS: [MemoryRegion; 256] = [MemoryRegion {
        base: 0, length: 0, kind: MemoryRegionKind::Reserved,
    }; 256];

    let entries = memmap_response.entries();
    let count   = entries.len().min(REGIONS.len());

    for (i, entry) in entries.iter().enumerate().take(count) {
        let kind = match entry.entry_type {
            limine::memory_map::EntryType::USABLE =>
                MemoryRegionKind::Usable,
            limine::memory_map::EntryType::ACPI_RECLAIMABLE =>
                MemoryRegionKind::AcpiReclaimable,
            limine::memory_map::EntryType::BOOTLOADER_RECLAIMABLE =>
                MemoryRegionKind::BootloaderReclaimable,
            limine::memory_map::EntryType::KERNEL_AND_MODULES =>
                MemoryRegionKind::KernelAndModules,
            limine::memory_map::EntryType::FRAMEBUFFER =>
                MemoryRegionKind::Framebuffer,
            _ => MemoryRegionKind::Reserved,
        };
        // SAFETY: single-threaded boot; no other reference to REGIONS.
        REGIONS[i] = MemoryRegion {
            base:   entry.base,
            length: entry.length,
            kind,
        };
    }

    // SAFETY: we just initialised REGIONS[0..count]; slice is valid for 'static.
    let regions = core::slice::from_raw_parts(REGIONS.as_ptr(), count);

    let boot_info = KernelBootInfo {
        memory_regions:  regions,
        hhdm_offset,
        kernel_phys_base,
    };

    crate::allocator_init(&boot_info);
    crate::kernel_main();
}
