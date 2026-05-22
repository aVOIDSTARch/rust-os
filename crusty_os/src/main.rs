//! crusty_os kernel binary entry point.
//!
//! `kernel_main` is called by the bootloader after it has set up long mode,
//! a page table, and a stack.  It receives a [`BootInfo`] reference describing
//! the physical memory map and the offset at which physical memory is mapped
//! into the virtual address space.
//!
//! Startup sequence:
//! 1. `platform::init()` — UART, framework writer, panic hook
//! 2. [`crusty_os::init()`] — GDT, IDT, PIC, interrupts enabled
//! 3. Memory mapper + frame allocator from `BootInfo`
//! 4. Heap allocator (`allocator::init_heap`)
//! 5. Idle loop (`hlt_loop`)
//!
//! When compiled with `cargo test`, the generated `test_main()` call is
//! injected before the idle loop.  The test runner is `crusty_os::test_runner`
//! (set via `#![test_runner(...)]` below), which runs tests via
//! `framework::runner` and then exits QEMU with the success code.

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(crusty_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

use crusty_os::println;
use bootloader::{BootInfo, entry_point};
use core::panic::PanicInfo;

/// Normal-boot panic: print the panic info over serial and halt QEMU.
#[cfg(not(test))]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    use framework::kprintln;
    kprintln!("{}", info);
    platform::exit_failure()
}

/// Test-mode panic (binary tests via `cargo test`): delegate to the standard
/// test panic handler so QEMU exits with the failure code.
#[cfg(test)]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    crusty_os::test_panic_handler(info)
}

extern crate alloc;
use alloc::{boxed::Box, rc::Rc, vec::Vec, vec};

entry_point!(kernel_main);

/// Main kernel entry point, called by the bootloader.
///
/// Performs full initialization and then halts.  In test builds, the injected
/// `test_main()` runs all collected test cases and exits QEMU before the idle
/// loop is reached.
fn kernel_main(boot_info: &'static BootInfo) -> ! {
    use crusty_os::memory;
    use x86_64::VirtAddr;
    use crusty_os::memory::BootInfoFrameAllocator;
    use crusty_os::allocator;

    unsafe { platform::init(); }

    println!("Hello World{}", "!");

    crusty_os::init();

    let phys_mem_offset = VirtAddr::new(boot_info.physical_memory_offset);
    let mut mapper = unsafe { memory::init(phys_mem_offset) };
    let mut frame_allocator = unsafe { BootInfoFrameAllocator::init(&boot_info.memory_map) };

    allocator::init_heap(&mut mapper, &mut frame_allocator).expect("heap initialization failed");

    // Smoke-test heap allocations in the normal boot path.
    let heap_value = Box::new(41);
    println!("heap_value at {:p}", heap_value);

    let mut vec = Vec::new();
    for i in 0..500 {
        vec.push(i);
    }
    println!("vec at {:p}", vec.as_slice());

    let reference_counted = Rc::new(vec![1, 2, 3]);
    let cloned_reference = reference_counted.clone();
    println!("current reference count is {}", Rc::strong_count(&cloned_reference));
    core::mem::drop(reference_counted);
    println!("current reference count is {}", Rc::strong_count(&cloned_reference));

    println!("It did not crash!");

    #[cfg(test)]
    test_main();

    crusty_os::hlt_loop();
}
