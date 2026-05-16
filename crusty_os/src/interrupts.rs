// Interrupt handling code.

use x86_64::structures::idt::InterruptDescriptorTable;
use lazy_static::lazy_static;
use crate::println;


// Initialize the IDT.
pub fn init_idt() {
    IDT.load();
}

// The IDT itself.
lazy_static!(
    static ref IDT: InterruptDescriptorTable = {
        let mut idt = InterruptDescriptorTable::new();
        idt.breakpoint.set_handler_fn(breakpoint_handler);
        unsafe {
            idt.double_fault
                .set_handler_fn(double_fault_handler)
                .set_stack_index(crate::gdt::DOUBLE_FAULT_IST_INDEX);
        }
        idt
    };
);

// Double fault exception code // -------------------------------------

// Double fault exception handler.
extern "x86-interrupt" fn double_fault_handler(stack_frame: x86_64::structures::idt::InterruptStackFrame, _error_code: u64) -> ! {
    // println!("EXCEPTION: DOUBLE FAULT\n{:#?}", stack_frame);
    panic!("EXCEPTION: DOUBLE FAULT\n{:#?}", stack_frame);
}







// Breakpoint exception code // -------------------------------------

// Breakpoint exception handler.
extern "x86-interrupt" fn breakpoint_handler(stack_frame: x86_64::structures::idt::InterruptStackFrame) {
    println!("EXCEPTION: BREAKPOINT\n{:#?}", stack_frame);
}

// Test case for the breakpoint exception.
#[test_case]
fn test_breakpoint_exception() {
    x86_64::instructions::interrupts::int3();
}
