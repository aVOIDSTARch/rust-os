//! Shared no_std test and output infrastructure for the crusty_os workspace.
//!
//! Provides the minimal shared plumbing that every workspace member needs for
//! kernel output and QEMU-based testing, without pulling in any hardware code:
//!
//! - [`KernelWriter`] — trait for byte-level serial/display output
//! - [`register_writer`] / [`with_writer`] — global writer registration
//! - [`register_panic_hook`] — pluggable panic behavior
//! - [`Testable`] / [`runner`] — custom test framework runner
//! - [`kprint!`] / [`kprintln!`] — formatted output macros
//!
//! `framework` depends only on `core`. Platform-specific code (UART init,
//! QEMU exit device) lives in the `platform` crate.
//!
//! # Crate feature: `panic_handler`
//!
//! When enabled, compiles a `#[panic_handler]` that delegates to the hook
//! registered via [`register_panic_hook`].
//!
//! - **Enable** in test binaries that don't own a panic handler (e.g. `bitwise`
//!   QEMU tests with `features = ["panic_handler"]` in dev-dependencies).
//! - **Do NOT enable** in `crusty_os`, which owns its own `#[panic_handler]` in
//!   `lib.rs` so that it can control the test-vs-normal-boot distinction.

#![no_std]
#![cfg_attr(test, feature(custom_test_frameworks))]

use core::fmt;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicPtr, Ordering};
use core::panic::PanicInfo;

// ── Writer ────────────────────────────────────────────────────────────────────

/// Trait for kernel output targets (UART, VGA framebuffer, etc.).
///
/// Implementors must also implement [`core::fmt::Write`] so that the standard
/// `write!` / `writeln!` formatting machinery works. The additional
/// [`write_byte`] method allows raw single-byte I/O without format overhead.
///
/// Register an implementation with [`register_writer`]; use [`with_writer`] or
/// the [`kprint!`] / [`kprintln!`] macros to emit output.
pub trait KernelWriter: fmt::Write {
    /// Write a single byte of output.
    fn write_byte(&mut self, byte: u8);
}

// UnsafeCell preserves the full fat pointer (*mut dyn KernelWriter = data ptr + vtable ptr).
// AtomicPtr<()> would silently strip the vtable on store, causing UB on method dispatch.
struct WriterCell(UnsafeCell<Option<*mut dyn KernelWriter>>);
// Safety: bare-metal single-core; init called once before any concurrent access.
unsafe impl Sync for WriterCell {}

static WRITER: WriterCell = WriterCell(UnsafeCell::new(None));

/// Register the global kernel writer.
///
/// Must be called once during platform initialization (typically from
/// `platform::init()`), before any use of [`kprint!`], [`kprintln!`], or
/// [`with_writer`].
///
/// # Safety
/// `w` must remain valid for the entire program lifetime (i.e. point to
/// static storage). Calling this more than once is unsound.
pub unsafe fn register_writer(w: &'static mut dyn KernelWriter) {
    unsafe { *WRITER.0.get() = Some(w as *mut dyn KernelWriter) };
}

/// Execute `f` with mutable access to the registered writer.
///
/// No-ops silently when no writer has been registered, so callers do not need
/// to guard against the pre-init window.
pub fn with_writer<F: FnOnce(&mut dyn KernelWriter)>(f: F) {
    unsafe {
        if let Some(ptr) = *WRITER.0.get() {
            f(&mut *ptr);
        }
    }
}

// ── Panic hook ────────────────────────────────────────────────────────────────

/// Signature of a platform panic handler registered via [`register_panic_hook`].
type PanicHook = fn(&PanicInfo) -> !;

fn default_panic(_info: &PanicInfo) -> ! {
    loop {}
}

// fn pointer is a thin pointer — AtomicPtr<()> is safe here (no vtable to lose).
static PANIC_HOOK: AtomicPtr<()> = AtomicPtr::new(default_panic as *mut ());

/// Register the platform panic handler.
///
/// The `#[panic_handler]` compiled by the `panic_handler` feature delegates
/// to this hook. Call from `platform::init()` to wire up serial output and
/// QEMU exit on panic.
///
/// The default hook (before registration) spins forever (`loop {}`).
pub fn register_panic_hook(hook: PanicHook) {
    PANIC_HOOK.store(hook as *mut (), Ordering::SeqCst);
}

/// Compiled only when the `panic_handler` feature is enabled.
///
/// Delegates to the hook registered via [`register_panic_hook`]. See the
/// crate-level documentation for when to enable this feature.
#[cfg(feature = "panic_handler")]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    let ptr = PANIC_HOOK.load(Ordering::SeqCst);
    let hook: PanicHook = unsafe { core::mem::transmute(ptr) };
    hook(info)
}

// ── Test runner ───────────────────────────────────────────────────────────────

/// Trait implemented by every test case collected by the custom test framework.
///
/// The blanket `impl<T: Fn()>` below means any zero-argument closure or bare
/// function automatically becomes a `Testable`, with its type name used as the
/// display name.
pub trait Testable {
    /// Run the test. Panics on failure.
    fn run(&self);
    /// Human-readable test name (defaults to the fully-qualified type name).
    fn name(&self) -> &'static str;
}

impl<T: Fn()> Testable for T {
    fn run(&self) {
        self();
    }
    fn name(&self) -> &'static str {
        core::any::type_name::<T>()
    }
}

/// Run all collected test cases, printing their names and `[ok]` on success.
///
/// Panics propagate upward to the `#[panic_handler]` if a test fails.
/// After this function returns, the caller is responsible for exiting QEMU
/// (e.g. via `platform::exit_success()`).
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

/// Print formatted text to the registered [`KernelWriter`] without a newline.
///
/// Behaves like `print!` from the standard library.  No-ops if no writer has
/// been registered.
///
/// # Example
/// ```ignore
/// kprint!("value = {:#x}", addr);
/// ```
#[macro_export]
macro_rules! kprint {
    ($($arg:tt)*) => {
        $crate::with_writer(|w| {
            let _ = core::fmt::write(w, format_args!($($arg)*));
        });
    };
}

/// Print formatted text to the registered [`KernelWriter`] followed by `\n`.
///
/// Behaves like `println!` from the standard library.  No-ops if no writer has
/// been registered.
///
/// # Example
/// ```ignore
/// kprintln!("boot info: {:?}", boot_info);
/// ```
#[macro_export]
macro_rules! kprintln {
    () => ($crate::kprint!("\n"));
    ($($arg:tt)*) => ($crate::kprint!("{}\n", format_args!($($arg)*)));
}
