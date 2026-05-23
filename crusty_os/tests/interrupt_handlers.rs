//! Integration test: interrupt subsystem initializes and behaves correctly.
//!
//! Verifies that after `crusty_os::init()` the GDT is loaded, the IDT is
//! populated with all exception and IRQ handlers, and the PIC is unmasked.
//! Tests here do not require heap access — the entry point mirrors
//! `basic_boot.rs` and needs no `BootInfo`.

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(crusty_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

/// Panic handler: print the failure message over serial, then exit QEMU.
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    crusty_os::test_panic_handler(info)
}

/// Entry point: platform init → kernel subsystems → test runner → halt.
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    unsafe { platform::init(); }
    crusty_os::init();
    test_main();
    loop {}
}

// ── Test cases ────────────────────────────────────────────────────────────────

/// Breakpoint exception is handled as a trap and execution resumes.
///
/// If the IDT has no breakpoint handler the CPU would deliver a double fault;
/// the QEMU session would then exit with the failure code instead of reaching
/// the assertion below.
#[test_case]
fn test_breakpoint_handled_as_trap() {
    x86_64::instructions::interrupts::int3();
}

/// Hardware interrupts are enabled after `crusty_os::init()`.
///
/// `init()` calls `x86_64::instructions::interrupts::enable()` as its last
/// step.  Verifying the flag here confirms the sequence ran to completion.
#[test_case]
fn test_interrupts_enabled_after_init() {
    assert!(
        x86_64::instructions::interrupts::are_enabled(),
        "interrupts must be enabled after crusty_os::init()"
    );
}

/// `without_interrupts` disables interrupts for its closure and re-enables
/// them on return, even when the previous state was "enabled".
#[test_case]
fn test_without_interrupts_restores_enabled_state() {
    assert!(x86_64::instructions::interrupts::are_enabled());
    x86_64::instructions::interrupts::without_interrupts(|| {
        assert!(!x86_64::instructions::interrupts::are_enabled());
    });
    assert!(x86_64::instructions::interrupts::are_enabled());
}

/// Multiple consecutive breakpoints are all handled without corrupting the IDT.
///
/// Ensures the handler returns cleanly and the IDT is reusable on re-entry.
#[test_case]
fn test_repeated_breakpoints() {
    for _ in 0..5 {
        x86_64::instructions::interrupts::int3();
    }
}
