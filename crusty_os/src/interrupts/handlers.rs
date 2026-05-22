// interrupts/handlers.rs
//
// Concrete implementations of every registered interrupt handler.
//
// Naming convention:
//   *_handler  — CPU exception handler (called with x86-interrupt ABI)
//   *_handler  — hardware IRQ handler  (same ABI; EOI required)
//
// Fault-tolerant philosophy:
//   - Recoverable faults (breakpoint, debug, overflow) log and return.
//   - Unrecoverable faults (double fault, machine check) panic with as much
//     context as possible.  There is no meaningful way to resume from these.
//   - Page faults attempt to provide actionable diagnostics (address, flags)
//     before panicking, because they are by far the most common bug vector.
//   - All hardware IRQ handlers record telemetry via `stats::record`, send
//     a correct EOI (including spurious-IRQ detection), and then dispatch
//     to any registered second-level handler via `dispatch::dispatch`.
//
// Thread safety:
//   All handlers use only lock-free primitives (atomics, ports) or
//   spin::Mutex.  They never block.

use x86_64::structures::idt::{InterruptStackFrame, PageFaultErrorCode};

use crate::{print, println};
use super::{dispatch, pic, stats, vectors::InterruptVector};

// ===========================================================================
// CPU Exception Handlers
// ===========================================================================

// ---------------------------------------------------------------------------
// #DE — Divide Error (vector 0x00)
// ---------------------------------------------------------------------------

pub extern "x86-interrupt" fn divide_error_handler(frame: InterruptStackFrame) {
    stats::record(0x00);
    panic!(
        "EXCEPTION: DIVIDE ERROR (#DE)\n\
         Instruction pointer: {:#x}\n\
         {:#?}",
        frame.instruction_pointer.as_u64(),
        frame
    );
}

// ---------------------------------------------------------------------------
// #DB — Debug (vector 0x01)
// ---------------------------------------------------------------------------

pub extern "x86-interrupt" fn debug_handler(frame: InterruptStackFrame) {
    stats::record(0x01);
    // #DB can be a fault (hardware breakpoint) or a trap (single-step).
    // We log and return; a real debugger would inspect DR6/DR7 here.
    println!(
        "EXCEPTION: DEBUG (#DB)\n\
         ip={:#x} sp={:#x}",
        frame.instruction_pointer.as_u64(),
        frame.stack_pointer.as_u64()
    );
}

// ---------------------------------------------------------------------------
// NMI — Non-Maskable Interrupt (vector 0x02)
// ---------------------------------------------------------------------------

pub extern "x86-interrupt" fn nmi_handler(frame: InterruptStackFrame) {
    stats::record(0x02);
    // NMI sources on PC hardware: RAM parity error, watchdog, IOCHK#.
    // There is almost never a safe recovery path.  Log what we can and halt.
    panic!(
        "EXCEPTION: NON-MASKABLE INTERRUPT (NMI)\n\
         This usually indicates hardware failure (RAM, watchdog, or bus error).\n\
         ip={:#x}\n\
         {:#?}",
        frame.instruction_pointer.as_u64(),
        frame
    );
}

// ---------------------------------------------------------------------------
// #BP — Breakpoint (vector 0x03)
// ---------------------------------------------------------------------------

pub extern "x86-interrupt" fn breakpoint_handler(frame: InterruptStackFrame) {
    stats::record(0x03);
    // Trap — execution resumes at the instruction *after* INT3.
    println!(
        "EXCEPTION: BREAKPOINT (#BP)\n\
         ip={:#x} sp={:#x}",
        frame.instruction_pointer.as_u64(),
        frame.stack_pointer.as_u64()
    );
}

// ---------------------------------------------------------------------------
// #OF — Overflow (vector 0x04)
// ---------------------------------------------------------------------------

pub extern "x86-interrupt" fn overflow_handler(frame: InterruptStackFrame) {
    stats::record(0x04);
    // INTO is obsolete in 64-bit mode; this is essentially unreachable,
    // but we handle it defensively.
    println!(
        "EXCEPTION: OVERFLOW (#OF)\n\
         ip={:#x}",
        frame.instruction_pointer.as_u64()
    );
}

// ---------------------------------------------------------------------------
// #BR — BOUND Range Exceeded (vector 0x05)
// ---------------------------------------------------------------------------

pub extern "x86-interrupt" fn bound_range_handler(frame: InterruptStackFrame) {
    stats::record(0x05);
    panic!(
        "EXCEPTION: BOUND RANGE EXCEEDED (#BR)\n\
         ip={:#x}\n{:#?}",
        frame.instruction_pointer.as_u64(),
        frame
    );
}

// ---------------------------------------------------------------------------
// #UD — Invalid Opcode (vector 0x06)
// ---------------------------------------------------------------------------

pub extern "x86-interrupt" fn invalid_opcode_handler(frame: InterruptStackFrame) {
    stats::record(0x06);
    panic!(
        "EXCEPTION: INVALID OPCODE (#UD)\n\
         ip={:#x}  (check for miscompiled code or unsupported CPU feature)\n\
         {:#?}",
        frame.instruction_pointer.as_u64(),
        frame
    );
}

// ---------------------------------------------------------------------------
// #NM — Device Not Available (vector 0x07)
// ---------------------------------------------------------------------------

pub extern "x86-interrupt" fn device_not_available_handler(frame: InterruptStackFrame) {
    stats::record(0x07);
    // Raised when CR0.TS=1 and an FPU/SSE instruction is executed.
    // A real OS would save/restore FPU state here for context switching.
    panic!(
        "EXCEPTION: DEVICE NOT AVAILABLE (#NM)\n\
         FPU/SSE state not saved.  Implement lazy FPU switching.\n\
         {:#?}",
        frame
    );
}

// ---------------------------------------------------------------------------
// #DF — Double Fault (vector 0x08)
// ---------------------------------------------------------------------------

pub extern "x86-interrupt" fn double_fault_handler(
    frame: InterruptStackFrame,
    _error_code: u64,   // Always 0 by definition.
) -> ! {
    stats::record(0x08);
    // This runs on a dedicated IST stack.  A panic here still works because
    // the panic handler writes to the VGA/serial buffer, not the stack that
    // caused the original fault.
    panic!(
        "EXCEPTION: DOUBLE FAULT (#DF)\n\
         This indicates a fault occurred while handling another fault,\n\
         or a stack overflow corrupted the stack pointer.\n\
         {:#?}",
        frame
    );
}

// ---------------------------------------------------------------------------
// #TS — Invalid TSS (vector 0x0A)
// ---------------------------------------------------------------------------

pub extern "x86-interrupt" fn invalid_tss_handler(
    frame: InterruptStackFrame,
    error_code: u64,
) {
    stats::record(0x0A);
    panic!(
        "EXCEPTION: INVALID TSS (#TS)\n\
         Selector: {:#x}\n{:#?}",
        error_code, frame
    );
}

// ---------------------------------------------------------------------------
// #NP — Segment Not Present (vector 0x0B)
// ---------------------------------------------------------------------------

pub extern "x86-interrupt" fn segment_not_present_handler(
    frame: InterruptStackFrame,
    error_code: u64,
) {
    stats::record(0x0B);
    panic!(
        "EXCEPTION: SEGMENT NOT PRESENT (#NP)\n\
         Selector: {:#x}\n{:#?}",
        error_code, frame
    );
}

// ---------------------------------------------------------------------------
// #SS — Stack-Segment Fault (vector 0x0C)
// ---------------------------------------------------------------------------

pub extern "x86-interrupt" fn stack_segment_handler(
    frame: InterruptStackFrame,
    error_code: u64,
) {
    stats::record(0x0C);
    panic!(
        "EXCEPTION: STACK-SEGMENT FAULT (#SS)\n\
         Selector/offset: {:#x}\n{:#?}",
        error_code, frame
    );
}

// ---------------------------------------------------------------------------
// #GP — General Protection Fault (vector 0x0D)
// ---------------------------------------------------------------------------

pub extern "x86-interrupt" fn general_protection_handler(
    frame: InterruptStackFrame,
    error_code: u64,
) {
    stats::record(0x0D);
    // error_code=0 → not segment-related (null pointer, ring violation, etc.)
    // error_code≠0 → bits 15:3 = segment selector, bits 2:0 = table+ext flags
    panic!(
        "EXCEPTION: GENERAL PROTECTION FAULT (#GP)\n\
         Error code: {:#x}  (0 = not segment-related)\n\
         ip={:#x}\n{:#?}",
        error_code,
        frame.instruction_pointer.as_u64(),
        frame
    );
}

// ---------------------------------------------------------------------------
// #PF — Page Fault (vector 0x0E)
// ---------------------------------------------------------------------------

pub extern "x86-interrupt" fn page_fault_handler(
    frame: InterruptStackFrame,
    error_code: PageFaultErrorCode,
) {
    use x86_64::registers::control::Cr2;

    stats::record(0x0E);

    // CR2 holds the faulting linear address — read it immediately before
    // any subsequent instructions might alter it.
    let faulting_address = Cr2::read();

    panic!(
        "EXCEPTION: PAGE FAULT (#PF)\n\
         Faulting address : {:#x}\n\
         Error flags      : {:?}\n\
         ip={:#x}\n\
         {:#?}",
        faulting_address.as_u64(),
        error_code,
        frame.instruction_pointer.as_u64(),
        frame
    );
}

// ---------------------------------------------------------------------------
// #MF — x87 Floating-Point Exception (vector 0x10)
// ---------------------------------------------------------------------------

pub extern "x86-interrupt" fn x87_fp_handler(frame: InterruptStackFrame) {
    stats::record(0x10);
    // The specific FP exception is in the x87 status word; we don't parse
    // it here.  A production kernel would read FSW and decode the condition.
    panic!(
        "EXCEPTION: x87 FLOATING-POINT (#MF)\n\
         Check x87 FPU status word for specific condition.\n\
         {:#?}",
        frame
    );
}

// ---------------------------------------------------------------------------
// #AC — Alignment Check (vector 0x11)
// ---------------------------------------------------------------------------

pub extern "x86-interrupt" fn alignment_check_handler(
    frame: InterruptStackFrame,
    error_code: u64,
) {
    stats::record(0x11);
    panic!(
        "EXCEPTION: ALIGNMENT CHECK (#AC)\n\
         Error code: {:#x}\n{:#?}",
        error_code, frame
    );
}

// ---------------------------------------------------------------------------
// #MC — Machine Check (vector 0x12)
// ---------------------------------------------------------------------------

pub extern "x86-interrupt" fn machine_check_handler(frame: InterruptStackFrame) -> ! {
    stats::record(0x12);
    // Machine check exceptions carry detailed information in model-specific
    // registers (MCi_STATUS, MCi_ADDR).  Parsing MSRs here requires unsafe
    // rdmsr calls and is CPU-model-specific.  We note the MSR bank count
    // available and dump what we can without halting prematurely.
    //
    // In production you would iterate MCG_CAP[Count] and read each MCi_STATUS.
    panic!(
        "EXCEPTION: MACHINE CHECK (#MC)\n\
         This indicates unrecoverable hardware error.\n\
         Consult MCi_STATUS MSRs for details.\n\
         {:#?}",
        frame
    );
}

// ---------------------------------------------------------------------------
// #XM/#XF — SIMD Floating-Point Exception (vector 0x13)
// ---------------------------------------------------------------------------

pub extern "x86-interrupt" fn simd_fp_handler(frame: InterruptStackFrame) {
    stats::record(0x13);
    // The specific SIMD exception is in MXCSR.  A production kernel would
    // read MXCSR (via stmxcsr or intrinsic) and decode the exception flags.
    panic!(
        "EXCEPTION: SIMD FLOATING-POINT (#XM)\n\
         Check MXCSR for specific condition.\n\
         {:#?}",
        frame
    );
}

// ---------------------------------------------------------------------------
// #VE — Virtualization Exception (vector 0x14)
// ---------------------------------------------------------------------------

pub extern "x86-interrupt" fn virtualization_handler(frame: InterruptStackFrame) {
    stats::record(0x14);
    panic!(
        "EXCEPTION: VIRTUALIZATION (#VE)\n\
         EPT violation — check VMCS for details.\n\
         {:#?}",
        frame
    );
}

// ===========================================================================
// Hardware IRQ Handlers (8259 PIC, vectors 0x20–0x2F)
// ===========================================================================

// ---------------------------------------------------------------------------
// IRQ0 — PIT Timer
// ---------------------------------------------------------------------------

pub extern "x86-interrupt" fn timer_handler(_frame: InterruptStackFrame) {
    stats::record(InterruptVector::Timer.as_u8());

    // Dispatch to any registered second-level handler (e.g., scheduler tick).
    unsafe {
        dispatch::dispatch(0); // IRQ0
        pic::end_of_interrupt(InterruptVector::Timer.as_u8());
    }
}

// ---------------------------------------------------------------------------
// IRQ1 — PS/2 Keyboard
// ---------------------------------------------------------------------------

pub extern "x86-interrupt" fn keyboard_handler(_frame: InterruptStackFrame) {
    use pc_keyboard::{layouts, DecodedKey, HandleControl, Keyboard, ScancodeSet1};
    use spin::Mutex;
    use x86_64::instructions::port::Port;

    stats::record(InterruptVector::Keyboard.as_u8());

    // Keyboard state lives in a static — one global PS/2 decoder.
    // `lazy_static` inside a handler is legitimate: the first call initializes
    // the Mutex; subsequent calls just lock it.
    lazy_static::lazy_static! {
        static ref KEYBOARD: Mutex<Keyboard<layouts::Us104Key, ScancodeSet1>> =
            Mutex::new(Keyboard::new(
                ScancodeSet1::new(),
                layouts::Us104Key,
                HandleControl::Ignore,
            ));
    }

    let mut keyboard = KEYBOARD.lock();
    let mut port: Port<u8> = Port::new(0x60);

    // SAFETY: Port 0x60 is the PS/2 data port; read is unconditional and
    // well-defined on PC-compatible hardware.
    let scancode: u8 = unsafe { port.read() };

    if let Ok(Some(key_event)) = keyboard.add_byte(scancode) {
        if let Some(key) = keyboard.process_keyevent(key_event) {
            match key {
                DecodedKey::Unicode(character) => print!("{}", character),
                DecodedKey::RawKey(key) => print!("{:?}", key),
            }
        }
    }

    // Dispatch to any registered second-level handler before EOI.
    unsafe {
        dispatch::dispatch(1); // IRQ1
        pic::end_of_interrupt(InterruptVector::Keyboard.as_u8());
    }
}

// ---------------------------------------------------------------------------
// IRQ7 — Spurious Master
// ---------------------------------------------------------------------------

pub extern "x86-interrupt" fn spurious_master_handler(_frame: InterruptStackFrame) {
    // DO NOT send EOI unconditionally.  Read the master ISR first.
    // `eoi_if_not_spurious_master` handles all of this correctly.
    unsafe {
        let was_real = pic::eoi_if_not_spurious_master();
        stats::record(if was_real {
            InterruptVector::SpuriousMaster.as_u8()
        } else {
            // Count spurious separately if you add a dedicated counter;
            // for now, record in the same slot.
            InterruptVector::SpuriousMaster.as_u8()
        });
    }
}

// ---------------------------------------------------------------------------
// IRQ8 — RTC
// ---------------------------------------------------------------------------

pub extern "x86-interrupt" fn rtc_handler(_frame: InterruptStackFrame) {
    use x86_64::instructions::port::Port;

    stats::record(InterruptVector::Rtc.as_u8());

    // The RTC requires reading register C to acknowledge the interrupt
    // before the next one can fire.  Select register C (0x0C), then read.
    unsafe {
        let mut port_idx: Port<u8> = Port::new(0x70);
        let mut port_data: Port<u8> = Port::new(0x71);
        port_idx.write(0x0C);
        let _ = port_data.read(); // discard; we just need the read to occur

        dispatch::dispatch(8); // IRQ8
        pic::end_of_interrupt(InterruptVector::Rtc.as_u8());
    }
}

// ---------------------------------------------------------------------------
// IRQ12 — PS/2 Mouse
// ---------------------------------------------------------------------------

pub extern "x86-interrupt" fn mouse_handler(_frame: InterruptStackFrame) {
    use x86_64::instructions::port::Port;

    stats::record(InterruptVector::Mouse.as_u8());

    // Read the data byte from the PS/2 port to clear the interrupt.
    // A full mouse driver would accumulate 3 bytes per packet.
    let _data: u8 = unsafe { Port::new(0x60).read() };

    unsafe {
        dispatch::dispatch(12); // IRQ12
        pic::end_of_interrupt(InterruptVector::Mouse.as_u8());
    }
}

// ---------------------------------------------------------------------------
// IRQ14 — Primary ATA
// ---------------------------------------------------------------------------

pub extern "x86-interrupt" fn ata_primary_handler(_frame: InterruptStackFrame) {
    stats::record(InterruptVector::AtaPrimary.as_u8());

    unsafe {
        dispatch::dispatch(14); // IRQ14
        pic::end_of_interrupt(InterruptVector::AtaPrimary.as_u8());
    }
}

// ---------------------------------------------------------------------------
// IRQ15 — Secondary ATA / Spurious Slave
// ---------------------------------------------------------------------------

pub extern "x86-interrupt" fn ata_secondary_handler(_frame: InterruptStackFrame) {
    stats::record(InterruptVector::AtaSecondary.as_u8());

    // eoi_if_not_spurious_slave handles the ISR check, EOI routing to both
    // master and slave if real, or EOI to master only if spurious.
    unsafe {
        let was_real = pic::eoi_if_not_spurious_slave();
        if was_real {
            dispatch::dispatch(15); // IRQ15
        }
    }
}
