// Serial output now provided by platform::QemuSerial via framework.
// These macros are kept for backwards compatibility with integration tests
// that import crusty_os::{serial_print!, serial_println!}.

#[macro_export]
macro_rules! serial_print {
    ($($arg:tt)*) => { framework::kprint!($($arg)*) };
}

#[macro_export]
macro_rules! serial_println {
    () => (framework::kprintln!());
    ($($arg:tt)*) => (framework::kprintln!($($arg)*));
}
