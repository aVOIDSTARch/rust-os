#![no_std]

use core::fmt;
use core::cell::UnsafeCell;
use uart_16550::{Config, Uart16550Tty, backend::PioBackend};
use x86_64::instructions::port::Port;
use framework::{KernelWriter, register_panic_hook, register_writer};
use core::panic::PanicInfo;

// ── QEMU exit ─────────────────────────────────────────────────────────────────

/// Exit codes match the iobase configured in .cargo/config.toml:
/// -device isa-debug-exit,iobase=0xf4,iosize=0x04
/// QEMU maps: (value << 1) | 1 → host exit code; 0x10 → 33, 0x11 → 35.
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

pub fn exit_success() -> ! { exit_qemu(QemuExitCode::Success) }
pub fn exit_failure() -> ! { exit_qemu(QemuExitCode::Failure) }

// ── Serial writer ─────────────────────────────────────────────────────────────

pub struct QemuSerial {
    port: Uart16550Tty<PioBackend>,
}

// Safety: Uart16550Tty<PioBackend> is Send (Uart16550<B>: Send per uart_16550).
// We assert Sync too because this is single-core bare-metal; init is called
// exactly once before any other use.
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

// ── Platform panic hook (called by framework's #[panic_handler]) ──────────────

pub fn platform_panic(info: &PanicInfo) -> ! {
    framework::with_writer(|w| {
        let _ = fmt::write(w, format_args!("\n[PANIC] {}\n", info));
    });
    exit_failure()
}

// ── Init ──────────────────────────────────────────────────────────────────────

/// Initialize QEMU serial output and register the platform panic hook.
///
/// # Safety
/// Must be called exactly once before any use of framework output macros.
pub unsafe fn init() {
    let slot: *mut Option<QemuSerial> = SERIAL.0.get();
    unsafe {
        *slot = Some(QemuSerial::new());
        // Extend to 'static: slot points to static storage, init-once contract.
        let serial_ref: &mut QemuSerial = (*slot).as_mut().unwrap_unchecked();
        let serial: &'static mut QemuSerial = &mut *(serial_ref as *mut QemuSerial);
        register_writer(serial);
    }
    register_panic_hook(platform_panic);
}
