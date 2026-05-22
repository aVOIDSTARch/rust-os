// interrupts/exceptions.rs
//
// IDT construction and loading.
//
// Covers all 32 Intel-defined CPU exception vectors (0x00–0x1F) plus the
// 16 remapped 8259 PIC IRQ vectors (0x20–0x2F).
//
// Each exception has its own IST stack index only where the Intel SDM
// *requires* a known-good stack (double fault) or where a fault in the
// handler itself would otherwise be unrecoverable (NMI, machine check).
// Overusing IST stacks wastes TSS resources and is architecturally
// unnecessary for faults that only occur in user mode or at low frequency.
//
// IST assignments (must match gdt::* constants):
//   IST1 → double fault
//   IST2 → NMI
//   IST3 → machine check

use lazy_static::lazy_static;
use x86_64::structures::idt::InterruptDescriptorTable;

use super::handlers;
use super::vectors::InterruptVector;

lazy_static! {
    static ref IDT: InterruptDescriptorTable = {
        let mut idt = InterruptDescriptorTable::new();

        // ---------------------------------------------------------------
        // CPU exceptions — vectors 0x00–0x1F
        // ---------------------------------------------------------------

        idt.divide_error
            .set_handler_fn(handlers::divide_error_handler);

        idt.debug
            .set_handler_fn(handlers::debug_handler);

        unsafe {
            idt.non_maskable_interrupt
                .set_handler_fn(handlers::nmi_handler)
                // NMI must have its own stack: an NMI arriving while the
                // main stack pointer is invalid would otherwise triple-fault.
                .set_stack_index(crate::gdt::NMI_IST_INDEX);
        }

        idt.breakpoint
            .set_handler_fn(handlers::breakpoint_handler);

        idt.overflow
            .set_handler_fn(handlers::overflow_handler);

        idt.bound_range_exceeded
            .set_handler_fn(handlers::bound_range_handler);

        idt.invalid_opcode
            .set_handler_fn(handlers::invalid_opcode_handler);

        idt.device_not_available
            .set_handler_fn(handlers::device_not_available_handler);

        unsafe {
            idt.double_fault
                .set_handler_fn(handlers::double_fault_handler)
                .set_stack_index(crate::gdt::DOUBLE_FAULT_IST_INDEX);
        }

        // Vector 9 (coprocessor segment overrun) is obsolete on all CPUs
        // since the 486.  Leave it unset — the CPU will deliver a #GP instead.

        idt.invalid_tss
            .set_handler_fn(handlers::invalid_tss_handler);

        idt.segment_not_present
            .set_handler_fn(handlers::segment_not_present_handler);

        idt.stack_segment_fault
            .set_handler_fn(handlers::stack_segment_handler);

        idt.general_protection_fault
            .set_handler_fn(handlers::general_protection_handler);

        idt.page_fault
            .set_handler_fn(handlers::page_fault_handler);

        // Vector 15 is Intel-reserved; do not set.

        idt.x87_floating_point
            .set_handler_fn(handlers::x87_fp_handler);

        idt.alignment_check
            .set_handler_fn(handlers::alignment_check_handler);

        unsafe {
            idt.machine_check
                .set_handler_fn(handlers::machine_check_handler)
                // MCE must have its own stack for the same reason as NMI.
                .set_stack_index(crate::gdt::MACHINE_CHECK_IST_INDEX);
        }

        idt.simd_floating_point
            .set_handler_fn(handlers::simd_fp_handler);

        idt.virtualization
            .set_handler_fn(handlers::virtualization_handler);

        // Vectors 0x15–0x1B are Intel-reserved; leave unset.

        // 0x1C–0x1E are AMD SVM / CET extensions; skip unless targeting AMD.
        // 0x1F is reserved.

        // ---------------------------------------------------------------
        // Hardware IRQs — vectors 0x20–0x2F (remapped 8259 PIC)
        // ---------------------------------------------------------------

        idt[InterruptVector::Timer.as_usize()]
            .set_handler_fn(handlers::timer_handler);

        idt[InterruptVector::Keyboard.as_usize()]
            .set_handler_fn(handlers::keyboard_handler);

        // IRQ2 (cascade) is managed by the PIC internally; we never see it.

        idt[InterruptVector::SpuriousMaster.as_usize()]
            .set_handler_fn(handlers::spurious_master_handler);

        idt[InterruptVector::Rtc.as_usize()]
            .set_handler_fn(handlers::rtc_handler);

        idt[InterruptVector::Mouse.as_usize()]
            .set_handler_fn(handlers::mouse_handler);

        idt[InterruptVector::AtaPrimary.as_usize()]
            .set_handler_fn(handlers::ata_primary_handler);

        // IRQ15 / spurious slave share the same vector; handler checks ISR.
        idt[InterruptVector::AtaSecondary.as_usize()]
            .set_handler_fn(handlers::ata_secondary_handler);

        idt
    };
}

/// Load the IDT into the CPU's IDTR register.
///
/// Must be called before any exception or interrupt can be safely handled.
/// Typically called early in the kernel boot sequence, after GDT/TSS setup.
pub fn init() {
    IDT.load();
}

// ---------------------------------------------------------------------------
// Integration test
// ---------------------------------------------------------------------------

#[test_case]
fn test_breakpoint_does_not_panic() {
    // A breakpoint must be handled as a trap (resume after INT3), not a fault.
    x86_64::instructions::interrupts::int3();
    // If execution reaches here, the handler returned correctly.
}
