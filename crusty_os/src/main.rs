// in src/main.rs

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(crusty_os::test_runner)]
#![reexport_test_harness_main = "test_main"]


use crusty_os::println;

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    println!("Hello World{}", "!");

    crusty_os::init();

    // trigger a page fault

    #[cfg(test)]
    test_main();

    println!("It did not crash!");
    crusty_os::hlt_loop();
}
