//! `boot/multiboot2.rs` — Multiboot2 (GRUB) boot protocol adapter.
//!
//! # Handoff contract (from `barnacle/src/boot/boot.asm`)
//!
//! The assembly trampoline handles everything up to and including:
//! * Multiboot2 magic validation (halts with VGA error code if invalid)
//! * CPUID and long-mode availability checks
//! * 4-level page tables with two mappings via shared 2 MB huge pages:
//!   - Identity:    [0x0000_0000_0000_0000, 0x0000_0000_0020_0000) → phys [0, 2 MB)
//!   - Higher-half: [0xFFFF_FFFF_8000_0000, 0xFFFF_FFFF_8020_0000) → phys [0, 2 MB)
//! * PAE + EFER.LME + paging enabled
//! * Far jump through minimal 64-bit GDT into the higher-half `.text` section
//!
//! By the time `kernel_main` is called:
//! * CPU is in 64-bit long mode
//! * `rdi` = physical address of the Multiboot2 information structure
//!   (set from `edi` in 32-bit mode; x86_64 zero-extends 32-bit register
//!    writes, so no sign-extension hazard)
//! * Stack is the identity-mapped bootstrap stack (`stack_top` in `.bss.boot`)
//! * No Rust magic validation needed — the assembler already halted on failure
//!
//! # HHDM layout
//!
//! The page tables are fixed at compile time.  The higher-half base is:
//!
//! ```text
//! HHDM_OFFSET = 0xFFFF_FFFF_8000_0000
//! ```
//!
//! Only the first 2 MB of physical RAM is accessible via HHDM at boot.
//! The buddy allocator must restrict itself to physical addresses < 2 MB
//! until the kernel installs full page tables covering all usable RAM.
//! That VMM step is outside the scope of this allocator module.
//!
//! # What this file does NOT contain
//!
//! * No assembly — the trampoline is entirely in `barnacle/src/boot/boot.asm`
//! * No GDT setup
//! * No page table construction
//! * No CPUID or magic checks
//!
//! This file is purely the Rust half: parse the Multiboot2 information
//! structure, build a `KernelBootInfo`, and call the allocator init sequence.

use multiboot2::{BootInformation, BootInformationHeader, MemoryAreaType};
use shared::{MemoryRegion, MemoryRegionKind, PAGE_SIZE};
use super::KernelBootInfo;

// ── Fixed boot-time constants ─────────────────────────────────────────────────

/// Virtual base of the higher-half direct map established by `boot.asm`.
///
/// Derived from the page table setup in the assembly:
/// `PML4[511] → pdpt_high → pdpt_high[510] → pd[0]`
///
/// pdpt_high is at PML4 index 511 → covers canonical address range
/// starting at bit pattern 0b1111_1111_1 in VA[47:39].
/// pdpt_high[510] → VA[38:30] = 510 = 0b111_1111_10.
///
/// Composing: VA = 0xFFFF | (511 << 39) | (510 << 30) = 0xFFFF_FFFF_8000_0000
pub const HHDM_OFFSET: usize = 0xFFFF_FFFF_8000_0000;

/// Physical RAM accessible via HHDM at boot (one 2 MB huge page).
/// The buddy allocator must not hand out physical pages above this limit
/// until full page tables are installed.
pub const BOOT_MAPPED_PHYS: u64 = 2 * 1024 * 1024;

// ── Static region buffer ──────────────────────────────────────────────────────

/// Maximum Multiboot2 memory map entries we will handle.
/// Real machines rarely exceed 32; 128 gives headroom for fragmented maps.
const MAX_REGIONS: usize = 128;

/// Static storage for parsed memory regions.
///
/// Must be `'static` so the slice we hand to `KernelBootInfo` outlives
/// `kernel_main`'s stack frame.  Placed in `.bss` (zero-initialised).
static mut REGIONS: [MemoryRegion; MAX_REGIONS] = [MemoryRegion {
    base:   0,
    length: 0,
    kind:   MemoryRegionKind::Reserved,
}; MAX_REGIONS];

// ── Rust kernel entry point ───────────────────────────────────────────────────

/// Called by `long_mode_start` in `boot.asm` via `call kernel_main`.
///
/// # Safety
///
/// * Called exactly once on the bootstrap processor.
/// * `mbi_phys` is the physical address of a valid Multiboot2 information
///   structure placed there by GRUB, accessible at `HHDM_OFFSET + mbi_phys`
///   via the boot page tables.
/// * The Multiboot2 magic has already been validated by the assembly stub —
///   if we are here, the magic was correct.
/// * Physical addresses below `BOOT_MAPPED_PHYS` are accessible via HHDM.
///   The Multiboot2 structure is placed by GRUB below 1 MB by convention,
///   which is within the mapped window.
#[no_mangle]
pub unsafe extern "C" fn kernel_main(mbi_phys: u64) -> ! {
    // Translate the physical MBI address to a virtual address under our HHDM.
    //
    // SAFETY: HHDM_OFFSET + mbi_phys is mapped by the boot page tables
    // (mbi_phys < 1 MB < BOOT_MAPPED_PHYS = 2 MB).  The pointer is valid
    // for the lifetime of this call; GRUB does not reclaim the MBI.
    let mbi_virt = (HHDM_OFFSET + mbi_phys as usize) as *const BootInformationHeader;

    let boot_info = BootInformation::load(mbi_virt)
        .expect("multiboot2: malformed boot information structure");

    let mmap_tag = boot_info
        .memory_map_tag()
        .expect("multiboot2: no memory map tag — ensure GRUB passes --memory-map");

    // ── Parse memory map ──────────────────────────────────────────────────────

    let mut count = 0usize;

    for entry in mmap_tag.memory_areas() {
        if count >= MAX_REGIONS {
            // Silently drop excess entries — this is a hard boot-time limit.
            // If this fires in practice, increase MAX_REGIONS.
            break;
        }

        // Align base up to page boundary; trim length accordingly.
        let raw_base   = entry.start_address();
        let raw_end    = entry.end_address();
        let page_base  = align_up(raw_base, PAGE_SIZE as u64);
        let page_end   = align_down(raw_end, PAGE_SIZE as u64);

        if page_end <= page_base {
            // Region too small or entirely within an alignment gap — skip.
            continue;
        }

        let kind = match entry.typ() {
            MemoryAreaType::Available     => MemoryRegionKind::Usable,
            MemoryAreaType::AcpiAvailable => MemoryRegionKind::AcpiReclaimable,
            _                             => MemoryRegionKind::Reserved,
        };

        // SAFETY: single-threaded boot; no other reference to REGIONS exists.
        REGIONS[count] = MemoryRegion {
            base:   page_base,
            length: page_end - page_base,
            kind,
        };
        count += 1;
    }

    // SAFETY: REGIONS[0..count] is fully initialised above.
    // The slice is valid for 'static because REGIONS is a static array.
    let regions: &'static [MemoryRegion] =
        core::slice::from_raw_parts(REGIONS.as_ptr(), count);

    // ── Determine kernel physical base ────────────────────────────────────────
    //
    // Prefer the ELF sections tag (most accurate).  Fall back to the
    // conventional Multiboot2 load address of 1 MiB if the tag is absent
    // (e.g., GRUB config does not request ELF section info).
    let kernel_phys_base = boot_info
        .elf_sections()
        .and_then(|sections| {
            sections
                .sections()
                .filter(|s| s.flags() & 0x2 != 0) // SHF_ALLOC
                .map(|s| s.start_address() as u64)
                // Virtual addresses — subtract HHDM_OFFSET to get physical.
                .map(|va| va.saturating_sub(HHDM_OFFSET as u64))
                .min()
        })
        .unwrap_or(0x10_0000); // 1 MiB fallback

    // ── Construct protocol-agnostic boot info and hand off ────────────────────

    let kernel_boot_info = KernelBootInfo {
        memory_regions:  regions,
        hhdm_offset:     HHDM_OFFSET,
        kernel_phys_base,
    };

    // `allocator_init` populates the buddy allocator from usable regions,
    // then carves a pool for TLSF.  After it returns, `alloc` is live.
    crate::allocator_init(&kernel_boot_info);
    crate::kernel_main_post_heap();
}

// ── Alignment helpers ─────────────────────────────────────────────────────────

/// Round `addr` up to the nearest multiple of `align`.
/// `align` must be a power of two.
#[inline]
const fn align_up(addr: u64, align: u64) -> u64 {
    debug_assert!(align.is_power_of_two());
    (addr + align - 1) & !(align - 1)
}

/// Round `addr` down to the nearest multiple of `align`.
/// `align` must be a power of two.
#[inline]
const fn align_down(addr: u64, align: u64) -> u64 {
    debug_assert!(align.is_power_of_two());
    addr & !(align - 1)
}

// ── Boot-time constraint validation ──────────────────────────────────────────

/// Verify at compile time that our HHDM constant matches the page table
/// arithmetic in `boot.asm`.
///
/// PML4 index 511 → bits [47:39] = 0b1_1111_1111
/// PDPT index 510 → bits [38:30] = 0b111_1111_10
/// With sign extension (canonical): 0xFFFF_FFFF_8000_0000
const _HHDM_CHECK: () = {
    let pml4_idx: u64 = 511;
    let pdpt_idx: u64 = 510;
    let expected: u64 = 0xFFFF_8000_0000_0000u64
        + (pml4_idx << 39)
        + (pdpt_idx << 30);
    // Canonical sign-extension: bit 47 is set (PML4[511]), so bits [63:48]
    // must all be 1.  The arithmetic above already produces the correct value;
    // this assert catches any future edit that accidentally changes either index.
    assert!(
        expected == HHDM_OFFSET as u64,
        "HHDM_OFFSET does not match page table indices from boot.asm"
    );
};
