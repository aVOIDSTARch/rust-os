#![no_std] // don't link the Rust standard library
#![no_main] // disable all Rust-level entry points

use core::panic::PanicInfo;

mod vga_buffer;




// this function is the entry point, since the linker looks for a function named `_start` by default
#[unsafe(no_mangle)] // don't mangle the name of this function
pub extern "C" fn _start() -> ! {
    // write the string to the VGA text buffer at 0xb8000
    use core::fmt::Write;
    vga_buffer::WRITER.lock().write_str("Hello World!").unwrap();
    write!(vga_buffer::WRITER.lock(), "The answer to the question of life, the universe and everything is {}.", 42).unwrap();
    // this function must never return, since there is no operating system to return to
    loop {}
}

/// This function is called on panic.
#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {}
}
