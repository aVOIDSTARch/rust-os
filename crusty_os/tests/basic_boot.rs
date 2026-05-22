//! Integration test: serial output and VGA println! work after a minimal boot.
//!
//! Uses bare `_start` (no memory or heap initialization) to verify that
//! `platform::init()` and the framework output macros function correctly
//! before any memory setup is done.  A panic here routes through
//! `crusty_os`'s `#[panic_handler]` → `test_panic_handler` → QEMU failure exit.

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(crusty_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

use crusty_os::println;

/// Panic handler: a test failure panics → print the error and exit QEMU.
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    crusty_os::test_panic_handler(info)
}

/// Minimal boot entry point: no heap, no interrupt setup.
///
/// `crusty_os::test_runner` exits QEMU after all test cases pass, so the
/// trailing `loop {}` is only reached if the runner somehow returns (it won't).
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    unsafe { platform::init(); }
    test_main();
    loop {}
}

// ── Test cases ────────────────────────────────────────────────────────────────

/// VGA println! doesn't panic on a basic string.
#[test_case]
fn test_println_simple() {
    println!("test_println_simple output");
}

/// VGA println! doesn't panic across many lines (tests buffer wrap-around).
#[test_case]
fn test_println_many_lines() {
    for _ in 0..200 {
        println!("test_println_many_lines output");
    }
}

/// Both serial_println! and println! produce output without panicking.
#[test_case]
fn test_serial_and_vga_output() {
    use crusty_os::serial_println;
    serial_println!("serial_println! works");
    println!("println! works");
}
