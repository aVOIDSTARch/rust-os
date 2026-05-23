//! Minimal barnacle test kernel.
//!
//! Validates the full boot pipeline: GRUB → Multiboot2 header → 32→64-bit
//! transition → Rust entry point.  Writes "OK" to the VGA text buffer at
//! the higher-half virtual address and halts.
//!
//! Expected QEMU output: top-left two characters of the VGA display are "OK"
//! in white-on-black.  No triple-fault reboot.

#![no_std]
#![no_main]

use barnacle::KernelBootInfo;
use core::panic::PanicInfo;

barnacle::entry_point!(kmain);

fn kmain(_kbi: &'static KernelBootInfo) -> ! {
    // Physical 0xB8000 (VGA text buffer) is accessible at:
    //   - 0x000000000000B8000  (identity map, valid until kernel sets up own tables)
    //   - 0xFFFFFFFF800B8000   (higher-half: KERNEL_OFFSET + 0xB8000)
    // Use the higher-half address to verify the page table mapping is correct.
    let vga = (0xFFFFFFFF80000000_usize + 0xB8000) as *mut u16;
    unsafe {
        vga.write_volatile(0x0F4F); // 'O', white-on-black
        vga.add(1).write_volatile(0x0F4B); // 'K'
    }
    loop {
        unsafe { core::arch::asm!("hlt", options(nomem, nostack)); }
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    // Write "!!" to VGA in red-on-black to indicate a panic.
    let vga = (0xFFFFFFFF80000000_usize + 0xB8000) as *mut u16;
    unsafe {
        vga.write_volatile(0x0C21); // '!' red-on-black
        vga.add(1).write_volatile(0x0C21);
    }
    loop {
        unsafe { core::arch::asm!("hlt", options(nomem, nostack)); }
    }
}
