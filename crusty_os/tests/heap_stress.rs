//! Integration test: heap stress patterns beyond `heap_allocation.rs`.
//!
//! Exercises `String`, `Rc`, nested `Vec`, and bulk alloc/dealloc cycles.
//! All tests require full heap initialization via `allocator::init_heap`.

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(crusty_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use alloc::{boxed::Box, rc::Rc, string::String, vec::Vec};
use bootloader::{entry_point, BootInfo};

/// Panic handler: print the failure message over serial, then exit QEMU.
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    crusty_os::test_panic_handler(info)
}

entry_point!(main);

fn main(boot_info: &'static BootInfo) -> ! {
    use crusty_os::{
        allocator,
        memory::{self, BootInfoFrameAllocator},
    };
    use x86_64::VirtAddr;

    unsafe { platform::init(); }
    crusty_os::init();

    let phys_offset = VirtAddr::new(boot_info.physical_memory_offset);
    let mut mapper  = unsafe { memory::init(phys_offset) };
    let mut frames  = unsafe { BootInfoFrameAllocator::init(&boot_info.memory_map) };
    allocator::init_heap(&mut mapper, &mut frames).expect("heap init failed");

    test_main();
    loop {}
}

// ── Test cases ────────────────────────────────────────────────────────────────

/// String allocation and concatenation preserves the correct bytes.
#[test_case]
fn test_string_alloc_and_concat() {
    let s = String::from("crusty_");
    let t = s + "os";
    assert_eq!(t.len(), 9);
    assert_eq!(t.as_str(), "crusty_os");
}

/// A `Vec<Box<u32>>` retains all elements after construction.
#[test_case]
fn test_vec_of_boxes() {
    let v: Vec<Box<u32>> = (0u32..32).map(Box::new).collect();
    for (i, b) in v.iter().enumerate() {
        assert_eq!(**b, i as u32);
    }
}

/// `Rc<T>` reference counting increments and decrements correctly.
#[test_case]
fn test_rc_reference_counting() {
    let a = Rc::new(0xdeadu32);
    let b = Rc::clone(&a);
    assert_eq!(Rc::strong_count(&a), 2);
    drop(b);
    assert_eq!(Rc::strong_count(&a), 1);
    assert_eq!(*a, 0xdead);
}

/// Nested `Vec<Vec<u8>>` exercises the allocator across multiple size classes.
#[test_case]
fn test_nested_vec() {
    let outer: Vec<Vec<u8>> = (0u8..8).map(|n| (0..n).collect()).collect();
    assert_eq!(outer.len(), 8);
    assert_eq!(outer[7].len(), 7);
    assert_eq!(outer[7][6], 6);
}

/// Allocate 256 boxes into a `Vec`, verify all values, then drop all at once.
///
/// Bulk drops stress the allocator's free-list bookkeeping: 256 consecutive
/// deallocations must not corrupt the free list for subsequent allocations.
#[test_case]
fn test_bulk_alloc_then_free() {
    let boxes: Vec<Box<u64>> = (0u64..256).map(Box::new).collect();
    for (i, b) in boxes.iter().enumerate() {
        assert_eq!(**b, i as u64);
    }
    // `boxes` drops here — 256 deallocs in one go.
}

/// A long-lived `Box` retains its value across many short-lived allocations.
///
/// Ensures the allocator does not corrupt a live allocation while recycling
/// memory for interleaved short-lived objects.
#[test_case]
fn test_long_lived_box_survives_churn() {
    let sentinel = Box::new(0xCAFE_BABEu64);
    for i in 0..500u64 {
        let tmp = Box::new(i);
        assert_eq!(*tmp, i);
    }
    assert_eq!(*sentinel, 0xCAFE_BABE);
}
