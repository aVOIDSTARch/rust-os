// interrupts/dispatch.rs
//
// Runtime IRQ dispatch table.
//
// The x86_64 IDT is a static structure: you cannot change a handler
// after the IDT is loaded without reloading it.  For hardware IRQs
// (vectors 0x20–0x2F) this module provides a second-level dispatch
// table — the IDT entries call a single "trampoline" per vector, which
// then calls the registered handler from this table.
//
// This allows drivers to register (and unregister) IRQ handlers at
// runtime without touching the IDT.
//
// Design constraints:
//   - Handler slots are `Option<fn()>` — a simple function pointer.
//     No trait objects, no heap allocation.
//   - A `spin::Mutex` guards registration; the hot-path read uses a raw
//     atomic pointer load so that registered handlers can be called
//     without acquiring the lock.  This is safe because:
//       * We only ever store valid function pointers or null.
//       * Reads happen after an Acquire fence (interrupt delivery on x86
//         implies a full memory barrier for the handler's core).
//   - Slots 0–15 correspond to IRQ0–IRQ15 (PIC lines).
//     Slot 16 is reserved for future software/APIC vectors.

use core::sync::atomic::{AtomicPtr, Ordering};
use spin::Mutex;

const IRQ_COUNT: usize = 16;

/// Type alias for an IRQ handler function.
/// Must be `extern "x86-interrupt"`-compatible in practice, but because we
/// call it from an already-entered interrupt frame, a plain `fn()` suffices.
pub type IrqHandler = fn();

// AtomicPtr<()> stores function pointer bits without UB.
// We transmute on load/store — see safety comments below.
static HANDLERS: [AtomicPtr<()>; IRQ_COUNT] = {
    #[allow(clippy::declare_interior_mutable_const)]
    const NULL: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());
    [NULL; IRQ_COUNT]
};

// Guards registration mutations (not the hot-path dispatch).
static REG_LOCK: Mutex<()> = Mutex::new(());

// ---------------------------------------------------------------------------
// Registration API
// ---------------------------------------------------------------------------

/// Register `handler` for `irq` (0–15).
///
/// Returns `Err` if the slot is already occupied.  Callers that need
/// shared ownership of an IRQ line must implement their own fanout.
pub fn register(irq: u8, handler: IrqHandler) -> Result<(), &'static str> {
    if irq as usize >= IRQ_COUNT {
        return Err("IRQ number out of range (0–15 only)");
    }

    let _guard = REG_LOCK.lock();

    let slot = &HANDLERS[irq as usize];
    if !slot.load(Ordering::Acquire).is_null() {
        return Err("IRQ slot already registered");
    }

    // SAFETY: IrqHandler is a non-null function pointer; casting to *mut ()
    // preserves the address bits.  We restore it on load below.
    let ptr = handler as *mut ();
    slot.store(ptr, Ordering::Release);
    Ok(())
}

/// Unregister any handler for `irq`.
///
/// After this returns, the slot is empty.  Any in-flight call to the
/// old handler may still be executing — callers are responsible for
/// synchronization if the handler references data that will be freed.
pub fn unregister(irq: u8) {
    if irq as usize >= IRQ_COUNT {
        return;
    }
    let _guard = REG_LOCK.lock();
    HANDLERS[irq as usize].store(core::ptr::null_mut(), Ordering::Release);
}

// ---------------------------------------------------------------------------
// Dispatch (called from IDT trampolines)
// ---------------------------------------------------------------------------

/// Dispatch to the registered handler for `irq`, if any.
///
/// Returns `true` if a handler was called, `false` if the slot was empty.
///
/// # Safety
/// Must be called from within an interrupt handler frame for `irq`.
#[inline]
pub unsafe fn dispatch(irq: u8) -> bool {
    let ptr = HANDLERS[irq as usize].load(Ordering::Acquire);
    if ptr.is_null() {
        return false;
    }
    // SAFETY: We only store valid IrqHandler pointers; null was filtered above.
    let handler: IrqHandler = unsafe { core::mem::transmute(ptr) };
    handler();
    true
}
