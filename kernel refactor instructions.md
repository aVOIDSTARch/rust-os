# Kernel Project Refactor: Agent Instructions

## Mission

Refactor an existing Rust bare-metal kernel project into a Cargo workspace that extracts
`no_std` testing infrastructure into reusable, independently composable crates. The result
must allow `cargo test` to work on any member crate without modifying the others, without
breaking the kernel’s existing functionality, and without duplicating any hardware-interface
or test-runner code.

-----

## Current State (What You Will Find)

The root directory contains two sibling crates:

```
/
├── kernel/          # bare-metal x86_64 kernel — currently owns everything
│   ├── Cargo.toml
│   └── src/
│       ├── main.rs          # entry point, panic handler, test entry
│       ├── serial.rs        # UART 16550 writer — raw port I/O
│       ├── vga_buffer.rs    # optional VGA text writer
│       └── (other modules)
└── bitwise/         # no_std logic crate — bit manipulation utilities
    ├── Cargo.toml
    └── src/
        └── lib.rs
```

The kernel currently contains:

- `#[panic_handler]` — defined directly in `main.rs` or a dedicated module
- Serial UART output — a `SerialPort` wrapper writing to `0x3F8`
- QEMU exit logic — port I/O to `0xf4` with hardcoded exit codes
- Custom test runner — `#![feature(custom_test_frameworks)]` and `#![test_runner(...)]`
  wired directly inside the kernel
- Test entry point — `#[no_mangle] pub extern "C" fn _start()` gated on `#[cfg(test)]`

The `bitwise` crate currently has **no test infrastructure** and cannot run `cargo test`.

-----

## Target State (What You Are Building)

```
/
├── Cargo.toml           # NEW — workspace root
├── framework/           # NEW — traits, runner, registration API
│   ├── Cargo.toml
│   └── src/
│       └── lib.rs
├── platform/            # NEW — QEMU-specific concrete implementations
│   ├── Cargo.toml
│   └── src/
│       └── lib.rs
├── kernel/              # MODIFIED — stripped of test/serial/panic infra
│   ├── Cargo.toml
│   └── src/
│       ├── main.rs
│       └── (other modules, serial.rs removed or gutted)
└── bitwise/             # MODIFIED — gains cargo test capability
    ├── Cargo.toml
    └── src/
        └── lib.rs
```

### Dependency Graph (strict — no inversions)

```
framework  ←  platform  ←  kernel
                    ↑
               bitwise (dev-dep only)
```

- `framework` depends on nothing but `core`
- `platform` depends on `framework` + `uart_16550` + `x86_64`
- `kernel` depends on `framework` + `platform`
- `bitwise` depends on `framework` + `platform` (dev-dependency only)

-----

## Step 1 — Create the Workspace Root

Create `/Cargo.toml`:

```toml
[workspace]
members = [
    "framework",
    "platform",
    "kernel",
    "bitwise",
]
resolver = "3"
```

> **Requirement:** Resolver `"3"` and edition `"2024"` require Rust 1.85 or later. Verify
> with `rustc --version` before proceeding. If the toolchain is older, update via
> `rustup update stable` or pin a nightly that includes 1.85+. Resolver 3 changes
> feature unification semantics — dev-dependencies and their features are no longer unified
> with normal dependencies across the workspace, which is exactly what this project needs:
> `framework/panic_handler` must not bleed from `bitwise`’s dev-deps into `kernel`’s
> normal build. Resolver 3 enforces this correctly without manual workarounds.

Do not add a `[package]` section. This is a pure workspace manifest.

-----

## Step 2 — Create the `framework` Crate

### `/framework/Cargo.toml`

```toml
[package]
name = "framework"
version = "0.1.0"
edition = "2024"

[features]
default = []
panic_handler = []   # enables the #[panic_handler] symbol — opt-in only

[dependencies]
# none — only core
```

### `/framework/src/lib.rs`

```rust
#![no_std]
#![cfg_attr(test, feature(custom_test_frameworks))]

use core::fmt;
use core::sync::atomic::{AtomicPtr, Ordering};

// ── Writer trait ──────────────────────────────────────────────────────────────

/// Anything capable of receiving kernel output.
/// Implement this on your concrete serial/VGA type.
pub trait KernelWriter: fmt::Write + Send + Sync {
    fn write_byte(&mut self, byte: u8);
}

static WRITER: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());

/// Register the global writer. Call this once during platform init.
/// # Safety
/// The reference must remain valid for the lifetime of the program.
pub unsafe fn register_writer(w: &'static mut dyn KernelWriter) {
    WRITER.store(w as *mut dyn KernelWriter as *mut (), Ordering::SeqCst);
}

/// Execute a closure with mutable access to the registered writer.
/// No-ops silently if no writer has been registered.
pub fn with_writer<F: FnOnce(&mut dyn KernelWriter)>(f: F) {
    let ptr = WRITER.load(Ordering::SeqCst) as *mut dyn KernelWriter;
    if !ptr.is_null() {
        unsafe { f(&mut *ptr) };
    }
}

// ── Panic hook ────────────────────────────────────────────────────────────────

use core::panic::PanicInfo;

type PanicHook = fn(&PanicInfo) -> !;

static PANIC_HOOK: AtomicPtr<()> = AtomicPtr::new(default_panic as *mut ());

fn default_panic(_info: &PanicInfo) -> ! {
    loop {}   // bare fallback: spin
}

/// Register a custom panic behavior. The framework's #[panic_handler]
/// (when enabled via feature) will delegate to this function.
pub fn register_panic_hook(hook: PanicHook) {
    PANIC_HOOK.store(hook as *mut (), Ordering::SeqCst);
}

/// Only compiled when the consumer enables feature = "panic_handler".
/// The kernel must NOT enable this feature — it owns its own #[panic_handler].
/// Test binaries for other crates (bitwise, etc.) SHOULD enable it.
#[cfg(feature = "panic_handler")]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    let ptr = PANIC_HOOK.load(Ordering::SeqCst);
    let hook: PanicHook = unsafe { core::mem::transmute(ptr) };
    hook(info)
}

// ── Test runner ───────────────────────────────────────────────────────────────

/// Implement this on types that represent a runnable test.
pub trait Testable {
    fn run(&self);
    fn name(&self) -> &'static str;
}

/// The test runner. Point #![test_runner] at this.
pub fn runner(tests: &[&dyn Testable]) {
    with_writer(|w| {
        let _ = fmt::write(w, format_args!("\nRunning {} test(s)\n", tests.len()));
    });

    for test in tests {
        with_writer(|w| {
            let _ = fmt::write(w, format_args!("  {}...", test.name()));
        });
        test.run();
        with_writer(|w| {
            let _ = fmt::write(w, format_args!("[ok]\n"));
        });
    }
}

// ── Print macros ──────────────────────────────────────────────────────────────

#[macro_export]
macro_rules! kprint {
    ($($arg:tt)*) => {
        $crate::with_writer(|w| {
            let _ = core::fmt::write(w, format_args!($($arg)*));
        });
    };
}

#[macro_export]
macro_rules! kprintln {
    () => ($crate::kprint!("\n"));
    ($($arg:tt)*) => ($crate::kprint!("{}\n", format_args!($($arg)*)));
}
```

-----

## Step 3 — Create the `platform` Crate

### `/platform/Cargo.toml`

```toml
[package]
name = "platform"
version = "0.1.0"
edition = "2024"

[dependencies]
framework = { path = "../framework" }
uart_16550 = "0.3"
x86_64 = "0.15"
```

### `/platform/src/lib.rs`

```rust
#![no_std]

use core::fmt;
use uart_16550::SerialPort;
use x86_64::instructions::port::Port;
use framework::{KernelWriter, register_panic_hook, register_writer};
use core::panic::PanicInfo;

// ── QEMU exit ─────────────────────────────────────────────────────────────────

/// Exit codes must match the iobase configured in your QEMU runner args.
/// See .cargo/config.toml: -device isa-debug-exit,iobase=0xf4,iosize=0x04
/// QEMU maps: (value << 1) | 1 → process exit code
/// 0x10 → 33 (success), 0x11 → 35 (failure)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum QemuExitCode {
    Success = 0x10,
    Failure = 0x11,
}

pub fn exit_qemu(code: QemuExitCode) -> ! {
    unsafe {
        let mut port: Port<u32> = Port::new(0xf4);
        port.write(code as u32);
    }
    unreachable!()
}

pub fn exit_success() -> ! {
    exit_qemu(QemuExitCode::Success)
}

pub fn exit_failure() -> ! {
    exit_qemu(QemuExitCode::Failure)
}

// ── Serial writer ─────────────────────────────────────────────────────────────

pub struct QemuSerial {
    port: SerialPort,
}

impl QemuSerial {
    /// # Safety
    /// Must only be called once. 0x3F8 is COM1.
    pub unsafe fn new() -> Self {
        let mut port = SerialPort::new(0x3F8);
        port.init();
        Self { port }
    }
}

impl fmt::Write for QemuSerial {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.port.write_str(s).map_err(|_| fmt::Error)
    }
}

impl KernelWriter for QemuSerial {
    fn write_byte(&mut self, byte: u8) {
        self.port.send(byte);
    }
}

// ── Static storage for the serial writer ─────────────────────────────────────
// Uses a spin-based once-cell pattern to avoid heap dependency.

static mut SERIAL: Option<QemuSerial> = None;

// ── Init ──────────────────────────────────────────────────────────────────────

/// Initialize QEMU serial output and register the platform panic hook.
/// Call this as the first thing in _start() or kernel_main().
///
/// # Safety
/// Must be called exactly once before any use of framework output macros.
pub unsafe fn init() {
    SERIAL = Some(QemuSerial::new());
    if let Some(ref mut serial) = SERIAL {
        register_writer(serial);
    }
    register_panic_hook(platform_panic);
}

fn platform_panic(info: &PanicInfo) -> ! {
    framework::with_writer(|w| {
        let _ = fmt::write(w, format_args!("\n[PANIC] {}\n", info));
    });
    exit_failure()
}
```

-----

## Step 4 — Modify the `kernel` Crate

### `/kernel/Cargo.toml`

Remove `uart_16550` and any serial/test-related dependencies. Add:

```toml
[dependencies]
framework = { path = "../framework" }
platform  = { path = "../platform" }
# keep your existing kernel dependencies (x86_64, bootloader, etc.)

# NOTE: do NOT add features = ["panic_handler"] to framework here.
# The kernel owns its own #[panic_handler].
```

### `/kernel/src/main.rs` — changes only

Remove:

- Any existing `#[panic_handler]` fn → replace with delegation (see below)
- Any serial port init code → replace with `platform::init()`
- Any QEMU exit port writes → replace with `platform::exit_success()` / `exit_failure()`
- Any `#![test_runner(...)]` pointing at a local fn → point at `framework::runner`
- Any local `Testable` trait definition

Add:

```rust
#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(framework::runner)]
#![reexport_test_harness_main = "test_main"]

use framework::{kprintln, register_panic_hook};
use core::panic::PanicInfo;

// The kernel owns the panic handler symbol.
// In test mode it delegates to the platform hook (already registered by platform::init()).
// In normal mode it can do whatever the kernel needs (halt, log, etc.).
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    kprintln!("\n[KERNEL PANIC] {}", info);
    platform::exit_failure()
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    unsafe { platform::init(); }

    #[cfg(test)]
    test_main();

    kernel_main()
}

fn kernel_main() -> ! {
    kprintln!("Kernel booted.");
    loop {}
}
```

### `/kernel/src/serial.rs`

If this file exists solely to provide a serial writer, **delete it**. The serial implementation
now lives in `platform`. If it contains other logic, extract and keep only what is unrelated
to the UART hardware.

-----

## Step 5 — Modify the `bitwise` Crate

### `/bitwise/Cargo.toml`

```toml
[package]
name = "bitwise"
version = "0.1.0"
edition = "2024"

[dependencies]
# none — pure no_std logic

[dev-dependencies]
framework = { path = "../framework", features = ["panic_handler"] }
platform  = { path = "../platform" }

# Required for no_std test binary linking:
[profile.test]
# If your target needs it:
# panic = "abort"
```

### `/bitwise/src/lib.rs` — add test scaffolding

At the top of the file, add (inside `#[cfg(test)]` guards where appropriate):

```rust
#![no_std]
#![cfg_attr(test, no_main)]
#![cfg_attr(test, feature(custom_test_frameworks))]
#![cfg_attr(test, test_runner(framework::runner))]
#![cfg_attr(test, reexport_test_harness_main = "test_main")]

// Test entry point — the irreducible minimum
#[cfg(test)]
#[no_mangle]
pub extern "C" fn _start() -> ! {
    unsafe { platform::init(); }
    test_main();
    platform::exit_success()
}

// Example test
#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn test_something() {
        // your assertions here
        // on panic: framework panic_handler fires → platform_panic → exit_failure
    }
}
```

-----

## Step 6 — Workspace `.cargo/config.toml`

Create or update `/.cargo/config.toml` (workspace root):

```toml
[target.x86_64-unknown-none]
runner = """
    qemu-system-x86_64
        -device isa-debug-exit,iobase=0xf4,iosize=0x04
        -serial stdio
        -display none
"""

rustflags = ["-C", "target-feature=+sse,+sse2"]
```

The `iobase=0xf4` here is the single source of truth that corresponds to `Port::new(0xf4)`
in `platform/src/lib.rs`. If you change one, change both.

-----

## Invariants to Preserve (Do Not Break These)

1. **`framework` must never depend on `platform`** — the dependency only flows the other way.
   If you find yourself importing platform types into framework, you have the architecture
   inverted.
1. **`kernel` must never enable `framework/panic_handler`** — the kernel owns `#[panic_handler]`.
   Enabling that feature in the kernel will cause a duplicate symbol linker error.
1. **`bitwise` in non-test builds must not pull in `platform`** — it is a `dev-dependency` only.
   The `bitwise` crate’s production code must remain pure logic with zero hardware coupling.
1. **`platform::init()` is called exactly once per binary** — both the kernel entry point
   and each test binary’s `_start` call it. It must not be called more than once; the static
   `SERIAL` initialization is not idempotent.
1. **Exit codes must match QEMU args** — `QemuExitCode::Success = 0x10` and the runner flag
   `-device isa-debug-exit,iobase=0xf4,iosize=0x04` are coupled. QEMU computes the host
   exit code as `(written_value << 1) | 1`, so `0x10` → `33` and `0x11` → `35`. If your
   CI checks for exit code 0 you will need to wrap the QEMU invocation in a script that
   maps 33 → 0.
1. **`#![reexport_test_harness_main = "test_main"]` is required** — without it the generated
   harness entry is named `main`, which conflicts with `#![no_main]` kernels.

-----

## Verification Sequence

Run these in order. Each must pass before proceeding to the next.

```bash
# 1. Framework compiles in isolation
cargo build -p framework

# 2. Platform compiles
cargo build -p platform

# 3. Kernel builds (no regressions)
cargo build -p kernel --target x86_64-unknown-none

# 4. Bitwise builds in library mode (no hardware deps leak in)
cargo build -p bitwise --target x86_64-unknown-none

# 5. Bitwise tests run in QEMU
cargo test -p bitwise --target x86_64-unknown-none

# 6. Kernel tests run in QEMU
cargo test -p kernel --target x86_64-unknown-none
```

If step 5 fails with a linker error about duplicate `panic_handler` symbols, the kernel’s
`#[panic_handler]` is leaking into the bitwise test binary — check that `bitwise` does not
transitively depend on `kernel`.

If step 5 fails with a missing `panic_handler` symbol, the `framework/panic_handler` feature
is not enabled in `bitwise`’s dev-dependencies. Verify the `Cargo.toml` entry includes
`features = ["panic_handler"]`.

-----

## What NOT to Do

- Do not move `serial.rs` from kernel into `framework`. Framework must remain hardware-agnostic.
- Do not make `bitwise` depend on `kernel`. That direction is permanently closed.
- Do not put a `#[panic_handler]` in `platform`. Platform provides a *hook*, not the symbol.
- Do not use `std` anywhere. Every crate in this workspace targets bare metal.
- Do not add `x86_64` or `uart_16550` as dependencies of `framework`. Those belong in `platform`.
- Do not skip the `cfg_attr(test, ...)` wrappers in `bitwise/src/lib.rs` — without them,
  the `custom_test_frameworks` feature attribute will apply to non-test builds and cause
  compilation failures on stable or in contexts where the feature is unavailable.