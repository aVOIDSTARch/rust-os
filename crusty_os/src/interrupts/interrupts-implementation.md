# Interrupt Subsystem Implementation Guide

> **Audience:** A Claude instance integrating the `interrupts/` subsystem into an existing
> Rust x86_64 kernel. This document is exhaustive by intent — guessing at integration
> details in kernel code produces triple faults, not compiler errors.

---

## What This Subsystem Is

Eight files that replace a single `interrupts.rs` module. The original handled two of twenty
CPU exception vectors, had no spurious IRQ detection, no runtime handler registration, and
no telemetry. This subsystem handles the full x86_64 interrupt vector space correctly.

```
interrupts/
├── mod.rs        — public surface, init() entry point
├── vectors.rs    — all 256 vector constants + InterruptVector enum
├── exceptions.rs — IDT construction and loading (lazy_static)
├── handlers.rs   — every handler implementation
├── pic.rs        — 8259 PIC init, masking, EOI, spurious IRQ detection
├── dispatch.rs   — runtime second-level IRQ registration table
├── stats.rs      — lock-free per-vector AtomicU64 counters
└── apic.rs       — APIC detection and SMP migration skeleton
```

---

## Prerequisites

Before integrating this subsystem, the following must already exist and be correct in the
kernel. Each item is a hard dependency — the subsystem will not compile or will triple-fault
without them.

### 1. Cargo.toml dependencies

```toml
[dependencies]
x86_64       = "0.15"
pic8259      = "0.10"
spin         = "0.9"
lazy_static  = { version = "1.4", features = ["spin_no_std"] }
pc_keyboard  = "0.7"
```

`pc_keyboard` is used only in `handlers::keyboard_handler`. If you do not need keyboard
input, remove that handler's body and drop the dependency. Everything else is required.

### 2. Three IST stacks in the GDT/TSS

`exceptions.rs` assigns IST stack indices to three handlers:

| Handler         | Constant required               | IST slot |
|-----------------|---------------------------------|----------|
| Double fault    | `crate::gdt::DOUBLE_FAULT_IST_INDEX` | IST1 |
| NMI             | `crate::gdt::NMI_IST_INDEX`         | IST2 |
| Machine check   | `crate::gdt::MACHINE_CHECK_IST_INDEX`| IST3 |

Your `gdt` module must export all three constants as `pub const` of type `u16`. If your
existing GDT only has `DOUBLE_FAULT_IST_INDEX`, add the other two.

A minimal GDT implementation that satisfies the dependency:

```rust
// src/gdt.rs
use x86_64::structures::tss::TaskStateSegment;
use x86_64::VirtAddr;

pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;
pub const NMI_IST_INDEX:          u16 = 1;
pub const MACHINE_CHECK_IST_INDEX: u16 = 2;

// Stack sizes — 4 KiB each is the practical minimum.
// Stack overflow within an IST handler is undetectable and will corrupt
// whatever memory lies below, so size these generously.
const STACK_SIZE: usize = 4096 * 5; // 20 KiB per IST stack

static mut DOUBLE_FAULT_STACK: [u8; STACK_SIZE] = [0; STACK_SIZE];
static mut NMI_STACK:          [u8; STACK_SIZE] = [0; STACK_SIZE];
static mut MACHINE_CHECK_STACK:[u8; STACK_SIZE] = [0; STACK_SIZE];

lazy_static::lazy_static! {
    static ref TSS: TaskStateSegment = {
        let mut tss = TaskStateSegment::new();

        tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = {
            let stack_start = VirtAddr::from_ptr(unsafe { &DOUBLE_FAULT_STACK });
            stack_start + STACK_SIZE
        };

        tss.interrupt_stack_table[NMI_IST_INDEX as usize] = {
            let stack_start = VirtAddr::from_ptr(unsafe { &NMI_STACK });
            stack_start + STACK_SIZE
        };

        tss.interrupt_stack_table[MACHINE_CHECK_IST_INDEX as usize] = {
            let stack_start = VirtAddr::from_ptr(unsafe { &MACHINE_CHECK_STACK });
            stack_start + STACK_SIZE
        };

        tss
    };
}
```

The IST index stored in the IDT entry is 1-based (1–7), but `x86_64::structures::tss`
indexes `interrupt_stack_table` 0-based (0–6). The `x86_64` crate handles this translation
automatically when you call `.set_stack_index()` — do not add 1 yourself.

### 3. `print!` and `println!` macros

`handlers.rs` calls `crate::print!` and `crate::println!` directly. These must write to a
VGA text buffer or serial port that does not itself use locking that would deadlock in
interrupt context. A `spin::Mutex`-protected VGA writer is safe. A `std::sync::Mutex` is
not — it can block.

### 4. `#![no_std]` environment

All eight files are `no_std`. They use `core::` only, plus `alloc::` in one place:
`stats::snapshot_active()` returns a `Vec<(u8, u64)>`. If your kernel does not have a
global allocator, either provide one or remove that function. The rest of the subsystem
does not touch the heap.

---

## File Placement

Place all eight files as a module directory:

```
src/
└── interrupts/
    ├── mod.rs
    ├── vectors.rs
    ├── exceptions.rs
    ├── handlers.rs
    ├── pic.rs
    ├── dispatch.rs
    ├── stats.rs
    └── apic.rs
```

In `src/lib.rs` (or `src/main.rs`), declare:

```rust
pub mod interrupts;
pub mod gdt; // must already exist
```

---

## Initialization

Call `interrupts::init()` from your kernel entry point. The call site must be after GDT and
TSS initialization, and before any code that could trigger a fault or hardware interrupt.

```rust
// src/lib.rs or src/main.rs
pub fn kernel_main() -> ! {
    gdt::init();          // 1. GDT + TSS loaded first
    interrupts::init();   // 2. IDT loaded, PIC unmasked
    // sti is implicit — the PIC starts delivering IRQs when you
    // first return from an interrupt or call x86_64::instructions::interrupts::enable()
    loop {
        x86_64::instructions::hlt();
    }
}
```

`interrupts::init()` does exactly two things, in order:

1. `exceptions::init()` — builds the IDT in a `lazy_static` and calls `IDT.load()`, which
   writes the IDT base address and limit to the CPU's IDTR register via `lidt`.
2. `pic::init()` — calls `ChainedPics::initialize()`, which programs the 8259 PICs with the
   correct vector offsets (0x20 and 0x28) and unmasks all IRQ lines.

**Do not call `x86_64::instructions::interrupts::enable()` between these two steps.**
After `exceptions::init()` but before `pic::init()`, the IDT is loaded but the PICs are
still in their BIOS-configured state (vectors 0x08–0x0F), which collide with CPU exceptions.
A timer tick in that window would be delivered to the wrong handler.

---

## Module Reference

### `vectors.rs` — The single source of truth for all vector numbers

Every interrupt vector number in the subsystem is defined here as a `pub const u8`. No
other file spells out a raw vector number inline; they import from here.

The `InterruptVector` enum wraps the PIC IRQ vectors with typed names and provides two
conversion methods (`as_u8()`, `as_usize()`) used to index the IDT. The `is_spurious()`
predicate identifies IRQ7 and IRQ15 — vectors that require ISR inspection before EOI.

**If you add a new hardware IRQ handler:**
1. Add a `pub const PIC_IRQ_*: u8` constant.
2. Add a variant to `InterruptVector` referencing that constant.
3. Register it in `exceptions.rs` and implement the handler in `handlers.rs`.

Do not add new exception constants — the CPU exception space (0x00–0x1F) is fixed by Intel.

---

### `exceptions.rs` — IDT construction

Contains a single `lazy_static!` block that builds and returns the `InterruptDescriptorTable`.
The IDT is initialized once on first access, which happens when `exceptions::init()` calls
`IDT.load()`.

**Exception handlers registered (20 total):**

| Vector | Mnemonic | Handler | IST | Notes |
|--------|----------|---------|-----|-------|
| 0x00 | #DE | `divide_error_handler` | — | Fault; panics |
| 0x01 | #DB | `debug_handler` | — | Fault/trap; logs and returns |
| 0x02 | NMI | `nmi_handler` | IST2 | Panics; almost always hardware failure |
| 0x03 | #BP | `breakpoint_handler` | — | Trap; logs and returns |
| 0x04 | #OF | `overflow_handler` | — | Trap; logs and returns |
| 0x05 | #BR | `bound_range_handler` | — | Fault; panics |
| 0x06 | #UD | `invalid_opcode_handler` | — | Fault; panics |
| 0x07 | #NM | `device_not_available_handler` | — | Fault; panics |
| 0x08 | #DF | `double_fault_handler` | IST1 | Abort; `-> !`; panics |
| 0x0A | #TS | `invalid_tss_handler` | — | Fault + error code; panics |
| 0x0B | #NP | `segment_not_present_handler` | — | Fault + error code; panics |
| 0x0C | #SS | `stack_segment_handler` | — | Fault + error code; panics |
| 0x0D | #GP | `general_protection_handler` | — | Fault + error code; panics |
| 0x0E | #PF | `page_fault_handler` | — | Fault + error code + CR2; panics |
| 0x10 | #MF | `x87_fp_handler` | — | Fault; panics |
| 0x11 | #AC | `alignment_check_handler` | — | Fault + error code; panics |
| 0x12 | #MC | `machine_check_handler` | IST3 | Abort; `-> !`; panics |
| 0x13 | #XM | `simd_fp_handler` | — | Fault; panics |
| 0x14 | #VE | `virtualization_handler` | — | Fault; panics |

Vectors 0x09 (obsolete coprocessor segment overrun), 0x0F (reserved), 0x15–0x1F
(reserved or AMD-SVM-specific) are intentionally left unregistered. The CPU delivers an
unhandled vector as a #GP, which is caught.

**Hardware IRQ handlers registered (7 of 14 lines):**

| IRQ | Vector | Handler | Notes |
|-----|--------|---------|-------|
| 0 | 0x20 | `timer_handler` | Dispatches to second-level handler then EOI |
| 1 | 0x21 | `keyboard_handler` | Reads PS/2 port, decodes scancode, EOI |
| 7 | 0x27 | `spurious_master_handler` | ISR check before any EOI |
| 8 | 0x28 | `rtc_handler` | Must read register C before EOI |
| 12 | 0x2C | `mouse_handler` | Reads PS/2 data port, EOI |
| 14 | 0x2E | `ata_primary_handler` | EOI only |
| 15 | 0x2F | `ata_secondary_handler` | ISR check; EOI routing differs for spurious |

IRQ2 (cascade), IRQ3–6, IRQ9–11, IRQ13 are left unregistered. They will deliver as
unhandled vectors, producing a #GP — acceptable for a kernel that does not use those
devices. If you add serial ports, a floppy, or PCI legacy interrupts, register handlers for
those IRQ lines here.

---

### `handlers.rs` — All handler implementations

Every function registered in `exceptions.rs` is implemented here. The structure is uniform:

**For CPU exceptions:**
```
stats::record(VECTOR_NUMBER);
// context-appropriate action (panic or log-and-return)
```

**For hardware IRQs:**
```
stats::record(vector);
// device-specific acknowledge (port read, register clear)
unsafe { dispatch::dispatch(irq_number); }
unsafe { pic::end_of_interrupt(vector); }
```

The EOI **always comes last**. Sending EOI before the device has been acknowledged can
cause an immediate re-interrupt, which in a spin-locking handler produces a deadlock.

**The page fault handler reads CR2 immediately on entry.** CR2 holds the faulting linear
address. Any subsequent instruction — including the `stats::record` call — could in
principle trigger another fault that overwrites CR2. The current ordering is correct:
`Cr2::read()` is the first operation after `stats::record`, and `stats::record` does
not fault. Do not reorder these without considering this constraint.

**The double fault and machine check handlers are `-> !`.** They call `panic!`, which
in a typical `no_std` kernel calls a diverging panic handler. The `x86_64` crate requires
the `-> !` return type for these two specific IDT slots — it is not optional. If your panic
handler is not diverging, the code will not compile.

**The keyboard handler owns a `lazy_static` PS/2 decoder state machine.** This is
intentional — the decoder must persist across interrupts. The `spin::Mutex` inside is
appropriate here because the keyboard handler cannot be re-entered (x86 interrupt gates
clear IF on entry). There is no deadlock risk from the Mutex itself, but do not call
`KEYBOARD.lock()` from non-interrupt context while holding another lock that the interrupt
handler might try to acquire.

---

### `pic.rs` — 8259 PIC management

#### Initialization

`pic::init()` calls `ChainedPics::initialize()` from the `pic8259` crate, which sends the
four Initialization Command Words (ICWs) to both PICs, remapping them to vectors 0x20–0x2F.

#### EOI protocol — this is where most PIC bugs live

The 8259 requires an explicit End-of-Interrupt signal after every interrupt handler
completes. Failing to send EOI causes the PIC to believe the interrupt is still in-service,
blocking all equal or lower priority IRQs indefinitely.

Three functions handle EOI:

| Function | When to call |
|----------|-------------|
| `end_of_interrupt(vector)` | Normal IRQ completion; never call for spurious |
| `eoi_if_not_spurious_master()` | IRQ7 handler only |
| `eoi_if_not_spurious_slave()` | IRQ15 handler only |

The spurious functions read the PIC's In-Service Register (ISR) via OCW3 before acting.
The ISR indicates which interrupt is currently being serviced. If the IRQ7/IRQ15 bit is
not set, the interrupt was spurious — a timing race where the CPU acknowledged an interrupt
that the PIC had already cleared. Sending EOI for a spurious interrupt corrupts PIC state.

Spurious IRQ routing rules:
- Spurious from master (IRQ7): send **no EOI** at all.
- Spurious from slave (IRQ15): send **master EOI only** (the master's cascade line did
  assert, so master must be notified; the slave must not).

#### IRQ masking

`mask_irq(irq)` and `unmask_irq(irq)` write the Interrupt Mask Register (IMR) for the
appropriate PIC. Use these to disable a noisy or unhandled device:

```rust
// Disable floppy (IRQ6) if you have no floppy driver.
interrupts::pic::mask_irq(6);
```

`read_imr()` returns the current mask as a `u16` (master in low byte, slave in high byte).
Useful for diagnostics.

---

### `dispatch.rs` — Runtime IRQ handler registration

The IDT is a static structure. Once loaded, its entries cannot change without reloading the
entire IDT. `dispatch.rs` implements a second dispatch tier for hardware IRQs: each IRQ's
IDT handler calls `dispatch::dispatch(irq)`, which looks up and calls a registered
`fn()` from a flat atomic table.

This lets drivers register at runtime without touching the IDT.

#### Registration

```rust
fn my_timer_tick() {
    // Called on every PIT interrupt, from within the IRQ0 handler frame.
    // Must not block. Must not call any function that acquires a non-reentrant lock.
    some_scheduler_tick();
}

// During driver init:
interrupts::dispatch::register(0, my_timer_tick)
    .expect("IRQ0 already registered");
```

`register` returns `Err` if the slot is occupied. One handler per IRQ line — if you need
fanout, build a small chain in your handler.

`unregister(irq)` clears the slot. There is no synchronization between `unregister` and
an in-flight call to the old handler. If the handler's associated data is about to be
freed, you must ensure the handler is not currently executing before freeing. On a
single-core system, disabling interrupts around the unregister + free sequence is
sufficient. On SMP, this requires more careful coordination.

#### Hot-path performance

`dispatch::dispatch` is inlined into each IRQ handler. The implementation:
1. Loads the slot's `AtomicPtr` with `Acquire` ordering — no lock.
2. Checks for null — branches to skip if empty.
3. Transmutes the pointer to `fn()` and calls it.

This is two to three instructions on the fast path when no handler is registered, or a
direct indirect call when one is. There is no `Mutex`, no allocation, and no conditional
compilation.

---

### `stats.rs` — Per-vector interrupt counters

A flat array of 256 `AtomicU64` values, indexed by vector number. Every handler calls
`stats::record(vector)` as its first operation.

```rust
// Read the timer interrupt count from anywhere:
let ticks = interrupts::stats::count(interrupts::vectors::PIC_IRQ_TIMER);

// Dump all non-zero counters (requires alloc):
for (vector, count) in interrupts::stats::snapshot_active() {
    println!("vector {:#04x}: {} deliveries", vector, count);
}
```

`record` uses `Relaxed` ordering — the atomic increment is sufficient for a monotonic
counter; the happens-before relationship with the handler body is established by the
CPU's interrupt delivery mechanism, not by the atomic ordering.

`count` uses `Acquire` to ensure the reading core sees all stores that preceded the
last increment.

`reset_all` is marked `unsafe` because calling it while handlers fire produces a
transient undercount. It exists for test harnesses; do not call it in production paths.

---

### `apic.rs` — APIC detection and SMP migration path

This module does not activate the APIC. It provides the infrastructure to do so when
you are ready. The 8259 PIC is adequate for a single-core kernel; APIC is required for SMP.

#### Detecting APIC presence

```rust
if interrupts::apic::apic_supported() {
    // CPU has an on-chip local APIC.
    // This is true on all post-P5 x86_64 CPUs — essentially universal.
}
```

#### Migrating from PIC to APIC

The sequence when you add SMP support:

1. Parse the ACPI MADT to get the local APIC physical base address (usually 0xFEE0_0000)
   and the I/O APIC address. This requires an ACPI table parser — not included here.

2. Map the local APIC MMIO region into virtual memory. The mapping must be uncacheable
   (cache-disable bit in the page table entry). This requires your memory mapper — not
   included here.

3. Call `apic::disable_pic()` to mask all 8259 lines. **Do this before enabling the
   APIC**, not after, to prevent spurious PIC interrupts from racing with APIC setup.
   The existing PIC IDT entries remain — they catch any residual spurious PIC interrupts
   that fire during the transition window.

4. On each CPU core, construct a `LocalApic` from the mapped virtual address and call
   `init()` with a chosen spurious vector (conventionally 0xFF):

   ```rust
   let apic = unsafe { LocalApic::new(mapped_apic_vaddr) };
   unsafe { apic.init(0xFF) };
   ```

5. Add an IDT entry for the spurious APIC vector (0xFF). The handler must **not** send
   an EOI — APIC spurious vectors are exempt from EOI. A no-op handler suffices:

   ```rust
   // In exceptions.rs, inside the IDT lazy_static:
   idt[0xFF].set_handler_fn(apic_spurious_handler);

   // In handlers.rs:
   pub extern "x86-interrupt" fn apic_spurious_handler(_frame: InterruptStackFrame) {
       stats::record(0xFF);
       // NO EOI — APIC spurious vectors are deliberately excluded.
   }
   ```

6. Update `timer_handler` to call `apic.end_of_interrupt()` instead of
   `pic::end_of_interrupt()`. The same applies to any other APIC-routed IRQ handler.

The `LocalApic` struct exposes `configure_timer` for the APIC timer (which replaces the
PIT for per-core scheduling ticks) and `send_ipi` for inter-processor interrupts. Both
require the APIC to be enabled first via `init()`.

---

## Common Integration Errors

### Triple fault on boot, no panic output

Cause: IDT loaded before GDT/TSS. The IST entries in the IDT reference TSS slots that
are not yet valid.

Fix: `gdt::init()` unconditionally before `interrupts::init()`.

### Triple fault when a double fault occurs

Cause: `DOUBLE_FAULT_IST_INDEX` references an IST slot that is zero or points to an
unmapped address.

Fix: Verify the TSS `interrupt_stack_table` entry at index `DOUBLE_FAULT_IST_INDEX` holds
a valid stack top address (high address, not base — x86 stacks grow downward). The stack
memory must be mapped and writable.

### Kernel hangs after first timer interrupt, no further interrupts

Cause: EOI was not sent, or was sent for the wrong vector. The PIC holds IRQ0 in-service
indefinitely, blocking all equal and lower priority lines.

Fix: Confirm `pic::end_of_interrupt(InterruptVector::Timer.as_u8())` is called at the
end of `timer_handler`, and that `InterruptVector::Timer.as_u8()` returns `0x20`.

### Keyboard input produces garbage or duplicate characters

Cause: Scancode read (`Port::new(0x60).read()`) must happen unconditionally regardless
of whether the key decoder produces output. If you skip the read, the PS/2 controller
holds the data register full, which prevents the next scancode from being latched, which
causes IRQ1 to re-fire immediately.

Fix: The `keyboard_handler` in `handlers.rs` already reads the scancode unconditionally
before inspecting the decoded result. Do not add an early return before the port read.

### `stats::snapshot_active` fails to compile

Cause: `alloc::vec::Vec` requires a global allocator. If your kernel has no heap, the
function does not compile.

Fix: Delete `snapshot_active` from `stats.rs`, or gate it with a cargo feature. The rest
of `stats.rs` — `record`, `count`, `reset_all` — uses only `core::sync::atomic` and
compiles without alloc.

### `dispatch::dispatch` transmutes a null pointer

Cause: This cannot happen from correct usage — the function checks for null before
transmuting. If you see this in Miri or a sanitizer, the `AtomicPtr` was written with
a null value through `register`, which validates the slot is non-null before storing.
The only route to a null dereference is if `unregister` raced with `dispatch` in a way
that produced a torn read — which `Acquire/Release` ordering prevents on x86.

---

## What This Subsystem Does Not Provide

These are explicit scope boundaries, not oversights:

**No I/O APIC driver.** The I/O APIC routes PCI and platform interrupts on modern
hardware. `apic.rs` covers the local APIC only.

**No MSI/MSI-X support.** PCIe devices use message-signalled interrupts that bypass
the I/O APIC entirely. These require PCI configuration space access and their own IDT
vector allocation.

**No interrupt affinity.** Steering specific IRQs to specific CPU cores requires I/O
APIC redirection table programming. Out of scope until the I/O APIC driver exists.

**No FPU context switching.** The `#NM` handler panics with a message to implement lazy
FPU switching. A real scheduler must save and restore `xsave` state on context switches
and clear CR0.TS to enable FPU access in the new context.

**No MCE bank decoding.** The machine check handler panics without reading the MCi_STATUS
MSRs. A production kernel would iterate MCG_CAP\[Count\] banks, decode the error type,
and potentially recover from correctable errors.

**No user-mode interrupt delivery.** All handlers assume ring 0. Delivering signals to
user-mode processes after a fault requires a separate mechanism (signal frames, upcalls,
or similar) not present here.

---

## Testing

The subsystem includes one integration test in `exceptions.rs`:

```rust
#[test_case]
fn test_breakpoint_does_not_panic() {
    x86_64::instructions::interrupts::int3();
    // Execution reaching here confirms: IDT loaded, breakpoint handler
    // returned correctly (trap semantics, not fault semantics).
}
```

This test is load-bearing — it verifies IDT initialization, handler dispatch, and
correct trap-vs-fault classification for `#BP` in a single operation. Run it first.

Additional tests worth writing:

```rust
#[test_case]
fn test_pic_irq_mask_round_trip() {
    // Mask IRQ5, verify it's masked, unmask it, verify restored.
    use interrupts::pic;
    let before = pic::read_imr();
    pic::mask_irq(5);
    let masked = pic::read_imr();
    assert!(masked & (1 << 5) != 0, "IRQ5 should be masked");
    pic::unmask_irq(5);
    let after = pic::read_imr();
    assert_eq!(before, after, "IMR should be restored");
}

#[test_case]
fn test_dispatch_register_and_unregister() {
    use interrupts::dispatch;
    static HIT: core::sync::atomic::AtomicBool =
        core::sync::atomic::AtomicBool::new(false);

    fn handler() { HIT.store(true, core::sync::atomic::Ordering::Relaxed); }

    // Use an IRQ that is masked and will not fire during the test.
    dispatch::register(5, handler).expect("register failed");
    unsafe { dispatch::dispatch(5); }
    assert!(HIT.load(core::sync::atomic::Ordering::Relaxed));
    dispatch::unregister(5);

    // Slot should now be empty.
    HIT.store(false, core::sync::atomic::Ordering::Relaxed);
    unsafe { dispatch::dispatch(5); }
    assert!(!HIT.load(core::sync::atomic::Ordering::Relaxed));
}
```

---

## Reference

Intel SDM Vol. 3A, Chapter 6 — Interrupt and Exception Handling.
Specifically: Table 6-1 (exception summary), Section 6.7 (IST mechanism), Section 6.14
(error code format).

The `x86_64` crate's IDT documentation: https://docs.rs/x86_64/latest/x86_64/structures/idt/

OSDev Wiki — "8259 PIC": https://wiki.osdev.org/8259_PIC
OSDev Wiki — "APIC": https://wiki.osdev.org/APIC
OSDev Wiki — "Interrupts": https://wiki.osdev.org/Interrupts
