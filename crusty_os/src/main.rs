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
    use x86_64::{structures::paging::Translate, VirtAddr};

    println!("Hello World{}", "!");

    crusty_os::init();

    let phys_mem_offset = VirtAddr::new(boot_info.physical_memory_offset);
    // Initialize the memory mapper and the frame allocator.
    let mapper = unsafe { memory::init(phys_mem_offset) };

    let addresses = [
        0xb8000,
        0x201008,
        0x0100_0020_1a10,
        boot_info.physical_memory_offset,
    ];

    for &address in &addresses {
        let virt = VirtAddr::new(address);
        let phys = mapper.translate_addr(virt);
        println!("virtual address: {:?} -> physical address: {:?}", virt, phys);
    }

    #[cfg(test)]
    test_main();


    #[cfg(test)]
    test_main();

    println!("It did not crash!");
    crusty_os::hlt_loop();
}
