//! Boot-protocol abstraction layer.
//!
//! Translates bootloader-specific structures into the protocol-neutral
//! [`KernelBootInfo`] type so the rest of the kernel never sees a
//! bootloader-specific API.  Each protocol adapter (multiboot2, limine) calls
//! `crate::allocator_init` with a populated [`KernelBootInfo`].
//!
//! # Feature gates
//!
//! Exactly one of the following Cargo features must be active per binary build:
//!
//! | Feature            | Protocol       | Entry symbol  |
//! |--------------------|----------------|---------------|
//! | `boot-multiboot2`  | Multiboot2     | `kernel_main` |
//! | `boot-limine`      | Limine         | `_start`      |
//! | `use-bootloader`   | blog_os loader | `kmain`       |
//!
//! Activating more than one is a compile error (enforced below).

use framework::{MemoryRegion, MemoryRegionKind};

// ── Unified boot information ──────────────────────────────────────────────────

/// Protocol-neutral description of the machine state at kernel entry.
///
/// Filled in by each boot adapter and passed to [`crate::allocator_init`].
/// The allocator, interrupt subsystem, and the rest of the kernel all use
/// this type rather than any bootloader-specific types.
pub struct KernelBootInfo {
    /// Physical memory map reported by the bootloader, in a static slice.
    pub memory_regions:   &'static [MemoryRegion],
    /// Virtual offset that converts a physical address to its Higher-Half
    /// Direct Map (HHDM) virtual address: `virt = hhdm_offset + phys`.
    pub hhdm_offset:      usize,
    /// Physical base address of the loaded kernel image.
    pub kernel_phys_base: u64,
}

impl KernelBootInfo {
    /// Iterate over memory regions of a specific [`MemoryRegionKind`].
    pub fn regions_of_kind(
        &self,
        kind: MemoryRegionKind,
    ) -> impl Iterator<Item = &MemoryRegion> {
        self.memory_regions.iter().filter(move |r| r.kind == kind)
    }

    /// Convert a physical address to its HHDM virtual address.
    #[inline]
    pub fn phys_to_virt(&self, phys: u64) -> usize {
        self.hhdm_offset + phys as usize
    }
}

// ── Compile-time mutual-exclusion guards ──────────────────────────────────────

#[cfg(all(feature = "boot-multiboot2", feature = "boot-limine"))]
compile_error!("boot-multiboot2 and boot-limine are mutually exclusive.");

#[cfg(all(feature = "boot-multiboot2", feature = "use-bootloader"))]
compile_error!("boot-multiboot2 and use-bootloader are mutually exclusive.");

#[cfg(all(feature = "boot-limine", feature = "use-bootloader"))]
compile_error!("boot-limine and use-bootloader are mutually exclusive.");

// ── Protocol modules ──────────────────────────────────────────────────────────

#[cfg(feature = "boot-multiboot2")]
pub mod multiboot2;

#[cfg(feature = "boot-limine")]
pub mod limine;
