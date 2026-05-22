//! `boot/mod.rs` — Boot-protocol abstraction layer.
//!
//! Every supported bootloader (Limine, Multiboot2, rust-bootloader) translates
//! its native information structure into a `KernelBootInfo` value and calls
//! `crate::allocator_init`.  The allocator stack — buddy, slab, TLSF — never
//! sees a protocol-specific type.
//!
//! # Selecting a boot protocol
//!
//! Enable exactly one Cargo feature at build time:
//!
//! ```text
//! cargo build --features boot-limine       # default
//! cargo build --features boot-multiboot2   # GRUB/Multiboot2
//! cargo build --features boot-rboot        # Philipp Oppermann's bootloader crate
//! ```
//!
//! The linker script and `rust-toolchain.toml` remain the same across all
//! three; only the entry-point symbol and memory-map parsing change.
//!
//! # HHDM offset
//!
//! All three protocols map physical memory into the higher half.  The exact
//! virtual base differs per protocol:
//!
//! | Protocol       | HHDM base (typical)          |
//! |----------------|------------------------------|
//! | Limine         | dynamic — read from response |
//! | Multiboot2     | none — identity map only;    |
//! |                | kernel sets up HHDM itself   |
//! | rust-bootloader| `physical_memory_offset` field |
//!
//! For Multiboot2, this kernel establishes a minimal HHDM in the long-mode
//! trampoline before calling Rust.  See `boot/multiboot2.rs`.

use shared::{MemoryRegion, MemoryRegionKind};

// ── Unified boot information ──────────────────────────────────────────────────

/// Protocol-agnostic kernel boot information passed to `allocator_init`.
///
/// Constructed by whichever `boot/<protocol>.rs` module is compiled in.
pub struct KernelBootInfo {
    /// Slice of memory regions describing the physical address space.
    /// The slice lives in memory that survives past boot (static or stack in
    /// the boot shim); the kernel must copy any regions it needs long-term.
    pub memory_regions: &'static [MemoryRegion],
    /// Virtual offset added to every physical address to obtain a kernel
    /// virtual address (higher-half direct map).
    pub hhdm_offset: usize,
    /// Physical address of the kernel ELF image base.
    pub kernel_phys_base: u64,
}

impl KernelBootInfo {
    /// Iterate over regions of the given kind.
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

// ── Compile-time feature guard ────────────────────────────────────────────────
// Exactly one boot feature must be active.  We enforce this with a compile
// error rather than letting it silently build a broken kernel.

#[cfg(not(any(
    feature = "boot-limine",
    feature = "boot-multiboot2",
    feature = "boot-rboot",
)))]
compile_error!(
    "No boot protocol selected. Enable exactly one of: \
     boot-limine, boot-multiboot2, boot-rboot"
);

#[cfg(all(feature = "boot-limine", feature = "boot-multiboot2"))]
compile_error!("boot-limine and boot-multiboot2 are mutually exclusive.");

#[cfg(all(feature = "boot-limine", feature = "boot-rboot"))]
compile_error!("boot-limine and boot-rboot are mutually exclusive.");

#[cfg(all(feature = "boot-multiboot2", feature = "boot-rboot"))]
compile_error!("boot-multiboot2 and boot-rboot are mutually exclusive.");

// ── Protocol module re-exports ────────────────────────────────────────────────

#[cfg(feature = "boot-limine")]
pub mod limine;

#[cfg(feature = "boot-multiboot2")]
pub mod multiboot2;

#[cfg(feature = "boot-rboot")]
pub mod rboot;
