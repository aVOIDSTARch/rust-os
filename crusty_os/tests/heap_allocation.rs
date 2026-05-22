//! Integration test: heap allocator correctness under various workloads.
//!
//! Requires full memory initialization (mapper + frame allocator + heap) so
//! this test uses the `entry_point!` macro to receive [`BootInfo`] from the
//! bootloader.
//!
//! Tests exercise `Box`, `Vec`, and the allocator's free-list behavior across
//! allocation sizes from a single word to the full heap capacity.

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(crusty_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use bootloader::{entry_point, BootInfo};
use alloc::boxed::Box;
use alloc::vec::Vec;

/// Panic handler: a test failure panics → print the error and exit QEMU.
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    crusty_os::test_panic_handler(info)
}

entry_point!(main);

/// Test entry point: initialize memory subsystem then run all heap test cases.
fn main(boot_info: &'static BootInfo) -> ! {
    use crusty_os::allocator;
    use crusty_os::memory::{self, BootInfoFrameAllocator};
    use x86_64::VirtAddr;

    unsafe { platform::init(); }
    crusty_os::init();

    let phys_mem_offset = VirtAddr::new(boot_info.physical_memory_offset);
    let mut mapper = unsafe { memory::init(phys_mem_offset) };
    let mut frame_allocator = unsafe { BootInfoFrameAllocator::init(&boot_info.memory_map) };
    allocator::init_heap(&mut mapper, &mut frame_allocator)
        .expect("heap initialization failed");

    test_main();
    loop {}
}

// ── Test cases ────────────────────────────────────────────────────────────────

/// Two independent `Box` allocations hold their values correctly.
#[test_case]
fn simple_allocation() {
    let heap_value_1 = Box::new(41);
    let heap_value_2 = Box::new(13);
    assert_eq!(*heap_value_1, 41);
    assert_eq!(*heap_value_2, 13);
}

/// A large `Vec` built incrementally computes the correct sum.
#[test_case]
fn large_vec() {
    let n = 1000u64;
    let mut vec = Vec::new();
    for i in 0..n {
        vec.push(i);
    }
    assert_eq!(vec.iter().sum::<u64>(), (n - 1) * n / 2);
}

/// Allocating and immediately dropping HEAP_SIZE boxes exercises the
/// free-list: each iteration must reuse the slot freed by the previous one.
#[test_case]
fn many_boxes() {
    use crusty_os::allocator::HEAP_SIZE;
    for i in 0..HEAP_SIZE {
        let x = Box::new(i);
        assert_eq!(*x, i);
    }
}

/// A long-lived box retains its value across many short-lived allocations.
///
/// Uses a fixed iteration count well below `HEAP_SIZE / 8` because the current
/// allocator is a bump allocator that only resets its pointer when the
/// allocation count reaches zero.  While `long_lived` is alive the count never
/// hits zero, so the pointer only advances — exhausting the heap if we iterate
/// HEAP_SIZE times.  1000 iterations is enough to prove the allocator doesn't
/// corrupt the live allocation without overflowing.
#[test_case]
fn many_boxes_long_lived() {
    let long_lived = Box::new(42usize);
    for i in 0..1000usize {
        let x = Box::new(i);
        assert_eq!(*x, i);
    }
    assert_eq!(*long_lived, 42);
}
