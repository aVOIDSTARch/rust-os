//! `shared` — types that must be ABI-compatible across the kernel/userspace
//! boundary (e.g., passed through a hypercall or shared-memory channel).
//!
//! Compiles as `no_std` when the `std` feature is absent; userspace crates
//! enable `std` so they can derive `Debug`, use `std::error::Error`, etc.

#![cfg_attr(not(feature = "std"), no_std)]

// ── Memory region descriptor ──────────────────────────────────────────────────

/// Classification of a physical memory region as reported by the bootloader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MemoryRegionKind {
    /// Usable RAM — may be handed to the buddy allocator.
    Usable = 0,
    /// Firmware / ACPI tables — must not be overwritten.
    Reserved = 1,
    /// ACPI reclaimable after the OS has consumed the tables.
    AcpiReclaimable = 2,
    /// Memory-mapped I/O; never hand to the allocator.
    Mmio = 3,
    /// Bootloader-used pages (may be reclaimed after paging is set up).
    BootloaderReclaimable = 4,
    /// Kernel ELF image loaded by the bootloader.
    KernelAndModules = 5,
    /// Framebuffer region.
    Framebuffer = 6,
}

/// A contiguous physical address range with a classification.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct MemoryRegion {
    /// Inclusive start address (always page-aligned).
    pub base: u64,
    /// Length in bytes (always a multiple of 4096).
    pub length: u64,
    pub kind: MemoryRegionKind,
}

impl MemoryRegion {
    #[inline]
    pub const fn end(&self) -> u64 {
        self.base + self.length
    }

    #[inline]
    pub const fn page_count(&self) -> u64 {
        self.length / 4096
    }
}

// ── Allocator statistics ──────────────────────────────────────────────────────

/// Snapshot of allocator state; populated by each allocator layer and surfaced
/// to userspace diagnostics or kernel debuggers.
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct AllocStats {
    /// Total bytes under management (does not change after init).
    pub total_bytes: u64,
    /// Bytes currently allocated (live).
    pub used_bytes: u64,
    /// Bytes free but not yet returned to the OS.
    pub free_bytes: u64,
    /// Number of successful allocation calls since init.
    pub alloc_count: u64,
    /// Number of successful deallocation calls since init.
    pub dealloc_count: u64,
    /// Peak `used_bytes` observed since init.
    pub peak_bytes: u64,
}

impl AllocStats {
    #[inline]
    pub fn fragmentation_ratio(&self) -> f64 {
        if self.total_bytes == 0 {
            return 0.0;
        }
        1.0 - (self.used_bytes as f64 / self.total_bytes as f64)
    }
}

// ── Page-size constants ───────────────────────────────────────────────────────

pub const PAGE_SIZE: usize       = 4096;
pub const PAGE_SHIFT: usize      = 12;
pub const HUGE_PAGE_SIZE: usize  = 2 * 1024 * 1024;   // 2 MiB
pub const HUGE_PAGE_SHIFT: usize = 21;

/// Maximum buddy order supported (2^10 × 4 KiB = 4 MiB).
pub const BUDDY_MAX_ORDER: usize = 11;
