//! x86_64 bare-metal platform glue for the crusty_os workspace.
//!
//! Wraps the `uart_16550 0.6` serial driver and the QEMU ISA debug-exit device
//! into the [`KernelWriter`] interface defined in `framework`, providing the
//! hardware side of the output/testing infrastructure.
//!
//! # Initialization
//!
//! Call [`init()`] **exactly once**, before any use of `framework` output macros
//! or QEMU exit functions.  `init()` performs three steps in order:
//! 1. Constructs [`QemuSerial`] around `0x3F8` (COM1).
//! 2. Calls [`framework::register_writer`] with the serial instance.
//! 3. Calls [`framework::register_panic_hook`] with [`platform_panic`].
//!
//! # QEMU exit device
//!
//! Uses `iobase = 0xf4` — must match the `-device isa-debug-exit,iobase=0xf4`
//! QEMU argument in `[package.metadata.bootimage]`.  The device maps writes as:
//! `host_exit_code = (value << 1) | 1`, so `0x10 → 33` (success) and
//! `0x11 → 35` (failure).

#![no_std]

use core::fmt;
use core::cell::UnsafeCell;
use uart_16550::{Config, Uart16550Tty, backend::PioBackend};
use x86_64::instructions::port::Port;
use framework::{KernelWriter, register_panic_hook, register_writer};
use core::panic::PanicInfo;

// ── QEMU exit ─────────────────────────────────────────────────────────────────

/// QEMU ISA debug-exit codes written to `iobase = 0xf4`.
///
/// QEMU maps each write as `host_exit_code = (value << 1) | 1`:
/// - `Success (0x10)` → host exit code **33**
/// - `Failure (0x11)` → host exit code **35**
///
/// `[package.metadata.bootimage] test-success-exit-code = 33` tells
/// `bootimage test` which host code means "all tests passed".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum QemuExitCode {
    /// All tests passed. Maps to host exit code 33.
    Success = 0x10,
    /// A test or assertion failed. Maps to host exit code 35.
    Failure = 0x11,
}

/// Write `code` to the QEMU ISA debug-exit port and halt execution.
///
/// # Safety
/// Performs an x86 `OUT` instruction. Safe to call only from bare-metal
/// x86_64 code running under QEMU with the matching `-device isa-debug-exit`
/// argument.
pub fn exit_qemu(code: QemuExitCode) -> ! {
    unsafe {
        let mut port: Port<u32> = Port::new(0xf4);
        port.write(code as u32);
    }
    unreachable!()
}

/// Exit QEMU with [`QemuExitCode::Success`] (host code 33).
pub fn exit_success() -> ! { exit_qemu(QemuExitCode::Success) }

/// Exit QEMU with [`QemuExitCode::Failure`] (host code 35).
pub fn exit_failure() -> ! { exit_qemu(QemuExitCode::Failure) }

// ── Serial writer ─────────────────────────────────────────────────────────────

/// UART serial output backed by `uart_16550 0.6`'s `Uart16550Tty<PioBackend>`.
///
/// Registered as the global [`KernelWriter`] during [`init()`].
/// All `framework::kprint!` / `framework::kprintln!` output flows through here.
pub struct QemuSerial {
    port: Uart16550Tty<PioBackend>,
}

// Safety: Uart16550Tty<PioBackend>: Send per uart_16550's own impl.
// Sync is asserted here because we run single-core bare-metal and init()
// is called exactly once before any other use.
unsafe impl Send for QemuSerial {}
unsafe impl Sync for QemuSerial {}

impl fmt::Write for QemuSerial {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.port.write_str(s)
    }
}

impl KernelWriter for QemuSerial {
    fn write_byte(&mut self, byte: u8) {
        self.port.inner_mut().send_bytes_exact(&[byte]);
    }
}

impl QemuSerial {
    unsafe fn new() -> Self {
        Self {
            port: unsafe {
                Uart16550Tty::new_port(0x3F8, Config::default())
                    .expect("UART init failed")
            },
        }
    }
}

// ── Static storage for the serial writer ─────────────────────────────────────

struct SerialOnce(UnsafeCell<Option<QemuSerial>>);
// Safety: access is guarded by the init-once contract; bare-metal single-core.
unsafe impl Sync for SerialOnce {}

static SERIAL: SerialOnce = SerialOnce(UnsafeCell::new(None));

// ── Platform panic hook ───────────────────────────────────────────────────────

/// Platform panic handler: prints the panic info over serial then exits QEMU.
///
/// Installed as the framework panic hook during [`init()`]. When the
/// `panic_handler` feature of `framework` is enabled (e.g. in `bitwise` QEMU
/// tests), panics are routed here automatically.
pub fn platform_panic(info: &PanicInfo) -> ! {
    framework::with_writer(|w| {
        let _ = fmt::write(w, format_args!("\n[PANIC] {}\n", info));
    });
    exit_failure()
}

// ── Init ──────────────────────────────────────────────────────────────────────

/// Initialize the platform: UART, global writer, and panic hook.
///
/// # Safety
/// Must be called **exactly once**, before any use of `framework` output macros
/// or QEMU exit functions. Calling more than once is unsound (double-init of
/// the UART and aliased `&'static mut` references).
pub unsafe fn init() {
    let slot: *mut Option<QemuSerial> = SERIAL.0.get();
    unsafe {
        *slot = Some(QemuSerial::new());
        // Extend lifetime to 'static: slot points to static storage and init
        // is called exactly once, so no other reference to this slot exists.
        let serial_ref: &mut QemuSerial = (*slot).as_mut().unwrap_unchecked();
        let serial: &'static mut QemuSerial = &mut *(serial_ref as *mut QemuSerial);
        register_writer(serial);
    }
    register_panic_hook(platform_panic);
}
