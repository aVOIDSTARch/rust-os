//! crusty_os kernel entry point.
//!
//! Build with the default `use-bootloader` feature for the legacy bootloader,
//! or `--no-default-features --features use-barnacle` for Multiboot2/GRUB via barnacle.

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(crusty_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

use core::panic::PanicInfo;
use crusty_os::println;

// ── Entry point ──────────────────────────────────────────────────────────────

#[cfg(feature = "use-bootloader")]
bootloader::entry_point!(kmain);

#[cfg(feature = "use-barnacle")]
barnacle::entry_point!(kmain);

// ── Panic handlers ───────────────────────────────────────────────────────────

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
    crusty_os::test_panic_handler(info)
}

// ── Bootloader path ──────────────────────────────────────────────────────────

#[cfg(feature = "use-bootloader")]
extern crate alloc;

#[cfg(feature = "use-bootloader")]
fn kmain(boot_info: &'static bootloader::BootInfo) -> ! {
    use alloc::{boxed::Box, rc::Rc, vec, vec::Vec};
    use crusty_os::{allocator, memory};
    use crusty_os::memory::BootInfoFrameAllocator;
    use x86_64::VirtAddr;

    unsafe { platform::init(); }

    println!("Hello World{}", "!");

    crusty_os::init();

    let phys_mem_offset = VirtAddr::new(boot_info.physical_memory_offset);
    let mut mapper = unsafe { memory::init(phys_mem_offset) };
    let mut frame_allocator = unsafe { BootInfoFrameAllocator::init(&boot_info.memory_map) };

    allocator::init_heap(&mut mapper, &mut frame_allocator).expect("heap initialization failed");

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

// ── Barnacle / Multiboot2 path ───────────────────────────────────────────────

#[cfg(feature = "use-barnacle")]
fn kmain(boot_info: &'static barnacle::BootInfo) -> ! {
    unsafe { platform::init(); }

    println!("Hello from barnacle!");

    crusty_os::init();

    // Physical memory layout via Multiboot2 memory map.
    // Full memory init (mapper + heap) is a follow-up once crusty_os's memory
    // module is ported away from bootloader's physical_memory_offset.
    if let Some(memory_map) = boot_info.memory_map() {
        for area in memory_map.memory_areas() {
            println!(
                "  mem {:016x}–{:016x}",
                area.start_address(),
                area.end_address(),
            );
        }
    }

    println!("It did not crash!");

    crusty_os::hlt_loop();
}
