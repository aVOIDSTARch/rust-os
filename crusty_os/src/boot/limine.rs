// Limine boot protocol adapter.
//
// Limine transfers control to `_start` already in 64-bit long mode with:
//   * A valid stack (≥ 16 KiB, 16-byte aligned)
//   * A higher-half direct map of all physical memory
//   * BSS zeroed
//   * All Limine request responses populated
//
// No assembly trampoline is needed.

use limine::{
    request::{HhdmRequest, KernelAddressRequest, MemoryMapRequest},
    BaseRevision,
};
use framework::{MemoryRegion, MemoryRegionKind};
use super::KernelBootInfo;

// ── Limine request statics ────────────────────────────────────────────────────

#[used]
static BASE_REVISION: BaseRevision = BaseRevision::new();

#[used]
static MEMMAP_REQUEST: MemoryMapRequest = MemoryMapRequest::new();

#[used]
static HHDM_REQUEST: HhdmRequest = HhdmRequest::new();

#[used]
static KERNEL_ADDR_REQUEST: KernelAddressRequest = KernelAddressRequest::new();

// ── Entry point ───────────────────────────────────────────────────────────────

/// Limine entry point. Called by the bootloader in 64-bit long mode.
///
/// # Safety
/// Called exactly once by the bootloader on the bootstrap processor.
#[no_mangle]
pub unsafe extern "C" fn _start() -> ! {
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

    static mut REGIONS: [MemoryRegion; 256] = [MemoryRegion {
        base: 0, length: 0, kind: MemoryRegionKind::Reserved,
    }; 256];

    let entries = memmap_response.entries();
    let count   = entries.len().min(unsafe { REGIONS.len() });

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
        unsafe {
            REGIONS[i] = MemoryRegion { base: entry.base, length: entry.length, kind };
        }
    }

    let regions: &'static [MemoryRegion] =
        unsafe { core::slice::from_raw_parts(REGIONS.as_ptr(), count) };

    let boot_info = KernelBootInfo { memory_regions: regions, hhdm_offset, kernel_phys_base };

    crate::allocator_init(&boot_info);
    crate::kernel_main_post_heap()
}
