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
//! - **[`BootInfo`]**: parsed view of the Multiboot2 information structure that
//!   GRUB passes at boot time.
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
//! use barnacle::BootInfo;
//!
//! barnacle::entry_point!(my_kernel_main);
//!
//! fn my_kernel_main(boot_info: &'static BootInfo) -> ! {
//!     // boot_info.memory_map(), .command_line(), .framebuffer(), ...
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

use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicBool, Ordering};

use multiboot2::{BootInformation, BootInformationHeader};

static INIT_CALLED: AtomicBool = AtomicBool::new(false);

// Wraps MaybeUninit in UnsafeCell to avoid Rust 2024 `static_mut_refs` lint.
// Written exactly once in `init` before any shared reference is handed out.
struct BootInfoCell(core::cell::UnsafeCell<MaybeUninit<BootInfo>>);
// Safety: bare-metal single-core; written once, read-only afterward.
unsafe impl Sync for BootInfoCell {}
static BOOT_INFO: BootInfoCell =
    BootInfoCell(core::cell::UnsafeCell::new(MaybeUninit::uninit()));

/// Parse the Multiboot2 information structure and return a `'static` reference.
///
/// Called by the [`entry_point!`] macro.  Must not be called directly.
///
/// # Safety
///
/// - `addr` must be the value of `rdi` on entry to `kernel_main` as set by
///   barnacle's assembly stub (the physical address GRUB placed in `ebx`).
/// - Must be called exactly once.
/// - The Multiboot2 structure at `addr` must remain unmodified and mapped for
///   the lifetime of the kernel.
pub unsafe fn init(addr: u64) -> &'static BootInfo {
    assert!(
        !INIT_CALLED.swap(true, Ordering::SeqCst),
        "barnacle::init called more than once"
    );

    // Safety: addr is GRUB-provided, 8-byte aligned, and points to memory
    // that is identity-mapped and valid for the full kernel lifetime.
    // Annotating 'static is sound: the MB2 structure lives for the entire run.
    let raw: BootInformation<'static> = unsafe {
        BootInformation::load(addr as *const BootInformationHeader)
            .expect("barnacle: invalid Multiboot2 information structure")
    };

    unsafe {
        // Get a raw pointer from the UnsafeCell — no reference to static mut.
        let ptr: *mut MaybeUninit<BootInfo> = BOOT_INFO.0.get();
        // Write via raw pointer (does not create a &mut reference).
        ptr.write(MaybeUninit::new(BootInfo::new(raw)));
        // Return a shared reference derived from the raw pointer, not the static.
        &*(ptr as *const BootInfo)
    }
}

/// Declare the kernel entry point, type-checking its signature.
///
/// The function provided must have the signature:
/// ```ignore
/// fn name(boot_info: &'static BootInfo) -> !
/// ```
///
/// barnacle's assembly stub calls `kernel_main`; this macro defines that
/// symbol and wires it to the user-supplied function.
///
/// # Example
///
/// ```ignore
/// barnacle::entry_point!(my_entry);
///
/// fn my_entry(boot_info: &'static barnacle::BootInfo) -> ! {
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
            // Type-check: compile error if $path does not match the expected signature.
            let f: fn(&'static $crate::BootInfo) -> ! = $path;
            // Safety: called once by boot.asm with GRUB's valid MB2 pointer.
            let boot_info = unsafe { $crate::init(multiboot2_addr) };
            f(boot_info)
        }
    };
}
