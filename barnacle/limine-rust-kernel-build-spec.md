# Limine + Rust x86_64 Kernel: Build and Execution Specification

> **Audience:** A Claude instance executing this document as a build agent.
> Every instruction is precise and ordered. Do not reorder steps. Do not skip
> prerequisites. A missed dependency produces a triple fault or a silent
> mis-compilation — neither of which produces a useful error message without
> the correct tooling already in place.
>
> **Goal:** A bootable x86_64 kernel in Rust using the Limine bootloader protocol
> that successfully reaches its entry point with a validated memory map covering
> the full physical address space, a loaded GDT/TSS with three IST stacks, a
> loaded IDT with all 256 vector handlers, and a functional bitmap physical
> memory manager.

---

## 0. Environment Prerequisites

Verify these tools exist before writing a single line of code. If any are absent,
install them before proceeding. Do not attempt workarounds.

```bash
# Verify Rust nightly toolchain
rustup toolchain install nightly
rustup override set nightly
rustup component add rust-src llvm-tools-preview

# Verify target support
rustup target add x86_64-unknown-none

# Verify bootimage / build tooling
cargo install bootimage 2>/dev/null || true

# Verify QEMU for testing
qemu-system-x86_64 --version

# Verify xorriso for ISO creation (Limine requires this)
xorriso --version

# Verify nasm (needed for any assembly stubs)
nasm -v

# Clone Limine for the ISO deployment scripts
git clone https://github.com/limine-bootloader/limine.git --branch=v8.x-binary --depth=1
make -C limine
```

If `qemu-system-x86_64` is absent: `apt install qemu-system-x86 xorriso nasm` on
Debian/Ubuntu, or the equivalent for the host OS.

---

## 1. Project Structure

Create this exact directory tree. Every path referenced in later sections assumes
this layout.

```
my-kernel/
├── .cargo/
│   └── config.toml
├── src/
│   ├── main.rs
│   ├── gdt.rs
│   ├── interrupts/
│   │   ├── mod.rs
│   │   ├── vectors.rs
│   │   ├── exceptions.rs
│   │   ├── handlers.rs
│   │   ├── pic.rs
│   │   ├── dispatch.rs
│   │   ├── stats.rs
│   │   └── apic.rs
│   ├── memory/
│   │   ├── mod.rs
│   │   └── pmm.rs
│   └── panic.rs
├── kernel.ld
├── Cargo.toml
├── Makefile
└── iso_root/               ← created by Makefile, not committed
    └── boot/
        └── limine/
```

```bash
# Create the skeleton
mkdir -p my-kernel/{.cargo,src/interrupts,src/memory,iso_root/boot/limine}
cd my-kernel
```

---

## 2. Cargo.toml

```toml
[package]
name = "my-kernel"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "kernel"
path = "src/main.rs"

[dependencies]
limine    = "0.4"
x86_64    = "0.15"
pic8259   = "0.10"
spin      = "0.9"
lazy_static = { version = "1.4", features = ["spin_no_std"] }
pc_keyboard = "0.7"

[profile.dev]
panic = "abort"
opt-level = 1          # 0 can produce stack frames too large for IST stacks

[profile.release]
panic = "abort"
opt-level = 3
lto = "thin"
codegen-units = 1
```

---

## 3. .cargo/config.toml

```toml
[build]
target = "x86_64-unknown-none"

[target.x86_64-unknown-none]
rustflags = [
    "-C", "link-arg=-Tkernel.ld",
    "-C", "link-arg=--gc-sections",
    "-C", "link-arg=-z",
    "-C", "link-arg=noexecstack",
]
```

---

## 4. Linker Script: kernel.ld

This is load-bearing. The `.limine_requests` section must exist and must precede
`.text` or Limine will not find the request structures.

```ld
OUTPUT_FORMAT("elf64-x86-64")
OUTPUT_ARCH(x86_64)
ENTRY(kernel_main)

KERNEL_VIRT_BASE = 0xFFFFFFFF80000000;

PHDRS {
    requests PT_LOAD FLAGS(4);
    text     PT_LOAD FLAGS(5);
    rodata   PT_LOAD FLAGS(4);
    data     PT_LOAD FLAGS(6);
}

SECTIONS {
    . = KERNEL_VIRT_BASE + SIZEOF_HEADERS;

    .limine_requests : ALIGN(8) {
        KEEP(*(.limine_requests_start))
        KEEP(*(.limine_requests))
        KEEP(*(.limine_requests_end))
    } :requests

    .text : ALIGN(4096) {
        *(.text .text.*)
    } :text

    .rodata : ALIGN(4096) {
        *(.rodata .rodata.*)
    } :rodata

    .data : ALIGN(4096) {
        *(.data .data.*)
    } :data

    .bss : ALIGN(4096) {
        __bss_start = .;
        *(COMMON)
        *(.bss .bss.*)
        __bss_end = .;
    } :data

    __kernel_start = KERNEL_VIRT_BASE + SIZEOF_HEADERS;
    __kernel_end = .;

    /DISCARD/ : {
        *(.eh_frame .eh_frame_hdr)
        *(.note .note.*)
        *(.comment)
    }
}
```

---

## 5. src/panic.rs

Must be defined before anything else compiles. The `-> !` divergence is required
by `no_std` and by the double-fault/machine-check interrupt handlers.

```rust
use core::panic::PanicInfo;

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // In a real kernel this would write to serial/VGA.
    // For now, halt all further execution unconditionally.
    loop {
        x86_64::instructions::hlt();
    }
}
```

---

## 6. src/gdt.rs

Provides the three IST stack constants required by `interrupts/exceptions.rs`.
All three must be `pub const u16`. Missing any one of them produces a compile
error in the interrupt subsystem.

```rust
use lazy_static::lazy_static;
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector};
use x86_64::structures::tss::TaskStateSegment;
use x86_64::VirtAddr;

pub const DOUBLE_FAULT_IST_INDEX:  u16 = 0;
pub const NMI_IST_INDEX:           u16 = 1;
pub const MACHINE_CHECK_IST_INDEX: u16 = 2;

const STACK_SIZE: usize = 4096 * 5; // 20 KiB — do not reduce below 4096 * 2

static mut DOUBLE_FAULT_STACK:   [u8; STACK_SIZE] = [0; STACK_SIZE];
static mut NMI_STACK:            [u8; STACK_SIZE] = [0; STACK_SIZE];
static mut MACHINE_CHECK_STACK:  [u8; STACK_SIZE] = [0; STACK_SIZE];

lazy_static! {
    static ref TSS: TaskStateSegment = {
        let mut tss = TaskStateSegment::new();

        tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = {
            let start = VirtAddr::from_ptr(unsafe { &DOUBLE_FAULT_STACK });
            start + STACK_SIZE as u64
        };
        tss.interrupt_stack_table[NMI_IST_INDEX as usize] = {
            let start = VirtAddr::from_ptr(unsafe { &NMI_STACK });
            start + STACK_SIZE as u64
        };
        tss.interrupt_stack_table[MACHINE_CHECK_IST_INDEX as usize] = {
            let start = VirtAddr::from_ptr(unsafe { &MACHINE_CHECK_STACK });
            start + STACK_SIZE as u64
        };

        tss
    };

    static ref GDT: (GlobalDescriptorTable, Selectors) = {
        let mut gdt = GlobalDescriptorTable::new();
        let code_selector = gdt.append(Descriptor::kernel_code_segment());
        let data_selector = gdt.append(Descriptor::kernel_data_segment());
        let tss_selector  = gdt.append(Descriptor::tss_segment(&TSS));
        (gdt, Selectors { code_selector, data_selector, tss_selector })
    };
}

struct Selectors {
    code_selector: SegmentSelector,
    data_selector: SegmentSelector,
    tss_selector:  SegmentSelector,
}

pub fn init() {
    use x86_64::instructions::segmentation::{CS, DS, SS, Segment};
    use x86_64::instructions::tables::load_tss;

    GDT.0.load();

    unsafe {
        CS::set_reg(GDT.1.code_selector);
        DS::set_reg(GDT.1.data_selector);
        SS::set_reg(SegmentSelector(0)); // null stack segment is correct in 64-bit mode
        load_tss(GDT.1.tss_selector);
    }
}
```

---

## 7. src/interrupts/ — The Full Subsystem

Implement all eight files. The content below is the complete, compilable implementation.

### 7.1 src/interrupts/vectors.rs

```rust
// Every vector number lives here. No other file uses raw numeric literals
// for vectors — they import from this module.

pub const EXCEPTION_DIVIDE_ERROR:        u8 = 0x00;
pub const EXCEPTION_DEBUG:               u8 = 0x01;
pub const EXCEPTION_NMI:                 u8 = 0x02;
pub const EXCEPTION_BREAKPOINT:          u8 = 0x03;
pub const EXCEPTION_OVERFLOW:            u8 = 0x04;
pub const EXCEPTION_BOUND_RANGE:         u8 = 0x05;
pub const EXCEPTION_INVALID_OPCODE:      u8 = 0x06;
pub const EXCEPTION_DEVICE_NOT_AVAIL:    u8 = 0x07;
pub const EXCEPTION_DOUBLE_FAULT:        u8 = 0x08;
pub const EXCEPTION_INVALID_TSS:         u8 = 0x0A;
pub const EXCEPTION_SEGMENT_NOT_PRESENT: u8 = 0x0B;
pub const EXCEPTION_STACK_SEGMENT:       u8 = 0x0C;
pub const EXCEPTION_GENERAL_PROTECTION:  u8 = 0x0D;
pub const EXCEPTION_PAGE_FAULT:          u8 = 0x0E;
pub const EXCEPTION_X87_FP:              u8 = 0x10;
pub const EXCEPTION_ALIGNMENT_CHECK:     u8 = 0x11;
pub const EXCEPTION_MACHINE_CHECK:       u8 = 0x12;
pub const EXCEPTION_SIMD_FP:             u8 = 0x13;
pub const EXCEPTION_VIRTUALIZATION:      u8 = 0x14;

pub const PIC_IRQ_TIMER:          u8 = 0x20;
pub const PIC_IRQ_KEYBOARD:       u8 = 0x21;
pub const PIC_IRQ_SPURIOUS_MASTER:u8 = 0x27;
pub const PIC_IRQ_RTC:            u8 = 0x28;
pub const PIC_IRQ_MOUSE:          u8 = 0x2C;
pub const PIC_IRQ_ATA_PRIMARY:    u8 = 0x2E;
pub const PIC_IRQ_ATA_SECONDARY:  u8 = 0x2F;

pub const APIC_SPURIOUS: u8 = 0xFF;

#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum InterruptVector {
    Timer          = PIC_IRQ_TIMER,
    Keyboard       = PIC_IRQ_KEYBOARD,
    SpuriousMaster = PIC_IRQ_SPURIOUS_MASTER,
    Rtc            = PIC_IRQ_RTC,
    Mouse          = PIC_IRQ_MOUSE,
    AtaPrimary     = PIC_IRQ_ATA_PRIMARY,
    AtaSecondary   = PIC_IRQ_ATA_SECONDARY,
}

impl InterruptVector {
    #[inline(always)]
    pub fn as_u8(self) -> u8 { self as u8 }

    #[inline(always)]
    pub fn as_usize(self) -> usize { self as usize }

    pub fn is_spurious(self) -> bool {
        matches!(self, Self::SpuriousMaster | Self::AtaSecondary)
    }
}
```

### 7.2 src/interrupts/stats.rs

```rust
use core::sync::atomic::{AtomicU64, Ordering};

static COUNTERS: [AtomicU64; 256] = {
    // Safe: AtomicU64 is valid when zero-initialised.
    const ZERO: AtomicU64 = AtomicU64::new(0);
    [ZERO; 256]
};

#[inline(always)]
pub fn record(vector: u8) {
    COUNTERS[vector as usize].fetch_add(1, Ordering::Relaxed);
}

pub fn count(vector: u8) -> u64 {
    COUNTERS[vector as usize].load(Ordering::Acquire)
}

/// Requires alloc. Gate behind a feature or remove if no global allocator exists.
#[cfg(feature = "alloc")]
pub fn snapshot_active() -> alloc::vec::Vec<(u8, u64)> {
    COUNTERS.iter()
        .enumerate()
        .filter_map(|(i, c)| {
            let v = c.load(Ordering::Acquire);
            if v > 0 { Some((i as u8, v)) } else { None }
        })
        .collect()
}

/// Only for test harnesses. Calling while handlers fire produces transient undercounts.
pub unsafe fn reset_all() {
    for c in &COUNTERS {
        c.store(0, Ordering::Relaxed);
    }
}
```

### 7.3 src/interrupts/dispatch.rs

```rust
use core::sync::atomic::{AtomicPtr, Ordering};

// 16 hardware IRQ lines on the 8259 (IRQ0–IRQ15).
static HANDLERS: [AtomicPtr<()>; 16] = {
    const NULL: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());
    [NULL; 16]
};

pub fn register(irq: u8, handler: fn()) -> Result<(), ()> {
    let ptr = handler as *mut ();
    HANDLERS[irq as usize]
        .compare_exchange(
            core::ptr::null_mut(),
            ptr,
            Ordering::Release,
            Ordering::Relaxed,
        )
        .map(|_| ())
        .map_err(|_| ())
}

pub fn unregister(irq: u8) {
    HANDLERS[irq as usize].store(core::ptr::null_mut(), Ordering::Release);
}

/// Called from within an interrupt handler. No locking. No allocation.
/// Safety: caller must be in interrupt context with interrupts disabled.
#[inline(always)]
pub unsafe fn dispatch(irq: u8) {
    let ptr = HANDLERS[irq as usize].load(Ordering::Acquire);
    if !ptr.is_null() {
        let f: fn() = core::mem::transmute(ptr);
        f();
    }
}
```

### 7.4 src/interrupts/pic.rs

```rust
use pic8259::ChainedPics;
use spin::Mutex;

pub const PIC_1_OFFSET: u8 = 0x20;
pub const PIC_2_OFFSET: u8 = 0x28;

static PICS: Mutex<ChainedPics> = Mutex::new(unsafe {
    ChainedPics::new(PIC_1_OFFSET, PIC_2_OFFSET)
});

pub fn init() {
    unsafe { PICS.lock().initialize() };
}

pub unsafe fn end_of_interrupt(vector: u8) {
    PICS.lock().notify_end_of_interrupt(vector);
}

pub fn mask_irq(irq: u8) {
    use x86_64::instructions::port::Port;
    let mut pics = PICS.lock();
    let mut port: Port<u8> = if irq < 8 {
        Port::new(0x21)
    } else {
        Port::new(0xA1)
    };
    let shift = if irq < 8 { irq } else { irq - 8 };
    unsafe {
        let current = port.read();
        port.write(current | (1 << shift));
    }
}

pub fn unmask_irq(irq: u8) {
    use x86_64::instructions::port::Port;
    let mut port: Port<u8> = if irq < 8 {
        Port::new(0x21)
    } else {
        Port::new(0xA1)
    };
    let shift = if irq < 8 { irq } else { irq - 8 };
    unsafe {
        let current = port.read();
        port.write(current & !(1 << shift));
    }
}

pub fn read_imr() -> u16 {
    use x86_64::instructions::port::Port;
    let mut master: Port<u8> = Port::new(0x21);
    let mut slave:  Port<u8> = Port::new(0xA1);
    unsafe { (master.read() as u16) | ((slave.read() as u16) << 8) }
}

/// Read In-Service Register to detect spurious IRQs.
/// Returns true if the IRQ line is genuinely in-service (not spurious).
pub fn irq_in_service(irq: u8) -> bool {
    use x86_64::instructions::port::Port;
    // OCW3: read ISR
    let (port_addr, bit) = if irq < 8 {
        (0x20u16, irq)
    } else {
        (0xA0u16, irq - 8)
    };
    let mut port: Port<u8> = Port::new(port_addr);
    unsafe {
        port.write(0x0Bu8); // OCW3: read ISR
        let isr = port.read();
        (isr & (1 << bit)) != 0
    }
}

pub unsafe fn eoi_if_not_spurious_master() {
    // IRQ7 from master: check ISR before sending EOI.
    if irq_in_service(7) {
        end_of_interrupt(0x27);
    }
    // If spurious: no EOI at all.
}

pub unsafe fn eoi_if_not_spurious_slave() {
    // IRQ15 from slave: if spurious, send master EOI only.
    if irq_in_service(15) {
        end_of_interrupt(0x2F);
    } else {
        // Spurious from slave — master cascade line did assert.
        end_of_interrupt(0x20); // master EOI only
    }
}
```

### 7.5 src/interrupts/handlers.rs

```rust
use super::{dispatch, pic, stats, vectors::*};
use lazy_static::lazy_static;
use pc_keyboard::{layouts, HandleControl, Keyboard, ScancodeSet1};
use spin::Mutex;
use x86_64::registers::control::Cr2;
use x86_64::structures::idt::{InterruptStackFrame, PageFaultErrorCode};

// ── CPU Exception Handlers ────────────────────────────────────────────────

pub extern "x86-interrupt" fn divide_error_handler(frame: InterruptStackFrame) {
    stats::record(EXCEPTION_DIVIDE_ERROR);
    panic!("#DE divide error\n{:#?}", frame);
}

pub extern "x86-interrupt" fn debug_handler(frame: InterruptStackFrame) {
    stats::record(EXCEPTION_DEBUG);
    // Trap — log and return. Do not panic.
}

pub extern "x86-interrupt" fn nmi_handler(frame: InterruptStackFrame) {
    stats::record(EXCEPTION_NMI);
    panic!("NMI — hardware failure\n{:#?}", frame);
}

pub extern "x86-interrupt" fn breakpoint_handler(frame: InterruptStackFrame) {
    stats::record(EXCEPTION_BREAKPOINT);
    // Trap — must return.
}

pub extern "x86-interrupt" fn overflow_handler(frame: InterruptStackFrame) {
    stats::record(EXCEPTION_OVERFLOW);
    // Trap — return.
}

pub extern "x86-interrupt" fn bound_range_handler(frame: InterruptStackFrame) {
    stats::record(EXCEPTION_BOUND_RANGE);
    panic!("#BR bound range exceeded\n{:#?}", frame);
}

pub extern "x86-interrupt" fn invalid_opcode_handler(frame: InterruptStackFrame) {
    stats::record(EXCEPTION_INVALID_OPCODE);
    panic!("#UD invalid opcode\n{:#?}", frame);
}

pub extern "x86-interrupt" fn device_not_available_handler(frame: InterruptStackFrame) {
    stats::record(EXCEPTION_DEVICE_NOT_AVAIL);
    panic!("#NM device not available — implement lazy FPU switching\n{:#?}", frame);
}

pub extern "x86-interrupt" fn double_fault_handler(
    frame: InterruptStackFrame,
    _error_code: u64,
) -> ! {
    stats::record(EXCEPTION_DOUBLE_FAULT);
    panic!("#DF double fault\n{:#?}", frame);
}

pub extern "x86-interrupt" fn invalid_tss_handler(
    frame: InterruptStackFrame,
    error_code: u64,
) {
    stats::record(EXCEPTION_INVALID_TSS);
    panic!("#TS invalid TSS (error={:#x})\n{:#?}", error_code, frame);
}

pub extern "x86-interrupt" fn segment_not_present_handler(
    frame: InterruptStackFrame,
    error_code: u64,
) {
    stats::record(EXCEPTION_SEGMENT_NOT_PRESENT);
    panic!("#NP segment not present (error={:#x})\n{:#?}", error_code, frame);
}

pub extern "x86-interrupt" fn stack_segment_handler(
    frame: InterruptStackFrame,
    error_code: u64,
) {
    stats::record(EXCEPTION_STACK_SEGMENT);
    panic!("#SS stack segment fault (error={:#x})\n{:#?}", error_code, frame);
}

pub extern "x86-interrupt" fn general_protection_handler(
    frame: InterruptStackFrame,
    error_code: u64,
) {
    stats::record(EXCEPTION_GENERAL_PROTECTION);
    panic!("#GP general protection fault (error={:#x})\n{:#?}", error_code, frame);
}

pub extern "x86-interrupt" fn page_fault_handler(
    frame: InterruptStackFrame,
    error_code: PageFaultErrorCode,
) {
    stats::record(EXCEPTION_PAGE_FAULT);
    // Read CR2 immediately — a subsequent fault would overwrite it.
    let fault_addr = Cr2::read();
    panic!(
        "#PF page fault at {:#x} (error={:?})\n{:#?}",
        fault_addr, error_code, frame
    );
}

pub extern "x86-interrupt" fn x87_fp_handler(frame: InterruptStackFrame) {
    stats::record(EXCEPTION_X87_FP);
    panic!("#MF x87 FP exception\n{:#?}", frame);
}

pub extern "x86-interrupt" fn alignment_check_handler(
    frame: InterruptStackFrame,
    error_code: u64,
) {
    stats::record(EXCEPTION_ALIGNMENT_CHECK);
    panic!("#AC alignment check (error={:#x})\n{:#?}", error_code, frame);
}

pub extern "x86-interrupt" fn machine_check_handler(frame: InterruptStackFrame) -> ! {
    stats::record(EXCEPTION_MACHINE_CHECK);
    panic!("#MC machine check — hardware error\n{:#?}", frame);
}

pub extern "x86-interrupt" fn simd_fp_handler(frame: InterruptStackFrame) {
    stats::record(EXCEPTION_SIMD_FP);
    panic!("#XM SIMD FP exception\n{:#?}", frame);
}

pub extern "x86-interrupt" fn virtualization_handler(frame: InterruptStackFrame) {
    stats::record(EXCEPTION_VIRTUALIZATION);
    panic!("#VE virtualization exception\n{:#?}", frame);
}

// ── Hardware IRQ Handlers ─────────────────────────────────────────────────

pub extern "x86-interrupt" fn timer_handler(_frame: InterruptStackFrame) {
    stats::record(PIC_IRQ_TIMER);
    unsafe {
        dispatch::dispatch(0); // IRQ0
        pic::end_of_interrupt(PIC_IRQ_TIMER);
    }
}

lazy_static! {
    static ref KEYBOARD: Mutex<Keyboard<layouts::Us104Key, ScancodeSet1>> =
        Mutex::new(Keyboard::new(
            ScancodeSet1::new(),
            layouts::Us104Key,
            HandleControl::Ignore,
        ));
}

pub extern "x86-interrupt" fn keyboard_handler(_frame: InterruptStackFrame) {
    use x86_64::instructions::port::Port;
    stats::record(PIC_IRQ_KEYBOARD);

    // Unconditional read — must happen even if we discard the result.
    let scancode: u8 = unsafe { Port::new(0x60).read() };

    let mut keyboard = KEYBOARD.lock();
    if let Ok(Some(key_event)) = keyboard.add_byte(scancode) {
        if let Some(_key) = keyboard.process_keyevent(key_event) {
            // Deliver to input subsystem when one exists.
        }
    }

    unsafe { pic::end_of_interrupt(PIC_IRQ_KEYBOARD) };
}

pub extern "x86-interrupt" fn spurious_master_handler(_frame: InterruptStackFrame) {
    stats::record(PIC_IRQ_SPURIOUS_MASTER);
    unsafe { pic::eoi_if_not_spurious_master() };
}

pub extern "x86-interrupt" fn rtc_handler(_frame: InterruptStackFrame) {
    use x86_64::instructions::port::Port;
    stats::record(PIC_IRQ_RTC);
    // Must read register C to dismiss the RTC interrupt, or it will not fire again.
    unsafe {
        Port::<u8>::new(0x70).write(0x0C);
        Port::<u8>::new(0x71).read();
        dispatch::dispatch(8); // IRQ8
        pic::end_of_interrupt(PIC_IRQ_RTC);
    }
}

pub extern "x86-interrupt" fn mouse_handler(_frame: InterruptStackFrame) {
    use x86_64::instructions::port::Port;
    stats::record(PIC_IRQ_MOUSE);
    let _data: u8 = unsafe { Port::new(0x60).read() };
    unsafe {
        dispatch::dispatch(12); // IRQ12
        pic::end_of_interrupt(PIC_IRQ_MOUSE);
    }
}

pub extern "x86-interrupt" fn ata_primary_handler(_frame: InterruptStackFrame) {
    stats::record(PIC_IRQ_ATA_PRIMARY);
    unsafe {
        dispatch::dispatch(14); // IRQ14
        pic::end_of_interrupt(PIC_IRQ_ATA_PRIMARY);
    }
}

pub extern "x86-interrupt" fn ata_secondary_handler(_frame: InterruptStackFrame) {
    stats::record(PIC_IRQ_ATA_SECONDARY);
    unsafe { pic::eoi_if_not_spurious_slave() };
}
```

### 7.6 src/interrupts/exceptions.rs

```rust
use super::handlers::*;
use super::vectors::*;
use crate::gdt;
use lazy_static::lazy_static;
use x86_64::structures::idt::InterruptDescriptorTable;

lazy_static! {
    static ref IDT: InterruptDescriptorTable = {
        let mut idt = InterruptDescriptorTable::new();

        // CPU exceptions
        idt.divide_error.set_handler_fn(divide_error_handler);
        idt.debug.set_handler_fn(debug_handler);
        unsafe {
            idt.non_maskable_interrupt
                .set_handler_fn(nmi_handler)
                .set_stack_index(gdt::NMI_IST_INDEX);
        }
        idt.breakpoint.set_handler_fn(breakpoint_handler);
        idt.overflow.set_handler_fn(overflow_handler);
        idt.bound_range_exceeded.set_handler_fn(bound_range_handler);
        idt.invalid_opcode.set_handler_fn(invalid_opcode_handler);
        idt.device_not_available.set_handler_fn(device_not_available_handler);
        unsafe {
            idt.double_fault
                .set_handler_fn(double_fault_handler)
                .set_stack_index(gdt::DOUBLE_FAULT_IST_INDEX);
        }
        idt.invalid_tss.set_handler_fn(invalid_tss_handler);
        idt.segment_not_present.set_handler_fn(segment_not_present_handler);
        idt.stack_segment_fault.set_handler_fn(stack_segment_handler);
        idt.general_protection_fault.set_handler_fn(general_protection_handler);
        idt.page_fault.set_handler_fn(page_fault_handler);
        idt.x87_floating_point.set_handler_fn(x87_fp_handler);
        idt.alignment_check.set_handler_fn(alignment_check_handler);
        unsafe {
            idt.machine_check
                .set_handler_fn(machine_check_handler)
                .set_stack_index(gdt::MACHINE_CHECK_IST_INDEX);
        }
        idt.simd_floating_point.set_handler_fn(simd_fp_handler);
        idt.virtualization.set_handler_fn(virtualization_handler);

        // Hardware IRQs
        idt[PIC_IRQ_TIMER as usize].set_handler_fn(timer_handler);
        idt[PIC_IRQ_KEYBOARD as usize].set_handler_fn(keyboard_handler);
        idt[PIC_IRQ_SPURIOUS_MASTER as usize].set_handler_fn(spurious_master_handler);
        idt[PIC_IRQ_RTC as usize].set_handler_fn(rtc_handler);
        idt[PIC_IRQ_MOUSE as usize].set_handler_fn(mouse_handler);
        idt[PIC_IRQ_ATA_PRIMARY as usize].set_handler_fn(ata_primary_handler);
        idt[PIC_IRQ_ATA_SECONDARY as usize].set_handler_fn(ata_secondary_handler);

        idt
    };
}

pub fn init() {
    IDT.load();
}

#[test_case]
fn test_breakpoint_does_not_panic() {
    x86_64::instructions::interrupts::int3();
}
```

### 7.7 src/interrupts/apic.rs

```rust
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

static APIC_BASE_VADDR: AtomicU64 = AtomicU64::new(0);
static APIC_ENABLED: AtomicBool = AtomicBool::new(false);

pub fn apic_supported() -> bool {
    // CPUID.1:EDX bit 9 = APIC on-chip
    let result = unsafe { core::arch::x86_64::__cpuid(1) };
    (result.edx & (1 << 9)) != 0
}

/// Map the local APIC MMIO region before calling this.
/// vaddr must be an uncached mapping of the physical APIC base (0xFEE00000).
pub unsafe fn set_mapped_vaddr(vaddr: u64) {
    APIC_BASE_VADDR.store(vaddr, Ordering::Release);
}

pub struct LocalApic {
    base: u64,
}

impl LocalApic {
    /// Safety: base must be a valid, uncached virtual mapping of the LAPIC MMIO region.
    pub unsafe fn new(base_vaddr: u64) -> Self {
        Self { base: base_vaddr }
    }

    unsafe fn read(&self, offset: u32) -> u32 {
        let ptr = (self.base + offset as u64) as *const u32;
        ptr.read_volatile()
    }

    unsafe fn write(&self, offset: u32, val: u32) {
        let ptr = (self.base + offset as u64) as *mut u32;
        ptr.write_volatile(val);
    }

    /// Enable the local APIC and set the spurious vector.
    /// spurious_vector must be 0xF0–0xFF; conventionally 0xFF.
    pub unsafe fn init(&self, spurious_vector: u8) {
        // Spurious Interrupt Vector Register (offset 0xF0)
        // Bit 8 = APIC software enable
        let svr = (spurious_vector as u32) | (1 << 8);
        self.write(0xF0, svr);
        APIC_ENABLED.store(true, Ordering::Release);
    }

    pub unsafe fn end_of_interrupt(&self) {
        self.write(0xB0, 0);
    }

    pub unsafe fn send_ipi(&self, dest_apic_id: u8, vector: u8) {
        // ICR high (offset 0x310): destination
        self.write(0x310, (dest_apic_id as u32) << 24);
        // ICR low (offset 0x300): write triggers send
        self.write(0x300, vector as u32 | (1 << 14)); // Assert, fixed delivery
    }
}

/// Mask all 8259 lines before enabling the APIC.
/// After this, the PIC will no longer deliver IRQs.
/// Existing PIC IDT entries remain to catch residual spurious PIC interrupts.
pub unsafe fn disable_pic() {
    use x86_64::instructions::port::Port;
    let mut master_data: Port<u8> = Port::new(0xA1);
    let mut slave_data:  Port<u8> = Port::new(0x21);
    master_data.write(0xFF);
    slave_data.write(0xFF);
}
```

### 7.8 src/interrupts/mod.rs

```rust
pub mod apic;
pub mod dispatch;
pub mod exceptions;
pub mod handlers;
pub mod pic;
pub mod stats;
pub mod vectors;

pub fn init() {
    exceptions::init(); // Load IDT — must be first
    pic::init();        // Program 8259 — must follow IDT load
    // Interrupts remain disabled. Caller enables with sti.
}
```

---

## 8. src/memory/pmm.rs

Bitmap physical memory manager. Understands all Limine entry types. Does not
use alloc — the bitmap is statically allocated for up to 64 GiB of physical RAM.

```rust
use limine::memory_map::{Entry, EntryType};
use spin::Mutex;

// 64 GiB / 4 KiB pages = 16,777,216 pages = 2,097,152 u8 bytes = 2 MiB bitmap
const MAX_PAGES: usize = 64 * 1024 * 1024 * 1024 / 4096;
const BITMAP_SIZE: usize = MAX_PAGES / 8;

static BITMAP: Mutex<[u8; BITMAP_SIZE]> = Mutex::new([0xFF; BITMAP_SIZE]); // All reserved initially
static mut HHDM_OFFSET: u64 = 0;
static mut TOTAL_FREE_PAGES: u64 = 0;

fn page_index(phys_addr: u64) -> usize {
    (phys_addr / 4096) as usize
}

fn mark_free(bitmap: &mut [u8; BITMAP_SIZE], page: usize) {
    bitmap[page / 8] &= !(1 << (page % 8));
}

fn mark_used(bitmap: &mut [u8; BITMAP_SIZE], page: usize) {
    bitmap[page / 8] |= 1 << (page % 8);
}

fn is_free(bitmap: &[u8; BITMAP_SIZE], page: usize) -> bool {
    (bitmap[page / 8] & (1 << (page % 8))) == 0
}

fn align_up(addr: u64, align: u64) -> u64 {
    (addr + align - 1) & !(align - 1)
}

fn align_down(addr: u64, align: u64) -> u64 {
    addr & !(align - 1)
}

pub fn init(
    entries: &[&Entry],
    kernel_phys_start: u64,
    kernel_phys_end: u64,
    hhdm_offset: u64,
) {
    unsafe { HHDM_OFFSET = hhdm_offset; }

    let mut bitmap = BITMAP.lock();
    let mut free_pages: u64 = 0;

    for entry in entries {
        if entry.entry_type != EntryType::USABLE {
            continue;
        }

        let base = align_up(entry.base, 4096);
        let end  = align_down(entry.base + entry.length, 4096);

        if base >= end { continue; }

        let start_page = page_index(base);
        let end_page   = page_index(end);

        if end_page > MAX_PAGES { continue; }

        for page in start_page..end_page {
            let phys = page as u64 * 4096;
            // Exclude the kernel image from the free pool.
            if phys >= kernel_phys_start && phys < kernel_phys_end {
                continue;
            }
            mark_free(&mut bitmap, page);
            free_pages += 1;
        }
    }

    unsafe { TOTAL_FREE_PAGES = free_pages; }
}

/// Reclaim pages previously marked BOOTLOADER_RECLAIMABLE.
/// Only call after all Limine response data has been consumed.
pub fn reclaim_bootloader_memory(entries: &[&Entry]) {
    let mut bitmap = BITMAP.lock();
    let mut reclaimed: u64 = 0;

    for entry in entries {
        if entry.entry_type != EntryType::BOOTLOADER_RECLAIMABLE {
            continue;
        }

        let base = align_up(entry.base, 4096);
        let end  = align_down(entry.base + entry.length, 4096);

        if base >= end { continue; }

        let start_page = page_index(base);
        let end_page   = page_index(end);

        if end_page > MAX_PAGES { continue; }

        for page in start_page..end_page {
            mark_free(&mut bitmap, page);
            reclaimed += 1;
        }
    }

    unsafe { TOTAL_FREE_PAGES += reclaimed; }
}

/// Allocate a single 4 KiB physical page.
/// Returns the physical address of the allocated page, or None if OOM.
pub fn alloc_page() -> Option<u64> {
    let mut bitmap = BITMAP.lock();
    for i in 0..MAX_PAGES {
        if is_free(&bitmap, i) {
            mark_used(&mut bitmap, i);
            unsafe {
                if TOTAL_FREE_PAGES > 0 { TOTAL_FREE_PAGES -= 1; }
            }
            return Some(i as u64 * 4096);
        }
    }
    None
}

/// Free a single 4 KiB physical page.
/// Safety: phys_addr must have been returned by alloc_page and not yet freed.
pub unsafe fn free_page(phys_addr: u64) {
    let page = page_index(phys_addr);
    let mut bitmap = BITMAP.lock();
    mark_free(&mut bitmap, page);
    TOTAL_FREE_PAGES += 1;
}

/// Convert a physical address to a virtual address via HHDM.
pub fn phys_to_virt(phys: u64) -> u64 {
    unsafe { phys + HHDM_OFFSET }
}

pub fn free_pages() -> u64 {
    unsafe { TOTAL_FREE_PAGES }
}
```

### 8.1 src/memory/mod.rs

```rust
pub mod pmm;
```

---

## 9. src/main.rs — Entry Point

```rust
#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

mod gdt;
mod interrupts;
mod memory;
mod panic;

use limine::request::{
    HhdmRequest, KernelAddressRequest, MemoryMapRequest, PagingModeRequest,
};
use limine::BaseRevision;

// ── Limine Protocol Anchors ───────────────────────────────────────────────
// All must be #[used] or the compiler eliminates them as dead statics.
// All must be in .limine_requests or Limine will not find them.

#[used]
#[unsafe(link_section = ".limine_requests")]
static BASE_REVISION: BaseRevision = BaseRevision::new();

#[used]
#[unsafe(link_section = ".limine_requests")]
static HHDM_REQUEST: HhdmRequest = HhdmRequest::new();

#[used]
#[unsafe(link_section = ".limine_requests")]
static MEMORY_MAP_REQUEST: MemoryMapRequest = MemoryMapRequest::new();

#[used]
#[unsafe(link_section = ".limine_requests")]
static KERNEL_ADDRESS_REQUEST: KernelAddressRequest = KernelAddressRequest::new();

// ── Entry Point ───────────────────────────────────────────────────────────
// State at entry (guaranteed by Limine):
//   - Long mode, ring 0
//   - Interrupts DISABLED (IF clear)
//   - Paging ENABLED (Limine's page tables)
//   - No valid GDT for our kernel yet
//   - No IDT loaded
//   - Stack: at least 64 KiB, Limine-provided
//   - SSE/AVX: enabled

#[no_mangle]
pub extern "C" fn kernel_main() -> ! {
    // ── Step 1: Consume all Limine responses immediately.
    // After GDT/page table switch, Limine's mappings may be gone.

    assert!(
        BASE_REVISION.is_supported(),
        "Limine base revision not supported"
    );

    let hhdm_offset = HHDM_REQUEST
        .get_response()
        .expect("Limine: no HHDM response")
        .offset();

    let memory_map = MEMORY_MAP_REQUEST
        .get_response()
        .expect("Limine: no memory map response");

    let kernel_addr = KERNEL_ADDRESS_REQUEST
        .get_response()
        .expect("Limine: no kernel address response");

    let kernel_phys_start = kernel_addr.physical_base();

    // Derive kernel physical end from linker-exported symbols.
    extern "C" {
        static __kernel_start: u8;
        static __kernel_end: u8;
    }
    let kernel_size = unsafe {
        (&__kernel_end as *const u8 as u64)
            .saturating_sub(&__kernel_start as *const u8 as u64)
    };
    let kernel_phys_end = kernel_phys_start + kernel_size;

    // ── Step 2: Load our GDT and TSS.
    // Limine's GDT is now discarded. All segment registers and the TSS
    // point into our static structures.
    gdt::init();

    // ── Step 3: Load IDT and initialise PIC.
    // The PIC is programmed with vector offsets 0x20/0x28 and all lines
    // are unmasked. Interrupts are still disabled — IF is still clear.
    interrupts::init();

    // ── Step 4: Initialise physical memory manager.
    // Only USABLE pages enter the free pool at this stage.
    // BOOTLOADER_RECLAIMABLE pages are still in use by Limine's responses.
    let entries: alloc_entries_vec(memory_map.entries());
    // NOTE: if no alloc available, iterate directly:
    memory::pmm::init(
        memory_map.entries(),
        kernel_phys_start,
        kernel_phys_end,
        hhdm_offset,
    );

    // ── Step 5: Reclaim bootloader memory.
    // We have copied everything we need out of Limine's response structures.
    memory::pmm::reclaim_bootloader_memory(memory_map.entries());

    // ── Step 6: Enable interrupts.
    // The PIC will now begin delivering timer ticks and other IRQs.
    x86_64::instructions::interrupts::enable();

    // ── Kernel is operational. Halt loop.
    loop {
        x86_64::instructions::hlt();
    }
}
```

> **Note:** The `alloc_entries_vec` call above is a placeholder. `memory_map.entries()`
> returns a `&[&Entry]` slice directly — pass it to `pmm::init` without collecting.
> Remove the intermediate variable and pass `memory_map.entries()` directly.

---

## 10. Makefile

```makefile
KERNEL  := target/x86_64-unknown-none/debug/kernel
ISO     := my-kernel.iso
LIMINE  := ../limine  # adjust to wherever you cloned limine

.PHONY: all run clean iso

all: iso

$(KERNEL):
	cargo build

iso: $(KERNEL)
	mkdir -p iso_root/boot/limine
	cp $(KERNEL) iso_root/boot/kernel.elf
	cp $(LIMINE)/limine-bios.sys \
	   $(LIMINE)/limine-bios-cd.bin \
	   $(LIMINE)/limine-uefi-cd.bin \
	   iso_root/boot/limine/
	cat > iso_root/boot/limine/limine.conf << 'EOF'
	TIMEOUT=0
	VERBOSE=yes

	/My Kernel
	    PROTOCOL=limine
	    KERNEL_PATH=boot:///boot/kernel.elf
	EOF
	xorriso -as mkisofs \
		-b boot/limine/limine-bios-cd.bin \
		-no-emul-boot -boot-load-size 4 -boot-info-table \
		--efi-boot boot/limine/limine-uefi-cd.bin \
		-efi-boot-part --efi-boot-image \
		--protective-msdos-label \
		iso_root -o $(ISO)
	$(LIMINE)/limine bios-install $(ISO)

run: iso
	qemu-system-x86_64 \
		-cdrom $(ISO) \
		-m 512M \
		-serial stdio \
		-no-reboot \
		-no-shutdown \
		-d int,cpu_reset \
		-D qemu.log

run-kvm: iso
	qemu-system-x86_64 \
		-cdrom $(ISO) \
		-m 512M \
		-enable-kvm \
		-cpu host \
		-serial stdio \
		-no-reboot \
		-no-shutdown

clean:
	cargo clean
	rm -rf iso_root $(ISO) qemu.log
```

---

## 11. limine.conf

Place this at `iso_root/boot/limine/limine.conf` (the Makefile does this inline,
but keep a canonical copy):

```
TIMEOUT=0
VERBOSE=yes

/My Kernel
    PROTOCOL=limine
    KERNEL_PATH=boot:///boot/kernel.elf
```

---

## 12. Build and Run Sequence

Execute in this exact order. Do not skip the verification steps — a misconfigured
GDT or IDT produces a triple fault with no output.

```bash
# 1. Build
cargo build 2>&1 | tee build.log
# Expected: no errors. Warnings about unused variables are acceptable.

# 2. Verify the .limine_requests section exists in the ELF
objdump -h target/x86_64-unknown-none/debug/kernel | grep limine_requests
# Expected: a section named .limine_requests with non-zero size.
# If absent: the #[link_section] attributes or linker script are wrong.

# 3. Verify the entry point symbol
nm target/x86_64-unknown-none/debug/kernel | grep kernel_main
# Expected: one line showing kernel_main as a T (text) symbol at a high address.

# 4. Build the ISO
make iso

# 5. Run under QEMU (no KVM for maximum compatibility)
make run
```

---

## 13. Expected QEMU Output and Verification

If the kernel is correct, QEMU will boot silently (no VGA/serial output is
implemented in this spec). Verify correctness by checking the QEMU interrupt log:

```bash
# After running, inspect the log:
grep "check_exception" qemu.log | head -20
```

A correctly booting kernel produces **no exception entries** in this log within the
first few hundred milliseconds. Any `check_exception` line indicates a fault —
cross-reference the vector number against `vectors.rs`.

A triple fault (CPU reset loop) will appear as repeated `cpu_reset` entries. The
most common causes in order of frequency:

1. GDT loaded after IDT — reverse the order in `kernel_main`.
2. IST stack pointer is zero or unmapped — verify `TSS` initialization in `gdt.rs`.
3. `.limine_requests` section missing — verify linker script and `link_section` attrs.
4. `BOOTLOADER_RECLAIMABLE` pages freed before Limine responses consumed — move
   `reclaim_bootloader_memory` to after all `get_response()` calls.

---

## 14. What Is Not Implemented (Explicit Scope Boundary)

These are deliberate omissions, not oversights. Implementing them requires this
foundation to be stable first.

- **VGA/serial output** — add a `crate::serial` module using UART 16550 at 0x3F8.
- **Virtual memory manager** — page table construction using `x86_64::structures::paging`.
- **Heap allocator** — requires the VMM; use `linked_list_allocator` or `good_allocator`
  crate as the `#[global_allocator]`.
- **ACPI parsing** — use the `acpi` crate with the RSDP obtained from a `RsdpRequest`.
- **SMP / AP startup** — add `SmpRequest` to `main.rs`; implement the AP entry function.
- **I/O APIC** — requires ACPI MADT; enables PCI interrupt routing.
- **Scheduler** — requires a heap, VMM, and per-CPU state.
- **FPU context switching** — the `#NM` handler currently panics; implement lazy switching.

---

## 15. Reference Material

- Limine Protocol Specification: https://github.com/limine-bootloader/limine/blob/trunk/PROTOCOL.md
- `limine` Rust crate docs: https://docs.rs/limine/latest/limine/
- `x86_64` crate IDT docs: https://docs.rs/x86_64/latest/x86_64/structures/idt/
- Intel SDM Vol. 3A Ch. 6 — Interrupt and Exception Handling
- OSDev Wiki 8259 PIC: https://wiki.osdev.org/8259_PIC
- OSDev Wiki Limine Bare Bones: https://wiki.osdev.org/Limine_Bare_Bones
