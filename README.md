# crusty_os

A bare-metal x86_64 operating system kernel written in Rust. The project explores
kernel architecture through a layered allocator stack, dual boot-protocol support,
and a QEMU-integrated test framework.

---

## Workspace layout

```
crusty_os/       — Main kernel binary (all subsystems)
framework/       — no_std test + output infrastructure (no hardware deps)
platform/        — x86_64 hardware glue: UART, QEMU exit device
barnacle/        — Multiboot2 boot library (build.rs compiles boot.asm via NASM)
test_kernel/     — Minimal GRUB test kernel demonstrating barnacle
bitwise/         — no_std bit-manipulation utilities
xtask/           — Build/run/test automation (host binary, not a workspace member)
```

---

## Boot protocols

Three boot paths are supported, selected by Cargo feature:

| Feature            | Bootloader         | Entry symbol  | Linker script          |
|--------------------|--------------------|---------------|------------------------|
| `boot-multiboot2`  | GRUB 2 (Multiboot2)| `kernel_main` | `barnacle/kernel.ld`   |
| `boot-limine`      | Limine             | `_start`      | `crusty_os/limine.ld`  |
| `use-bootloader`   | blog_os bootloader | `kmain`       | default                |

Only one feature may be active per build.

---

## Allocator stack

For `boot-multiboot2` and `boot-limine` builds, the kernel uses a three-layer
heap:

```
Box / Vec / alloc crate
  └── TlsfAllocator     src/allocator/tlsf.rs    O(1) alloc/dealloc
        └── BuddyAllocator  src/allocator/buddy.rs   page-granularity
```

`SlabCache<T>` (`src/allocator/slab.rs`) provides typed per-object caching on top
of the buddy allocator for high-frequency fixed-size allocations.

The legacy `use-bootloader` path keeps the original `BumpAllocator`.

---

## Building

```bash
# Multiboot2 / GRUB
cargo build -p crusty_os --no-default-features --features boot-multiboot2

# Limine
cargo build -p crusty_os --no-default-features --features boot-limine

# Legacy (blog_os bootloader)
cargo build -p crusty_os
```

---

## Running

```bash
# Prerequisites
cargo xtask check-deps          # verify nasm, qemu-system-x86_64, grub-mkrescue/docker

# GRUB (test_kernel)
cargo xtask run

# crusty_os via GRUB
cargo xtask run --boot multiboot2

# crusty_os via Limine
cargo xtask run --boot limine
```

On macOS, `grub-mkrescue` is unavailable natively; xtask falls back to Docker
automatically when Docker is installed.

---

## Testing

All tests run on bare-metal x86_64 inside QEMU via the `bootimage` test runner.
Test output goes to the serial port (standard I/O in QEMU); pass/fail is
signalled through the ISA debug-exit device at port `0xf4`.

### Run the full test suite

```bash
cargo xtask test
```

This is equivalent to `cargo test -p crusty_os`.  Each test binary is compiled,
wrapped in a boot image, and executed in QEMU.  QEMU exits with code **33** on
success and **35** on failure (configured in `crusty_os/Cargo.toml`).

### Run a specific test binary

```bash
cargo xtask test --test heap_stress
cargo xtask test --test interrupt_handlers
```

### Test suite overview

| Test binary          | Harness | What it covers                                      |
|----------------------|---------|-----------------------------------------------------|
| `basic_boot`         | default | VGA `println!` and serial output after minimal boot |
| `heap_allocation`    | default | `Box`, `Vec`, allocator free-list reuse             |
| `heap_stress`        | default | `String`, `Rc`, nested `Vec`, bulk alloc/dealloc    |
| `interrupt_handlers` | default | Breakpoint trap, interrupt enable/disable lifecycle |
| `should_panic`       | none    | Panic path: `assert_eq!(0,1)` triggers expected exit|
| `stack_overflow`     | none    | Double-fault handler fires on infinite recursion    |

Unit tests (`#[test_case]`) inside source modules:

| Module                      | Tests | What they cover                              |
|-----------------------------|-------|----------------------------------------------|
| `src/allocator/buddy.rs`    | 7     | alloc, dealloc, coalescing, OOM, stats       |
| `src/interrupts/exceptions.rs` | 1  | Breakpoint handled as trap                   |
| `src/vga_buffer.rs`         | 3     | VGA write without panic                      |
| `bitwise/…`                 | 18    | Bit arithmetic, address alignment, endianness|

### Testing framework design

```
framework  (no hardware)
  ├── Testable trait — blanket impl for Fn()
  ├── runner()       — prints name + [ok] per test
  ├── KernelWriter   — output trait (VGA, serial)
  └── register_panic_hook — pluggable panic handler

platform   (hardware)
  ├── QemuSerial     — UART at COM1 (0x3F8), implements KernelWriter
  └── exit_qemu()    — writes to ISA debug-exit device at 0xf4

crusty_os  (integration)
  ├── test_runner()  — calls framework::runner then platform::exit_success
  └── test_panic_handler() — serial output then platform::exit_failure
```

Each integration test is a standalone `no_std` binary that:
1. Calls `platform::init()` (UART + writer + panic hook)
2. Optionally calls `crusty_os::init()` (GDT + IDT + PIC)
3. Optionally initializes the heap via `allocator::init_heap`
4. Calls the generated `test_main()` which invokes `framework::runner`
5. Exits via `platform::exit_success()` after all tests pass

---

## Project structure (key files)

```
crusty_os/src/
├── main.rs              — Entry points for all three boot protocols
├── lib.rs               — Kernel subsystem init + test harness wiring
├── gdt.rs               — GDT, TSS, IST stack allocation
├── memory.rs            — OffsetPageTable + BootInfoFrameAllocator (use-bootloader)
├── allocator/
│   ├── mod.rs           — Global allocator selection + legacy init_heap
│   ├── buddy.rs         — Binary buddy page allocator
│   ├── slab.rs          — Typed slab cache backed by buddy
│   ├── tlsf.rs          — Two-Level Segregated Fit (GlobalAlloc impl)
│   ├── bump.rs          — Bump allocator (legacy use-bootloader path)
│   └── linked_list.rs   — Linked-list allocator (reference, unused)
├── boot/
│   ├── mod.rs           — KernelBootInfo + mutual-exclusion guards
│   ├── multiboot2.rs    — GRUB/Multiboot2 adapter → KernelBootInfo
│   └── limine.rs        — Limine adapter → KernelBootInfo
└── interrupts/
    ├── mod.rs           — Subsystem init (IDT + PIC)
    ├── exceptions.rs    — IDT construction (CPU exceptions + PIC IRQs)
    ├── handlers.rs      — Individual exception/IRQ handler functions
    ├── pic.rs           — 8259A PIC management + EOI helpers
    ├── vectors.rs       — Vector number constants + InterruptVector enum
    ├── apic.rs          — Local APIC stub (detection + skeleton)
    ├── dispatch.rs      — IRQ dispatch table
    └── stats.rs         — Per-IRQ statistics counters
```

---

## Prerequisites

| Tool                | Required for              | Install                              |
|---------------------|---------------------------|--------------------------------------|
| Rust nightly        | All builds                | `rustup install nightly`             |
| nasm                | boot-multiboot2           | `brew install nasm` / `apt install nasm` |
| qemu-system-x86_64  | Running and testing       | `brew install qemu` / `apt install qemu-system-x86` |
| grub-mkrescue       | GRUB ISO (macOS: Docker)  | `apt install grub-pc-bin xorriso`    |
| limine + xorriso    | Limine ISO (optional)     | `brew install limine xorriso`        |
| docker              | macOS ISO fallback        | docker.com                           |
