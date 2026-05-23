//! Integration test: serial output and VGA println! work after a minimal boot.
//!
//! Verifies that `platform::init()` and the framework output macros function
//! correctly before any heap setup is done.

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(crusty_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

use crusty_os::println;

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    crusty_os::test_panic_handler(info)
}

barnacle::entry_point!(test_entry);

fn test_entry(_kbi: &'static crusty_os::KernelBootInfo) -> ! {
    unsafe { platform::init(); }
    test_main();
    loop {}
}

// ── Test cases ────────────────────────────────────────────────────────────────

#[test_case]
fn test_println_simple() {
    println!("test_println_simple output");
}

#[test_case]
fn test_println_many_lines() {
    for _ in 0..200 {
        println!("test_println_many_lines output");
    }
}

#[test_case]
fn test_serial_and_vga_output() {
    use crusty_os::serial_println;
    serial_println!("serial_println! works");
    println!("println! works");
}
