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

// ── Re-exports for backwards compat with integration tests ────────────────────

pub use framework::Testable;

/// Wraps framework::runner and exits QEMU on success.
/// Used by integration tests and the binary test runner.
pub fn test_runner(tests: &[&dyn framework::Testable]) {
    framework::runner(tests);
    platform::exit_success();
}

pub use platform::QemuExitCode;

/// Backwards-compat wrapper: integration tests expect () return type.
pub fn exit_qemu(code: QemuExitCode) {
    platform::exit_qemu(code);
}

pub fn test_panic_handler(info: &PanicInfo) -> ! {
    use framework::kprintln;
    kprintln!("[failed]\n");
    kprintln!("Error: {}\n", info);
    platform::exit_failure()
}

// ── Panic handlers (crusty_os owns the #[panic_handler] symbol) ───────────────

#[cfg(not(test))]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    use framework::kprintln;
    kprintln!("{}", info);
    platform::exit_failure()
}

#[cfg(test)]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    test_panic_handler(info)
}

// ── Test entry point ──────────────────────────────────────────────────────────

#[cfg(test)]
use bootloader::{entry_point, BootInfo};

#[cfg(test)]
entry_point!(test_kernel_main);

#[cfg(test)]
fn test_kernel_main(_boot_info: &'static BootInfo) -> ! {
    unsafe { platform::init(); }
    init();
    test_main();           // calls framework::runner (lib-test runner attr above)
    platform::exit_success()
}

// ── Kernel init and halt ──────────────────────────────────────────────────────

pub fn init() {
    gdt::init();
    interrupts::init_idt();
    unsafe { interrupts::PICS.lock().initialize() };
    x86_64::instructions::interrupts::enable();
}

pub fn hlt_loop() -> ! {
    loop {
        x86_64::instructions::hlt();
    }
}
