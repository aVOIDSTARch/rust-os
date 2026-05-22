// interrupts/apic.rs
//
// Local APIC (Advanced Programmable Interrupt Controller) stub.
//
// The 8259 PIC handled by `pic.rs` is a legacy single-core device.
// Modern systems use the local APIC + I/O APIC for:
//   - Per-core timer interrupts (replaces the PIT-driven IRQ0)
//   - SMP inter-processor interrupts (IPIs)
//   - Higher-precision interrupt delivery
//   - MSI/MSI-X support for PCIe
//
// This module provides:
//   1. CPUID detection of APIC capability.
//   2. A `disable_pic()` function to mask the 8259 before enabling the APIC.
//   3. A skeleton `LocalApic` structure with MMIO accessors.
//
// To activate APIC support:
//   a. Parse the MADT from ACPI tables to locate the local APIC base address
//      and I/O APIC address.
//   b. Call `disable_pic()` to silence the 8259.
//   c. Map the APIC MMIO region into virtual memory.
//   d. Call `LocalApic::init()` on each core.
//   e. Update the IDT with APIC-routed vectors (currently stubbed in
//      `exceptions.rs` as PIC IRQ slots).
//
// This module does NOT currently enable the APIC.  It is structured so
// that adding the ACPI/MADT parser and MMIO mapper unblocks the full path
// without requiring changes to the rest of the interrupt subsystem.

use x86_64::instructions::port::Port;

// ---------------------------------------------------------------------------
// CPUID detection
// ---------------------------------------------------------------------------

/// Returns `true` if the CPU reports an on-chip local APIC via CPUID.
/// Does not indicate whether the APIC has been enabled in MSR 0x1B.
pub fn apic_supported() -> bool {
    // CPUID leaf 1, EDX bit 9 = APIC on-chip.
    // The `raw_cpuid` crate or inline asm can be used; we use a minimal
    // inline asm approach to keep the dependency list flat.
    let edx: u32;
    unsafe {
        core::arch::asm!(
            "mov eax, 1",
            "cpuid",
            out("edx") edx,
            // Clobber eax, ebx, ecx — CPUID modifies all four.
            lateout("eax") _,
            lateout("ebx") _,
            lateout("ecx") _,
        );
    }
    edx & (1 << 9) != 0
}

// ---------------------------------------------------------------------------
// Disable the 8259 PIC
// ---------------------------------------------------------------------------

/// Mask all 8259 IRQ lines by writing 0xFF to both IMR ports.
///
/// Call this **before** enabling the local APIC to prevent spurious
/// interrupts from the 8259 racing with APIC initialization.
///
/// After this, the PIC will not deliver any interrupts, but its vectors
/// (0x20–0x2F) remain in the IDT so that any residual 8259 spurious
/// interrupt is caught and discarded rather than triggering a #GP on an
/// unhandled vector.
pub fn disable_pic() {
    unsafe {
        // Master IMR
        Port::<u8>::new(0x21).write(0xFF);
        // Slave IMR
        Port::<u8>::new(0xA1).write(0xFF);
    }
}

// ---------------------------------------------------------------------------
// Local APIC MMIO interface (skeleton)
// ---------------------------------------------------------------------------

/// Default physical base address of the local APIC MMIO region.
/// May be overridden by the MADT's Local APIC Address field.
pub const LOCAL_APIC_DEFAULT_BASE: u64 = 0xFEE0_0000;

// APIC register offsets (from base).
const APIC_ID:           u32 = 0x020;
const APIC_VERSION:      u32 = 0x030;
const APIC_TPR:          u32 = 0x080; // Task Priority Register
const APIC_EOI:          u32 = 0x0B0; // End-of-Interrupt (write-only)
const APIC_SPURIOUS:     u32 = 0x0F0; // Spurious Interrupt Vector Register
const APIC_LVT_TIMER:    u32 = 0x320;
const APIC_TIMER_INIT:   u32 = 0x380;
const APIC_TIMER_CURRENT: u32 = 0x390;
const APIC_TIMER_DIVIDE: u32 = 0x3E0;
const APIC_ICR_LO:       u32 = 0x300; // Interrupt Command Register (low)
const APIC_ICR_HI:       u32 = 0x310; // Interrupt Command Register (high)

/// A handle to the local APIC MMIO region on a single CPU core.
///
/// `base` is the virtual address of the APIC's MMIO region.
/// On each core, create one of these after mapping the physical
/// `LOCAL_APIC_DEFAULT_BASE` (or MADT-supplied address) into virtual memory.
pub struct LocalApic {
    base: *mut u32,
}

// SAFETY: The local APIC registers are core-local; there is no shared
// mutable state between cores through this pointer.  Callers must ensure
// only one `LocalApic` instance exists per core.
unsafe impl Send for LocalApic {}
unsafe impl Sync for LocalApic {}

impl LocalApic {
    /// Construct a `LocalApic` from a mapped virtual base address.
    ///
    /// # Safety
    /// `virtual_base` must be the virtual address of a valid, mapped local
    /// APIC MMIO region.  Passing an incorrect address causes UB.
    pub unsafe fn new(virtual_base: u64) -> Self {
        Self { base: virtual_base as *mut u32 }
    }

    // SAFETY: All read/write helpers access a memory-mapped I/O region.
    // Volatile accesses prevent the compiler from reordering or eliding them.

    unsafe fn read(&self, offset: u32) -> u32 {
        core::ptr::read_volatile(self.base.add((offset / 4) as usize))
    }

    unsafe fn write(&self, offset: u32, value: u32) {
        core::ptr::write_volatile(self.base.add((offset / 4) as usize), value);
    }

    /// Enable the local APIC and set the spurious interrupt vector.
    ///
    /// The spurious vector receives interrupts the CPU "almost" acknowledged;
    /// it must be handled but must NOT send an EOI.  Use a dedicated high
    /// vector (e.g., 0xFF) and leave its IDT entry as a no-op.
    pub unsafe fn init(&self, spurious_vector: u8) {
        // SVR bit 8 = APIC software enable; bits 7:0 = spurious vector.
        // The spurious vector must have bits 3:0 = 0b1111 (i.e., be aligned
        // to 16) per the Intel SDM.
        let svr = (1u32 << 8) | (spurious_vector as u32);
        self.write(APIC_SPURIOUS, svr);
    }

    /// Send an End-of-Interrupt signal.
    ///
    /// Must be called at the end of every local APIC interrupt handler
    /// (except spurious vectors, which must NOT receive an EOI).
    #[inline]
    pub unsafe fn end_of_interrupt(&self) {
        self.write(APIC_EOI, 0);
    }

    /// Return the local APIC ID for this core.
    pub unsafe fn id(&self) -> u8 {
        ((self.read(APIC_ID) >> 24) & 0xFF) as u8
    }

    /// Return the local APIC version register value.
    pub unsafe fn version(&self) -> u32 {
        self.read(APIC_VERSION)
    }

    /// Configure the APIC timer in one-shot or periodic mode.
    ///
    /// `vector`  — IDT vector to fire on timer expiry.
    /// `initial` — Initial count value (decrements at bus_clock / divide_by).
    /// `divide`  — Divide configuration register value (e.g., 0x3 = divide by 16).
    /// `periodic`— `true` for periodic, `false` for one-shot.
    pub unsafe fn configure_timer(
        &self,
        vector: u8,
        initial: u32,
        divide: u32,
        periodic: bool,
    ) {
        // LVT Timer: bit 17 = periodic mode, bits 7:0 = vector.
        let lvt = if periodic { (1 << 17) | vector as u32 } else { vector as u32 };
        self.write(APIC_TIMER_DIVIDE, divide);
        self.write(APIC_LVT_TIMER, lvt);
        self.write(APIC_TIMER_INIT, initial);
    }

    /// Send an Inter-Processor Interrupt (IPI) to a specific APIC ID.
    ///
    /// `dest`   — target local APIC ID.
    /// `vector` — IDT vector to deliver.
    pub unsafe fn send_ipi(&self, dest: u8, vector: u8) {
        // Write destination first (high word), then command (low word).
        // Writing ICR_LO triggers the IPI.
        self.write(APIC_ICR_HI, (dest as u32) << 24);
        // Fixed delivery mode (bits 10:8 = 000), edge-triggered, assert.
        self.write(APIC_ICR_LO, vector as u32 | (1 << 14));
    }
}
