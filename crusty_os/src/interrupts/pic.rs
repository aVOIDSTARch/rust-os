// interrupts/pic.rs
//
// 8259A Programmable Interrupt Controller (PIC) management.
//
// The PC/AT uses two cascaded 8259 PICs:
//   Master (PIC1): IRQ0–IRQ7,  I/O ports 0x20 / 0x21
//   Slave  (PIC2): IRQ8–IRQ15, I/O ports 0xA0 / 0xA1
//
// By default the BIOS maps them to vectors 0x08–0x0F (master) and
// 0x70–0x77 (slave), which collide with CPU exceptions.  We remap them
// to vectors 0x20–0x2F immediately.
//
// SPURIOUS IRQs
// -------------
// When the CPU sends an INTA pulse to acknowledge an interrupt but the
// PIC has already cleared the request internally (a timing race), the
// PIC raises a "spurious" IRQ on its lowest-priority line:
//   Master spurious → IRQ7  (vector 0x27)
//   Slave  spurious → IRQ15 (vector 0x2F)
//
// A spurious IRQ from the master must NOT receive an EOI.
// A spurious IRQ from the slave MUST send an EOI to the master only
// (because the master's cascade line did assert).
//
// Distinguishing spurious IRQs requires reading the PIC's In-Service
// Register (ISR) before sending EOI.  This module handles all of that.

use pic8259::ChainedPics;
use spin::Mutex;
use x86_64::instructions::port::Port;

use super::vectors::{PIC_1_OFFSET, PIC_2_OFFSET, PIC_IRQ_LPT1, PIC_IRQ_ATA_SECONDARY};

// ---------------------------------------------------------------------------
// PIC instance
// ---------------------------------------------------------------------------

pub static PICS: Mutex<ChainedPics> = Mutex::new(unsafe {
    ChainedPics::new(PIC_1_OFFSET, PIC_2_OFFSET)
});

// ---------------------------------------------------------------------------
// Initialization
// ---------------------------------------------------------------------------

/// Initialize and unmask the chained 8259 PICs.
///
/// Called exactly once during boot, after the IDT has been loaded.
/// Enabling interrupts (`sti`) happens implicitly when we return to a
/// context that sets IF — do **not** call `sti` here; that belongs to
/// the caller's boot sequence after all subsystems are ready.
pub fn init() {
    unsafe {
        PICS.lock().initialize();
    }
}

// ---------------------------------------------------------------------------
// EOI helpers
// ---------------------------------------------------------------------------

/// Send a non-spurious EOI for the given vector.
///
/// # Safety
/// Must only be called from within a hardware interrupt handler for `vector`.
/// Sending a spurious EOI can corrupt PIC state.
#[inline]
pub unsafe fn end_of_interrupt(vector: u8) {
    unsafe { PICS.lock().notify_end_of_interrupt(vector); }
}

/// Check the master PIC's In-Service Register and send EOI only if the
/// IRQ7 is genuine (not spurious).
///
/// Returns `true` if the interrupt was real (EOI sent), `false` if spurious.
///
/// # Safety
/// Must be called from the IRQ7 handler before any EOI.
pub unsafe fn eoi_if_not_spurious_master() -> bool {
    unsafe {
        if master_isr_bit(7) {
            end_of_interrupt(PIC_IRQ_LPT1);
            true
        } else {
            false
        }
    }
}

/// Check slave PIC's ISR for IRQ15; send EOI to master only if spurious,
/// or full EOI to both if genuine.
///
/// Returns `true` if the interrupt was real, `false` if spurious.
///
/// # Safety
/// Must be called from the IRQ15 handler before any EOI.
pub unsafe fn eoi_if_not_spurious_slave() -> bool {
    unsafe {
        if slave_isr_bit(7) {
            end_of_interrupt(PIC_IRQ_ATA_SECONDARY);
            true
        } else {
            master_eoi_only();
            false
        }
    }
}

// ---------------------------------------------------------------------------
// Mask / unmask individual IRQ lines
// ---------------------------------------------------------------------------

/// Mask (disable) an individual IRQ line (0–15).
///
/// Masked IRQs are not delivered to the CPU until unmasked.
/// Use to quiesce a noisy or unhandled device.
pub fn mask_irq(irq: u8) {
    assert!(irq < 16, "IRQ number out of range");
    unsafe {
        if irq < 8 {
            let mut port: Port<u8> = Port::new(0x21);
            let mask = port.read() | (1 << irq);
            port.write(mask);
        } else {
            let mut port: Port<u8> = Port::new(0xA1);
            let mask = port.read() | (1 << (irq - 8));
            port.write(mask);
        }
    }
}

/// Unmask (enable) an individual IRQ line (0–15).
pub fn unmask_irq(irq: u8) {
    assert!(irq < 16, "IRQ number out of range");
    unsafe {
        if irq < 8 {
            let mut port: Port<u8> = Port::new(0x21);
            let mask = port.read() & !(1 << irq);
            port.write(mask);
        } else {
            let mut port: Port<u8> = Port::new(0xA1);
            let mask = port.read() & !(1 << (irq - 8));
            port.write(mask);
        }
    }
}

/// Read the current interrupt mask register for master (low byte) and
/// slave (high byte) as a single u16.
pub fn read_imr() -> u16 {
    unsafe {
        let lo: u8 = Port::<u8>::new(0x21).read();
        let hi: u8 = Port::<u8>::new(0xA1).read();
        (hi as u16) << 8 | lo as u16
    }
}

// ---------------------------------------------------------------------------
// Internal ISR helpers
// ---------------------------------------------------------------------------

/// Read the master PIC's In-Service Register and return whether bit `bit`
/// (0–7) is set, indicating an in-service (acknowledged) interrupt.
unsafe fn master_isr_bit(bit: u8) -> bool {
    unsafe {
        Port::<u8>::new(0x20).write(0x0Bu8);
        let isr: u8 = Port::<u8>::new(0x20).read();
        isr & (1 << bit) != 0
    }
}

/// Read the slave PIC's In-Service Register bit.
unsafe fn slave_isr_bit(bit: u8) -> bool {
    unsafe {
        Port::<u8>::new(0xA0).write(0x0Bu8);
        let isr: u8 = Port::<u8>::new(0xA0).read();
        isr & (1 << bit) != 0
    }
}

/// Send EOI to master PIC only (used for spurious slave IRQs).
unsafe fn master_eoi_only() {
    unsafe { Port::<u8>::new(0x20).write(0x20u8); }
}
