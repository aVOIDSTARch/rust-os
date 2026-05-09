#![no_std] // don't link the Rust standard library
#![no_main] // disable all Rust-level entry points
#![feature(custom_test_frameworks)]
#![test_runner(crate::test_runner)]

use core::panic::PanicInfo;

mod vga_buffer;




// this function is the entry point, since the linker looks for a function named `_start` by default
#[unsafe(no_mangle)] // don't mangle the name of this function
pub extern "C" fn _start() -> ! {
    // write the string to the VGA text buffer at 0xb8000
    println!("Hello World{}", "!");
    // this function must never return, since there is no operating system to return to
    loop {}
}

/// This function is called on panic.
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    println!("{}", info);
    loop {}
}

#[cfg(test)]
pub fn test_runner(tests: &[&dyn Fn()]) {
    println!("Running {} tests", tests.len());
    for test in tests {
        test();
    }
}
