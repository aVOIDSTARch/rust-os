//! Integration test: verifies that a deliberate assertion failure causes a panic.
//!
//! This test binary does NOT use the standard test harness (`harness = false`).
//! It manually calls `should_fail()`, which must panic.  If it panics →
//! `#[panic_handler]` prints `[ok]` and exits with success.  If it somehow
//! returns without panicking → exits with failure.

#![no_std]
#![no_main]

use crusty_os::{exit_qemu, serial_print, serial_println, QemuExitCode};

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    serial_println!("[ok]");
    exit_qemu(QemuExitCode::Success);
    loop {}
}

barnacle::entry_point!(test_entry);

fn test_entry(_kbi: &'static crusty_os::KernelBootInfo) -> ! {
    unsafe { platform::init(); }
    should_fail();
    serial_println!("[test did not panic]");
    exit_qemu(QemuExitCode::Failure);
    loop {}
}

fn should_fail() {
    serial_print!("should_panic::should_fail...\t");
    assert_eq!(0, 1);
}
