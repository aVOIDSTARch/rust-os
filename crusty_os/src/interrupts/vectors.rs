// interrupts/vectors.rs
//
// Canonical interrupt vector number definitions.
//
// The x86_64 interrupt vector space is 256 entries (0–255):
//   0x00–0x1F : CPU exceptions (Intel-defined, fixed)
//   0x20–0x2F : Remapped 8259 PIC IRQs  (IRQ0–IRQ15 after offset 0x20)
//   0x30–0xFF : Available for software/APIC use
//
// All values here must remain consistent with pic::PIC_1_OFFSET and
// pic::PIC_2_OFFSET.  If those offsets change, update PIC_IRQ_* below.

// ---------------------------------------------------------------------------
// CPU Exception Vectors (Intel SDM Vol. 3A, Table 6-1)
// ---------------------------------------------------------------------------

/// Divide-by-zero error (#DE). Fault; no error code.
pub const EXC_DIVIDE_ERROR: u8         = 0x00;
/// Debug exception (#DB). Fault/trap; no error code.
pub const EXC_DEBUG: u8                = 0x01;
/// Non-maskable interrupt (NMI). Not technically an exception; no error code.
pub const EXC_NMI: u8                  = 0x02;
/// Breakpoint (#BP). Trap; no error code.
pub const EXC_BREAKPOINT: u8           = 0x03;
/// Overflow (#OF). Trap; no error code.
pub const EXC_OVERFLOW: u8             = 0x04;
/// BOUND range exceeded (#BR). Fault; no error code.
pub const EXC_BOUND_RANGE: u8          = 0x05;
/// Invalid opcode (#UD). Fault; no error code.
pub const EXC_INVALID_OPCODE: u8       = 0x06;
/// Device not available / no math coprocessor (#NM). Fault; no error code.
pub const EXC_DEVICE_NOT_AVAILABLE: u8 = 0x07;
/// Double fault (#DF). Abort; error code always 0.
pub const EXC_DOUBLE_FAULT: u8         = 0x08;
/// Invalid TSS (#TS). Fault; error code = segment selector.
pub const EXC_INVALID_TSS: u8          = 0x0A;
/// Segment not present (#NP). Fault; error code = segment selector.
pub const EXC_SEGMENT_NOT_PRESENT: u8  = 0x0B;
/// Stack-segment fault (#SS). Fault; error code = segment selector or 0.
pub const EXC_STACK_SEGMENT: u8        = 0x0C;
/// General protection fault (#GP). Fault; error code = segment selector or 0.
pub const EXC_GENERAL_PROTECTION: u8   = 0x0D;
/// Page fault (#PF). Fault; error code = page-fault error flags.
pub const EXC_PAGE_FAULT: u8           = 0x0E;
/// x87 floating-point exception (#MF). Fault; no error code.
pub const EXC_X87_FP: u8               = 0x10;
/// Alignment check (#AC). Fault; error code always 0.
pub const EXC_ALIGNMENT_CHECK: u8      = 0x11;
/// Machine check (#MC). Abort; no error code (model-specific MSRs carry info).
pub const EXC_MACHINE_CHECK: u8        = 0x12;
/// SIMD floating-point exception (#XM/#XF). Fault; no error code.
pub const EXC_SIMD_FP: u8             = 0x13;
/// Virtualization exception (#VE). Fault; no error code.
pub const EXC_VIRTUALIZATION: u8       = 0x14;
/// Control protection exception (#CP). Fault; error code.
pub const EXC_CONTROL_PROTECTION: u8   = 0x15;
/// Hypervisor injection exception (#HV). (AMD SVM only.) Fault; no error code.
pub const EXC_HYPERVISOR_INJECTION: u8 = 0x1C;
/// VMM communication exception (#VC). (AMD SVM only.) Fault; error code.
pub const EXC_VMM_COMM: u8             = 0x1D;
/// Security exception (#SX). Fault; error code.
pub const EXC_SECURITY: u8             = 0x1E;

// ---------------------------------------------------------------------------
// 8259 PIC IRQ Vectors (remapped to 0x20–0x2F)
// ---------------------------------------------------------------------------

/// Base offset for PIC master (IRQ 0–7).
pub const PIC_1_OFFSET: u8 = 0x20;
/// Base offset for PIC slave  (IRQ 8–15).
pub const PIC_2_OFFSET: u8 = PIC_1_OFFSET + 8;

/// IRQ0  — Programmable Interval Timer (PIT).
pub const PIC_IRQ_TIMER: u8      = PIC_1_OFFSET;
/// IRQ1  — PS/2 keyboard.
pub const PIC_IRQ_KEYBOARD: u8   = PIC_1_OFFSET + 1;
/// IRQ2  — Cascade from slave PIC (not real hardware).
pub const PIC_IRQ_CASCADE: u8    = PIC_1_OFFSET + 2;
/// IRQ3  — COM2 serial port.
pub const PIC_IRQ_COM2: u8       = PIC_1_OFFSET + 3;
/// IRQ4  — COM1 serial port.
pub const PIC_IRQ_COM1: u8       = PIC_1_OFFSET + 4;
/// IRQ5  — LPT2 / sound card.
pub const PIC_IRQ_LPT2: u8       = PIC_1_OFFSET + 5;
/// IRQ6  — Floppy disk controller.
pub const PIC_IRQ_FLOPPY: u8     = PIC_1_OFFSET + 6;
/// IRQ7  — LPT1 / spurious (master). **Must always be handled.**
pub const PIC_IRQ_LPT1: u8       = PIC_1_OFFSET + 7;
/// IRQ8  — Real-time clock (RTC).
pub const PIC_IRQ_RTC: u8        = PIC_2_OFFSET;
/// IRQ9  — ACPI / free.
pub const PIC_IRQ_ACPI: u8       = PIC_2_OFFSET + 1;
/// IRQ10 — Free / PCI.
pub const PIC_IRQ_FREE1: u8      = PIC_2_OFFSET + 2;
/// IRQ11 — Free / PCI.
pub const PIC_IRQ_FREE2: u8      = PIC_2_OFFSET + 3;
/// IRQ12 — PS/2 mouse.
pub const PIC_IRQ_MOUSE: u8      = PIC_2_OFFSET + 4;
/// IRQ13 — FPU / coprocessor.
pub const PIC_IRQ_FPU: u8        = PIC_2_OFFSET + 5;
/// IRQ14 — Primary ATA (IDE) channel.
pub const PIC_IRQ_ATA_PRIMARY: u8   = PIC_2_OFFSET + 6;
/// IRQ15 — Secondary ATA (IDE) channel / spurious (slave). **Must always be handled.**
pub const PIC_IRQ_ATA_SECONDARY: u8 = PIC_2_OFFSET + 7;

// ---------------------------------------------------------------------------
// Typed enum for use with the IDT indexing API
// ---------------------------------------------------------------------------

/// A typed wrapper over the u8 vector index.  Add variants as you register
/// new hardware IRQ handlers; exception vectors are handled directly by the
/// typed IDT fields in `x86_64::structures::idt`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum InterruptVector {
    Timer           = PIC_IRQ_TIMER,
    Keyboard        = PIC_IRQ_KEYBOARD,
    SpuriousMaster  = PIC_IRQ_LPT1,        // IRQ7
    Rtc             = PIC_IRQ_RTC,
    Mouse           = PIC_IRQ_MOUSE,
    AtaPrimary      = PIC_IRQ_ATA_PRIMARY,
    // IRQ15: secondary ATA channel or spurious slave — distinguish via ISR.
    AtaSecondary    = PIC_IRQ_ATA_SECONDARY,
}

impl InterruptVector {
    #[inline]
    pub fn as_u8(self) -> u8 {
        self as u8
    }

    #[inline]
    pub fn as_usize(self) -> usize {
        self.as_u8() as usize
    }

    /// True if this vector corresponds to a spurious IRQ that must NOT
    /// receive an EOI from the slave PIC.
    #[inline]
    /// True if this is the spurious master (IRQ7).
    /// For IRQ15 (`AtaSecondary`), check the slave ISR to determine spuriousness.
    pub fn is_spurious_master(self) -> bool {
        matches!(self, Self::SpuriousMaster)
    }
}
