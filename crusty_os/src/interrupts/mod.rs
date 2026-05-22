// interrupts/mod.rs
//
// Top-level interrupt subsystem.
//
// Initialization order matters:
//   1. exceptions::init()   — loads the IDT (must happen before any faults)
//   2. pic::init()          — unmasks PIC IRQs (starts hardware interrupts)
//
// If APIC support is desired in future, replace step 2 with apic::init(),
// which must first disable the 8259 before enabling the local APIC.

pub mod apic;
pub mod dispatch;
pub mod exceptions;
pub mod handlers;
pub mod pic;
pub mod stats;
pub mod vectors;

pub use vectors::InterruptVector;

use crate::println;

/// Initialize the entire interrupt subsystem in the correct order.
/// Panics if any step fails — there is no meaningful recovery from a
/// broken IDT or masked PIC at boot time.
pub fn init() {
    exceptions::init();
    pic::init();
    println!("[interrupts] subsystem initialized");
}
