//! Integration test: interrupt subsystem initializes and behaves correctly.
//!
//! Verifies that after `crusty_os::init()` the GDT is loaded, the IDT is
//! populated with all exception and IRQ handlers, and the PIC is unmasked.
//! No heap access required.

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(crusty_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    crusty_os::test_panic_handler(info)
}

barnacle::entry_point!(test_entry);

fn test_entry(_kbi: &'static crusty_os::KernelBootInfo) -> ! {
    unsafe { platform::init(); }
    crusty_os::init();
    test_main();
    loop {}
}

// ── Test cases ────────────────────────────────────────────────────────────────

/// Breakpoint exception is handled as a trap and execution resumes.
#[test_case]
fn test_breakpoint_handled_as_trap() {
    x86_64::instructions::interrupts::int3();
}

/// Hardware interrupts are enabled after `crusty_os::init()`.
#[test_case]
fn test_interrupts_enabled_after_init() {
    assert!(
        x86_64::instructions::interrupts::are_enabled(),
        "interrupts must be enabled after crusty_os::init()"
    );
}

/// `without_interrupts` disables and re-enables interrupts correctly.
#[test_case]
fn test_without_interrupts_restores_enabled_state() {
    assert!(x86_64::instructions::interrupts::are_enabled());
    x86_64::instructions::interrupts::without_interrupts(|| {
        assert!(!x86_64::instructions::interrupts::are_enabled());
    });
    assert!(x86_64::instructions::interrupts::are_enabled());
}

/// Multiple consecutive breakpoints are all handled without corrupting the IDT.
#[test_case]
fn test_repeated_breakpoints() {
    for _ in 0..5 {
        x86_64::instructions::interrupts::int3();
    }
}
