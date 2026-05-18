// in src/main.rs

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(crusty_os::test_runner)]
#![reexport_test_harness_main = "test_main"]


use crusty_os::println;

use bootloader::{BootInfo, entry_point};

entry_point!(kernel_main);

fn kernel_main(boot_info: &'static BootInfo) -> ! {
    use crusty_os::memory;
    use x86_64::{structures::paging::Page, VirtAddr};
    use crusty_os::memory::BootInfoFrameAllocator;

    println!("Hello World{}", "!");

    crusty_os::init();

    let phys_mem_offset = VirtAddr::new(boot_info.physical_memory_offset);
    // Initialize the memory mapper and the frame allocator.
    let mut mapper = unsafe { memory::init(phys_mem_offset) };
    let mut frame_allocator = unsafe { BootInfoFrameAllocator::init(&boot_info.memory_map) };

     // Map the page containing the VGA text buffer to an unused virtual address.
    let page = Page::containing_address(VirtAddr::new(0xdeadbeaf000));
    memory::create_example_mapping(page, &mut mapper, &mut frame_allocator);

    let page_ptr: *mut u64 = page.start_address().as_mut_ptr();
    unsafe { page_ptr.offset(400).write_volatile(0x_f021_f077_f065_f04e) };

    println!("It did not crash!");

    #[cfg(test)]
    test_main();


    #[cfg(test)]
    test_main();


    #[cfg(test)]
    test_main();

    println!("It did not crash!");
    crusty_os::hlt_loop();
}
