//! crusty_os kernel library crate.
//!
//! Exposes the kernel's hardware-facing subsystems (GDT, IDT, interrupt
//! controllers, memory mapper, heap allocator, VGA text buffer) and
//! re-exports the shared test infrastructure that integration tests depend on.
//!
//! # Architecture
//!
//! ```text
//! crusty_os  ──depends on──▶  framework  (output + test runner, no hardware)
//!    │        ──depends on──▶  platform   (UART, QEMU exit, x86_64 I/O)
//!    │
//!    └── integration tests use crusty_os::test_runner (wraps framework::runner
//!                              + calls platform::exit_success on completion)
//! ```
//!
//! # Test infrastructure
//!
//! Lib-level tests (cargo test `crusty_os` lib) use the entry point
//! `test_kernel_main` below.  Integration tests (in `tests/`) set
//! `#![test_runner(crusty_os::test_runner)]` and call
//! `unsafe { platform::init() }` at their own entry point.
//!
//! [`test_panic_handler`] is provided for integration tests that opt out of
//! the standard harness (`harness = false` in Cargo.toml, e.g. `should_panic`
//! and `stack_overflow`) so they can still report failures via serial.

#![no_std]
#![cfg_attr(test, no_main)]
#![feature(custom_test_frameworks)]
#![test_runner(framework::runner)]
#![reexport_test_harness_main = "test_main"]
#![feature(abi_x86_interrupt)]

use core::panic::PanicInfo;

pub mod serial;
pub mod vga_buffer;
pub mod interrupts;
pub mod gdt;
pub mod memory;
pub mod allocator;

extern crate alloc;

// ── Re-exports for integration tests ─────────────────────────────────────────

/// Re-export so integration tests can write `use crusty_os::Testable` without
/// knowing about the `framework` crate directly.
pub use framework::Testable;

/// Test runner used by all integration tests (`#![test_runner(crusty_os::test_runner)]`).
///
/// Delegates to [`framework::runner`] for output, then calls
/// [`platform::exit_success`] so QEMU exits with the success code after all
/// tests pass.  If any test panics the `#[panic_handler]` calls
/// [`test_panic_handler`] instead, exiting with the failure code.
pub fn test_runner(tests: &[&dyn framework::Testable]) {
    framework::runner(tests);
    platform::exit_success();
}

/// Re-export so integration tests can use `QemuExitCode` without a direct
/// `platform` dependency.
pub use platform::QemuExitCode;

/// Backwards-compatible wrapper: integration tests expect a `()` return type,
/// but [`platform::exit_qemu`] returns `!`.
pub fn exit_qemu(code: QemuExitCode) {
    platform::exit_qemu(code);
}

/// Panic handler for integration tests that use `harness = false`.
///
/// Prints `[failed]` and the panic info over serial, then exits QEMU with
/// the failure code.  Wire up via `#[panic_handler]` in the test file.
pub fn test_panic_handler(info: &PanicInfo) -> ! {
    use framework::kprintln;
    kprintln!("[failed]\n");
    kprintln!("Error: {}\n", info);
    platform::exit_failure()
}

// ── Panic handler (lib-test builds only) ─────────────────────────────────────
//
// This handler is compiled ONLY when the lib is built as its own test binary
// (cargo test --lib). In that mode crusty_os is both the lib and the final
// binary, so exactly one #[panic_handler] exists here.
//
// For the kernel binary the handler lives in main.rs.
// For integration tests (harness = true) the handler lives in each test file
// and delegates to test_panic_handler.
// For integration tests (harness = false, e.g. should_panic / stack_overflow)
// the handler also lives in each test file with custom logic.
//
// This arrangement prevents duplicate-panic-handler link errors: the lib never
// exports a non-test handler, so integration test binaries that define their
// own never see a conflict.

#[cfg(test)]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    test_panic_handler(info)
}

// ── Lib-test entry point ──────────────────────────────────────────────────────

#[cfg(test)]
use bootloader::{entry_point, BootInfo};

/// Entry point for `cargo test -p crusty_os` (lib tests only).
///
/// Mirrors `kernel_main` in setup order: platform init → kernel subsystems →
/// test harness → exit.  Does NOT initialize the heap because lib tests do not
/// exercise the allocator; heap_allocation.rs integration tests handle that.
#[cfg(test)]
entry_point!(test_kernel_main);

#[cfg(test)]
fn test_kernel_main(_boot_info: &'static BootInfo) -> ! {
    unsafe { platform::init(); }
    init();
    test_main();
    platform::exit_success()
}

// ── Kernel subsystem init ─────────────────────────────────────────────────────

/// Initialize core x86_64 kernel subsystems.
///
/// Sets up the GDT, IDT, PIC interrupt controllers, and enables hardware
/// interrupts.  Must be called before any code that relies on interrupts or
/// hardware exception handlers (e.g. page fault, double fault).
pub fn init() {
    gdt::init();
    interrupts::init_idt();
    unsafe { interrupts::PICS.lock().initialize() };
    x86_64::instructions::interrupts::enable();
}

/// Halt the CPU in a loop, yielding on each iteration.
///
/// Used as the idle loop in the normal boot path after all initialization is
/// complete.  The `hlt` instruction reduces power consumption compared to a
/// busy spin.
pub fn hlt_loop() -> ! {
    loop {
        x86_64::instructions::hlt();
    }
}
