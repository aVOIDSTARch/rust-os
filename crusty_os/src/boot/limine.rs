//! Limine boot protocol adapter (`limine` crate 0.6.x).
//!
//! Limine transfers control to `_start` already in 64-bit long mode with:
//!
//! - A valid stack (≥ 16 KiB, 16-byte aligned)
//! - A full higher-half direct map of all physical memory (HHDM offset
//!   reported via `HhdmRequest`)
//! - BSS zeroed
//! - All Limine request responses populated in the request statics
//!
//! No assembly trampoline is needed — `_start` is a plain Rust `extern "C"`
//! function linked as the ELF entry point via `crusty_os/limine.ld`.

use limine::{
    request::{ExecutableAddressRequest, HhdmRequest, MemmapRequest},
    memmap,
    BaseRevision,
};
use framework::{KernelBootInfo, MemoryRegion, MemoryRegionKind};

// ── Limine request statics ────────────────────────────────────────────────────

#[used]
static BASE_REVISION: BaseRevision = BaseRevision::new();

#[used]
static MEMMAP_REQUEST: MemmapRequest = MemmapRequest::new();

#[used]
static HHDM_REQUEST: HhdmRequest = HhdmRequest::new();

#[used]
static EXEC_ADDR_REQUEST: ExecutableAddressRequest = ExecutableAddressRequest::new();

// ── Entry point ───────────────────────────────────────────────────────────────

/// Limine entry point. Called by the bootloader in 64-bit long mode.
///
/// # Safety
/// Called exactly once by the bootloader on the bootstrap processor.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _start() -> ! {
    assert!(BASE_REVISION.is_supported(), "Limine: unsupported base revision");

    let hhdm_offset = HHDM_REQUEST
        .response()
        .expect("Limine: no HHDM response")
        .offset as usize;

    let kernel_phys_base = EXEC_ADDR_REQUEST
        .response()
        .expect("Limine: no executable address response")
        .physical_base;

    let memmap_response = MEMMAP_REQUEST
        .response()
        .expect("Limine: no memory map response");

    static mut REGIONS: [MemoryRegion; 256] = [MemoryRegion {
        base: 0, length: 0, kind: MemoryRegionKind::Reserved,
    }; 256];

    let entries = memmap_response.entries();
    let count   = entries.len().min(256);

    for (i, entry) in entries.iter().enumerate().take(count) {
        let kind = match entry.type_ {
            memmap::MEMMAP_USABLE                  => MemoryRegionKind::Usable,
            memmap::MEMMAP_ACPI_RECLAIMABLE        => MemoryRegionKind::AcpiReclaimable,
            memmap::MEMMAP_BOOTLOADER_RECLAIMABLE  => MemoryRegionKind::BootloaderReclaimable,
            memmap::MEMMAP_EXECUTABLE_AND_MODULES  => MemoryRegionKind::KernelAndModules,
            memmap::MEMMAP_FRAMEBUFFER             => MemoryRegionKind::Framebuffer,
            _                                      => MemoryRegionKind::Reserved,
        };
        unsafe {
            REGIONS[i] = MemoryRegion { base: entry.base, length: entry.length, kind };
        }
    }

    let regions: &'static [MemoryRegion] =
        unsafe { core::slice::from_raw_parts(core::ptr::addr_of!(REGIONS) as *const MemoryRegion, count) };

    let boot_info = KernelBootInfo { memory_regions: regions, hhdm_offset, kernel_phys_base };

    crusty_os::allocator_init(&boot_info);
    crate::kernel_main_post_heap()
}
