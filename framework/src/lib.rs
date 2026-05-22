#![no_std]
#![cfg_attr(test, feature(custom_test_frameworks))]

use core::fmt;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicPtr, Ordering};
use core::panic::PanicInfo;

// ── Writer trait ──────────────────────────────────────────────────────────────

pub trait KernelWriter: fmt::Write {
    fn write_byte(&mut self, byte: u8);
}

// UnsafeCell preserves the full fat pointer (*mut dyn KernelWriter = data ptr + vtable ptr).
// AtomicPtr<()> would silently strip the vtable on cast, causing UB when methods are called.
struct WriterCell(UnsafeCell<Option<*mut dyn KernelWriter>>);
unsafe impl Sync for WriterCell {}

static WRITER: WriterCell = WriterCell(UnsafeCell::new(None));

/// Register the global writer. Call once during platform init.
/// # Safety
/// The reference must remain valid for the lifetime of the program.
pub unsafe fn register_writer(w: &'static mut dyn KernelWriter) {
    unsafe { *WRITER.0.get() = Some(w as *mut dyn KernelWriter) };
}

/// Execute a closure with mutable access to the registered writer.
/// No-ops silently if no writer has been registered.
pub fn with_writer<F: FnOnce(&mut dyn KernelWriter)>(f: F) {
    unsafe {
        if let Some(ptr) = *WRITER.0.get() {
            f(&mut *ptr);
        }
    }
}

// ── Panic hook ────────────────────────────────────────────────────────────────

type PanicHook = fn(&PanicInfo) -> !;

fn default_panic(_info: &PanicInfo) -> ! {
    loop {}
}

// fn pointer is a thin pointer — AtomicPtr<()> is safe here (no vtable to lose).
static PANIC_HOOK: AtomicPtr<()> = AtomicPtr::new(default_panic as *mut ());

/// Register a custom panic behavior. The framework's #[panic_handler]
/// (when enabled via feature) delegates to this.
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

pub trait Testable {
    fn run(&self);
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
