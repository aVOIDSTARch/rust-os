//! Integration test: double-fault handler fires correctly on stack overflow.
//!
//! Deliberately triggers infinite recursion to exhaust the stack.  The
//! expected outcome is a double-fault exception, caught by the test-specific
//! IDT loaded here, which prints `[ok]` and exits QEMU with success.

#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

use crusty_os::{exit_qemu, serial_print, serial_println, QemuExitCode};
use lazy_static::lazy_static;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame};

// ── Test-specific IDT ─────────────────────────────────────────────────────────

lazy_static! {
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

pub fn init_test_idt() {
    TEST_IDT.load();
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    crusty_os::test_panic_handler(info)
}

barnacle::entry_point!(test_entry);

fn test_entry(_kbi: &'static crusty_os::KernelBootInfo) -> ! {
    unsafe { platform::init(); }
    serial_print!("stack_overflow::stack_overflow...\t");

    crusty_os::gdt::init();
    init_test_idt();

    stack_overflow();

    panic!("Execution continued after stack overflow");
}

// ── Double-fault handler ──────────────────────────────────────────────────────

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
    volatile::Volatile::new(0).read();
}
