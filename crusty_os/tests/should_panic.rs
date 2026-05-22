//! Integration test: verifies that a deliberate assertion failure causes a panic.
//!
//! This test binary does NOT use the standard test harness (`harness = false`
//! in Cargo.toml).  It manually runs `should_fail()`, which must panic.
//! If it panics → `#[panic_handler]` below prints `[ok]` and exits with
//! success.  If it somehow returns without panicking → exits with failure.
//!
//! The test validates the panic path works end-to-end: from a Rust `assert_eq!`
//! all the way through the `#[panic_handler]` to QEMU exit.

#![no_std]
#![no_main]

use crusty_os::{exit_qemu, serial_print, serial_println, QemuExitCode};

/// Entry point: call should_fail() and report if it unexpectedly doesn't panic.
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    unsafe { platform::init(); }
    should_fail();
    serial_println!("[test did not panic]");
    exit_qemu(QemuExitCode::Failure);
    loop {}
}

/// Deliberately triggers a panic via a failing assertion.
fn should_fail() {
    serial_print!("should_panic::should_fail...\t");
    assert_eq!(0, 1);
}

/// Panic handler for this test: a panic from `should_fail` means the test passed.
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    serial_println!("[ok]");
    exit_qemu(QemuExitCode::Success);
    loop {}
}
