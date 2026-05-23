//! barnacle — Multiboot2 bootloader library for x86_64 kernels.
//!
//! Provides everything a kernel needs to be Multiboot2-compliant and bootable
//! via GRUB, without writing any assembly or a linker script by hand:
//!
//! - **Assembly boot stub** (`src/boot/boot.asm`): linked in automatically via
//!   `build.rs`.  Handles Multiboot2 header placement, CPUID check, 4-level
//!   page table setup, 32-bit → 64-bit long-mode transition, and the
//!   handoff to the Rust entry point.
//!
//! - **Linker script** (`kernel.ld`): also applied automatically via `build.rs`.
//!   Places the Multiboot2 header in the first 32 KB, maps the boot stub at a
//!   physical address, and maps the rest of the kernel at
//!   `KERNEL_OFFSET = 0xFFFFFFFF80000000` (the conventional higher-half base).
//!
//! - **[`KernelBootInfo`]**: protocol-neutral boot information parsed from the
//!   Multiboot2 information structure that GRUB passes at boot time.
//!
//! - **[`entry_point!`]**: macro that declares the kernel's Rust entry function
//!   and type-checks its signature.
//!
//! # Usage
//!
//! ```ignore
//! #![no_std]
//! #![no_main]
//!
//! barnacle::entry_point!(my_kernel_main);
//!
//! fn my_kernel_main(kbi: &'static barnacle::KernelBootInfo) -> ! {
//!     // kbi.memory_regions, kbi.hhdm_offset, kbi.kernel_phys_base, ...
//!     loop {}
//! }
//!
//! #[panic_handler]
//! fn panic(_: &core::panic::PanicInfo) -> ! { loop {} }
//! ```
//!
//! # Invariants
//!
//! - [`entry_point!`] must be invoked exactly once per binary.
//! - barnacle's `build.rs` links `boot.asm` and `kernel.ld` into the
//!   final binary automatically — do not pass conflicting `rustc-link-arg`
//!   entries for the linker script.
//! - The kernel's own `.cargo/config.toml` must specify an `x86_64-*-none`
//!   target with `disable-redzone = true` and `panic-strategy = "abort"`.
//!   The workspace `x86_64-crusty_os.json` satisfies these requirements.

#![no_std]

pub mod info;
pub use info::BootInfo;

pub use framework::KernelBootInfo;
use framework::{MemoryRegion, MemoryRegionKind, PAGE_SIZE};

use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicBool, Ordering};

use multiboot2::{BootInformation, BootInformationHeader};

// ── Linker-script symbols ─────────────────────────────────────────────────────

unsafe extern "C" {
    /// Physical address of the first byte past the kernel image.
    /// Exported by `kernel.ld` as `_kernel_end_phys`.
    static _kernel_end_phys: u8;
}

/// Returns the physical address of the first byte past the end of the loaded
/// kernel image, page-aligned up to the next 4 KiB boundary.
///
/// Use this to start the heap region above the kernel to avoid overwriting
/// code, data, or BSS.
pub fn kernel_end_phys() -> u64 {
    let raw = core::ptr::addr_of!(_kernel_end_phys) as u64;
    // Align up to page boundary.
    (raw + 0xFFF) & !0xFFF
}

// ── HHDM layout ───────────────────────────────────────────────────────────────

/// Higher-half direct-map base address.
///
/// Matches `PML4[511] / PDPT_HIGH[510]` in `boot.asm`:
/// `0xFFFF_FF80_0000_0000 + 510 × 2^30 = 0xFFFF_FFFF_8000_0000`.
pub const HHDM_OFFSET: usize = 0xFFFF_FFFF_8000_0000;

// Compile-time proof that the constant matches the assembly page tables.
const _HHDM_CHECK: () = {
    let expected: u64 = 0xFFFF_FF80_0000_0000u64 + 510u64 * (1u64 << 30);
    assert!(expected == HHDM_OFFSET as u64, "HHDM_OFFSET does not match boot.asm page tables");
};

// ── Raw BootInfo storage (for callers that need the low-level MB2 wrapper) ───

struct BootInfoCell(UnsafeCell<MaybeUninit<BootInfo>>);
unsafe impl Sync for BootInfoCell {}
static BOOT_INFO: BootInfoCell = BootInfoCell(UnsafeCell::new(MaybeUninit::uninit()));

static INIT_CALLED: AtomicBool = AtomicBool::new(false);

/// Parse the Multiboot2 information structure and return a `'static` reference
/// to the raw [`BootInfo`] wrapper.
///
/// Most kernels should use [`entry_point!`] instead; `init` is provided for
/// code that needs access to Multiboot2-specific tags not surfaced by
/// [`KernelBootInfo`].
///
/// # Safety
/// - `addr` must be the physical address GRUB placed in `rbx / edi`.
/// - Must be called exactly once.
pub unsafe fn init(addr: u64) -> &'static BootInfo {
    assert!(
        !INIT_CALLED.swap(true, Ordering::SeqCst),
        "barnacle::init called more than once"
    );
    let raw: BootInformation<'static> = unsafe {
        BootInformation::load(addr as *const BootInformationHeader)
            .expect("barnacle: invalid Multiboot2 information structure")
    };
    unsafe {
        let ptr: *mut MaybeUninit<BootInfo> = BOOT_INFO.0.get();
        ptr.write(MaybeUninit::new(BootInfo::new(raw)));
        &*(ptr as *const BootInfo)
    }
}

// ── KernelBootInfo storage ────────────────────────────────────────────────────

const MAX_REGIONS: usize = 128;

struct RegionsCell(UnsafeCell<[MemoryRegion; MAX_REGIONS]>);
unsafe impl Sync for RegionsCell {}
static REGIONS: RegionsCell = RegionsCell(UnsafeCell::new([MemoryRegion {
    base: 0, length: 0, kind: MemoryRegionKind::Reserved,
}; MAX_REGIONS]));

struct KbiCell(UnsafeCell<MaybeUninit<KernelBootInfo>>);
unsafe impl Sync for KbiCell {}
static KBI: KbiCell = KbiCell(UnsafeCell::new(MaybeUninit::uninit()));

static PARSE_CALLED: AtomicBool = AtomicBool::new(false);

/// Parse the Multiboot2 information structure and return a `'static`
/// [`KernelBootInfo`].
///
/// Called by the [`entry_point!`] macro.  Translates the Multiboot2 memory
/// map into the protocol-neutral [`MemoryRegion`] slice and fills
/// [`KernelBootInfo`] with the HHDM offset and kernel physical base.
///
/// # Safety
/// - `addr` must be the physical address GRUB placed in `rbx / edi`.
/// - Must be called exactly once.
pub unsafe fn parse_boot_info(addr: u64) -> &'static KernelBootInfo {
    assert!(
        !PARSE_CALLED.swap(true, Ordering::SeqCst),
        "barnacle::parse_boot_info called more than once"
    );

    // Safety: addr is GRUB-provided, valid for the kernel lifetime.
    let raw: BootInformation<'static> = unsafe {
        BootInformation::load(addr as *const BootInformationHeader)
            .expect("barnacle: invalid Multiboot2 information structure")
    };

    let regions_ptr: *mut [MemoryRegion; MAX_REGIONS] = REGIONS.0.get();
    let mut count = 0usize;

    if let Some(mmap) = raw.memory_map_tag() {
        for area in mmap.memory_areas() {
            if count >= MAX_REGIONS { break; }

            let raw_base = area.start_address();
            let raw_end  = area.end_address();
            let page_base = align_up_u64(raw_base, PAGE_SIZE as u64);
            let page_end  = align_down_u64(raw_end, PAGE_SIZE as u64);
            if page_end <= page_base { continue; }

            let kind = match area.typ() {
                t if t == multiboot2::MemoryAreaType::Available     => MemoryRegionKind::Usable,
                t if t == multiboot2::MemoryAreaType::AcpiAvailable => MemoryRegionKind::AcpiReclaimable,
                _                                                    => MemoryRegionKind::Reserved,
            };

            unsafe {
                (*regions_ptr)[count] = MemoryRegion { base: page_base, length: page_end - page_base, kind };
            }
            count += 1;
        }
    }

    // Conventional Multiboot2 load base matches barnacle/kernel.ld LMA (1 MiB).
    let kernel_phys_base: u64 = 0x10_0000;

    let regions: &'static [MemoryRegion] = unsafe {
        core::slice::from_raw_parts((*regions_ptr).as_ptr(), count)
    };

    let kbi = KernelBootInfo { memory_regions: regions, hhdm_offset: HHDM_OFFSET, kernel_phys_base };

    unsafe {
        let ptr: *mut MaybeUninit<KernelBootInfo> = KBI.0.get();
        ptr.write(MaybeUninit::new(kbi));
        &*(ptr as *const KernelBootInfo)
    }
}

#[inline]
const fn align_up_u64(addr: u64, align: u64) -> u64 {
    (addr + align - 1) & !(align - 1)
}

#[inline]
const fn align_down_u64(addr: u64, align: u64) -> u64 {
    addr & !(align - 1)
}

// ── entry_point! macro ────────────────────────────────────────────────────────

/// Declare the kernel entry point, type-checking its signature.
///
/// The function provided must have the signature:
/// ```ignore
/// fn name(kbi: &'static KernelBootInfo) -> !
/// ```
///
/// barnacle's assembly stub calls `kernel_main`; this macro defines that
/// symbol, calls [`parse_boot_info`] to build the [`KernelBootInfo`], and
/// forwards it to the user-supplied function.
///
/// # Example
///
/// ```ignore
/// barnacle::entry_point!(my_entry);
///
/// fn my_entry(kbi: &'static barnacle::KernelBootInfo) -> ! {
///     loop {}
/// }
/// ```
#[macro_export]
macro_rules! entry_point {
    ($path:path) => {
        /// Kernel entry point defined by `barnacle::entry_point!`.
        ///
        /// Called by the assembly stub in `boot.asm` after long-mode transition.
        /// `multiboot2_addr` is the physical address of the Multiboot2 info
        /// structure, passed in `rdi` per the SysV x86_64 ABI.
        #[unsafe(no_mangle)]
        pub extern "C" fn kernel_main(multiboot2_addr: u64) -> ! {
            let f: fn(&'static $crate::KernelBootInfo) -> ! = $path;
            // Safety: called once by boot.asm with GRUB's valid MB2 pointer.
            let kbi = unsafe { $crate::parse_boot_info(multiboot2_addr) };
            f(kbi)
        }
    };
}
