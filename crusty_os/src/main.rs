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
    use crusty_os::memory::active_level_4_table;
    use x86_64::VirtAddr;

    println!("Hello World{}", "!");

    crusty_os::init();

    let phys_mem_offset = VirtAddr::new(boot_info.physical_memory_offset);
    let level_4_table = unsafe { active_level_4_table(phys_mem_offset)};

    for (i, entry) in level_4_table.iter().enumerate() {
        if entry.is_unused() {
            println!("L4 Entry {}: {:?}", i, entry);
        }

    }

    #[cfg(test)]
    test_main();


    #[cfg(test)]
    test_main();

    println!("It did not crash!");
    crusty_os::hlt_loop();
}
