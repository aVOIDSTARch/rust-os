//! Serial output macros for crusty_os.
//!
//! `serial_print!` and `serial_println!` delegate to [`framework::kprint!`] /
//! [`framework::kprintln!`], which write through the `platform::QemuSerial`
//! registered during [`platform::init()`].
//!
//! These macros are kept for backwards compatibility with existing integration
//! tests that `use crusty_os::{serial_print, serial_println}`.  New code
//! should prefer `framework::kprint!` / `framework::kprintln!` directly.

/// Write formatted text to the serial port without a trailing newline.
///
/// Forwards to [`framework::kprint!`]. Output appears on the QEMU `-serial stdio`
/// stream, which `bootimage test` captures and displays on the host terminal.
///
/// # Example
/// ```ignore
/// serial_print!("result = {:#010x}", value);
/// ```
#[macro_export]
macro_rules! serial_print {
    ($($arg:tt)*) => { framework::kprint!($($arg)*) };
}

/// Write formatted text to the serial port followed by a newline.
///
/// Forwards to [`framework::kprintln!`].
///
/// # Example
/// ```ignore
/// serial_println!("heap at {:#x}", heap_start);
/// ```
#[macro_export]
macro_rules! serial_println {
    () => (framework::kprintln!());
    ($($arg:tt)*) => (framework::kprintln!($($arg)*));
}
