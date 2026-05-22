// Boot-protocol abstraction layer.
//
// Each supported bootloader translates its native info structure into a
// `KernelBootInfo` value and calls `crate::allocator_init`.  The allocator
// stack (buddy, slab, TLSF) never sees a protocol-specific type.
//
// Exactly one boot feature must be active per build; this module enforces that
// with compile_error! guards.

use framework::{MemoryRegion, MemoryRegionKind};

// ── Unified boot information ──────────────────────────────────────────────────

pub struct KernelBootInfo {
    pub memory_regions:   &'static [MemoryRegion],
    /// Virtual offset added to physical addresses to obtain HHDM virtual addrs.
    pub hhdm_offset:      usize,
    pub kernel_phys_base: u64,
}

impl KernelBootInfo {
    pub fn regions_of_kind(
        &self,
        kind: MemoryRegionKind,
    ) -> impl Iterator<Item = &MemoryRegion> {
        self.memory_regions.iter().filter(move |r| r.kind == kind)
    }

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
