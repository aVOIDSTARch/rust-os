//! Integration test: heap allocator correctness under various workloads.
//!
//! Uses `barnacle::entry_point!` to receive `KernelBootInfo` and calls
//! `crusty_os::allocator_init` to bring up the TLSF-backed global heap.
//!
//! Tests exercise `Box`, `Vec`, and the allocator's free-list behavior.

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(crusty_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use alloc::boxed::Box;
use alloc::vec::Vec;

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    crusty_os::test_panic_handler(info)
}

barnacle::entry_point!(main);

fn main(kbi: &'static crusty_os::KernelBootInfo) -> ! {
    unsafe { platform::init(); }
    crusty_os::init();
    unsafe { crusty_os::allocator_init(kbi); }
    test_main();
    loop {}
}

// ── Test cases ────────────────────────────────────────────────────────────────

#[test_case]
fn simple_allocation() {
    let heap_value_1 = Box::new(41);
    let heap_value_2 = Box::new(13);
    assert_eq!(*heap_value_1, 41);
    assert_eq!(*heap_value_2, 13);
}

#[test_case]
fn large_vec() {
    let n = 1000u64;
    let mut vec = Vec::new();
    for i in 0..n {
        vec.push(i);
    }
    assert_eq!(vec.iter().sum::<u64>(), (n - 1) * n / 2);
}

/// Allocate and immediately drop boxes repeatedly; each iteration reuses freed memory.
#[test_case]
fn many_boxes() {
    for i in 0..10_000usize {
        let x = Box::new(i);
        assert_eq!(*x, i);
    }
}

/// A long-lived box retains its value across many short-lived allocations.
#[test_case]
fn many_boxes_long_lived() {
    let long_lived = Box::new(42usize);
    for i in 0..1000usize {
        let x = Box::new(i);
        assert_eq!(*x, i);
    }
    assert_eq!(*long_lived, 42);
}
