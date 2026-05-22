//! Integration test: double-fault handler fires correctly on stack overflow.
//!
//! Deliberately triggers infinite recursion to exhaust the stack.  The
//! expected outcome is a double-fault exception, caught by the test-specific
//! IDT loaded here, which prints `[ok]` and exits QEMU with success.
//!
//! If execution somehow continues after the overflow (tail-call optimization,
//! or the double fault wasn't caught), the `panic!` at the end ensures the
//! test fails rather than hanging.
//!
//! Uses `harness = false` (see Cargo.toml) because it bypasses the normal
//! test runner — the "pass" condition is the double fault itself.

#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

use crusty_os::{exit_qemu, serial_print, serial_println, QemuExitCode};
use lazy_static::lazy_static;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame};

// ── Test-specific IDT ─────────────────────────────────────────────────────────

lazy_static! {
    /// A minimal IDT with only a double-fault handler, using the IST slot
    /// configured by `crusty_os::gdt` for the kernel double-fault stack.
    static ref TEST_IDT: InterruptDescriptorTable = {
        let mut idt = InterruptDescriptorTable::new();
        unsafe {
            idt.double_fault
                .set_handler_fn(test_double_fault_handler)
                .set_stack_index(crusty_os::gdt::DOUBLE_FAULT_IST_INDEX);
        }
        idt
    };
}

/// Load the test-specific IDT (replaces the kernel's normal IDT for this run).
pub fn init_test_idt() {
    TEST_IDT.load();
}

// ── Entry point ───────────────────────────────────────────────────────────────

/// Initialize GDT (for IST), load the test IDT, then trigger the overflow.
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    unsafe { platform::init(); }
    serial_print!("stack_overflow::stack_overflow...\t");

    crusty_os::gdt::init();
    init_test_idt();

    stack_overflow();

    panic!("Execution continued after stack overflow");
}

// ── Double-fault handler ──────────────────────────────────────────────────────

/// Invoked by the CPU on double fault: print success and exit QEMU.
extern "x86-interrupt" fn test_double_fault_handler(
    _stack_frame: InterruptStackFrame,
    _error_code: u64,
) -> ! {
    serial_println!("[ok]");
    exit_qemu(QemuExitCode::Success);
    loop {}
}

// ── Stack overflow trigger ────────────────────────────────────────────────────

#[allow(unconditional_recursion)]
fn stack_overflow() {
    stack_overflow();
    // Volatile read prevents the compiler from optimizing this into a tail call.
    volatile::Volatile::new(0).read();
}

// ── Panic handler ─────────────────────────────────────────────────────────────

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    crusty_os::test_panic_handler(info)
}
