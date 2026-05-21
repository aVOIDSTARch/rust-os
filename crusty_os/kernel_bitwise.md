# Kernel Bitwise Utility Functions

> **Scope**: Production-grade Rust bitwise primitives for x86_64 kernel development. All functions are `const`-capable where the operation permits it, `#[inline(always)]` for zero-cost use in hot paths, and written to be agnostic to specific page/frame sizes so they generalize across 4 KiB standard pages, 2 MiB huge pages, 1 GiB gigantic pages, and arbitrary DMA buffer alignments.
>
> **Conventions**:
> - `PhysAddr` / `VirtAddr` are `u64` newtypes — treat the raw aliases here as `u64` for clarity.
> - All alignment arguments **must** be powers of two. Functions that require this are marked `# Panics` in debug builds; in release they invoke undefined behavior if violated (matching the contract of the Linux and seL4 kernels).
> - Volatile reads/writes use `core::ptr::read_volatile` / `write_volatile` — the compiler is explicitly forbidden from reordering or eliding them.

---

## Module Layout

```
kernel_bitwise/
├── align.rs       — address alignment, page/frame arithmetic
├── bits.rs        — general-purpose bit manipulation
├── flags.rs       — flag registers and bitfield insertion/extraction
├── mmio.rs        — memory-mapped I/O (volatile read/write)
├── dma.rs         — DMA buffer alignment and descriptor helpers
├── cache.rs       — cache-line arithmetic
└── paging.rs      — page table entry encoding and decoding
```

---

## 1. Address Alignment (`align.rs`)

The foundation of every other module. These six functions cover every alignment operation you will encounter.

```rust
/// Round `addr` DOWN to the nearest multiple of `align`.
///
/// Equivalent to `addr - (addr % align)` when `align` is a power of two,
/// but implemented with a single AND and a NOT — no division instruction.
///
/// # Panics (debug)
/// Panics if `align` is zero or not a power of two.
#[inline(always)]
pub const fn align_down(addr: u64, align: u64) -> u64 {
    debug_assert!(align.is_power_of_two(), "align must be a power of two");
    addr & !(align - 1)
}

/// Round `addr` UP to the nearest multiple of `align`.
///
/// If `addr` is already aligned, it is returned unchanged.
///
/// # Panics (debug)
/// Panics if `align` is zero or not a power of two.
#[inline(always)]
pub const fn align_up(addr: u64, align: u64) -> u64 {
    debug_assert!(align.is_power_of_two(), "align must be a power of two");
    align_down(addr.wrapping_add(align - 1), align)
}

/// Returns `true` if `addr` is aligned to `align` bytes.
///
/// Equivalent to `addr % align == 0` for power-of-two alignments,
/// but reduces to a single AND instruction.
#[inline(always)]
pub const fn is_aligned(addr: u64, align: u64) -> bool {
    debug_assert!(align.is_power_of_two(), "align must be a power of two");
    (addr & (align - 1)) == 0
}

/// Returns the number of bytes between `addr` and the next aligned boundary.
///
/// Returns 0 if already aligned. The result is in [0, align).
#[inline(always)]
pub const fn align_offset(addr: u64, align: u64) -> u64 {
    debug_assert!(align.is_power_of_two(), "align must be a power of two");
    addr & (align - 1)
}

/// Compute the frame number for a physical address given a frame size.
///
/// For 4 KiB pages:  frame_number(0x8001_2000, 0x1000) = 0x8_0012
/// For 2 MiB pages:  frame_number(0x4000_0000, 0x20_0000) = 0x200
#[inline(always)]
pub const fn frame_number(phys_addr: u64, frame_size: u64) -> u64 {
    debug_assert!(frame_size.is_power_of_two(), "frame_size must be a power of two");
    phys_addr >> frame_size.trailing_zeros()
}

/// Reconstruct the base physical address from a frame number and frame size.
///
/// Inverse of `frame_number`.
#[inline(always)]
pub const fn frame_base(frame_num: u64, frame_size: u64) -> u64 {
    debug_assert!(frame_size.is_power_of_two(), "frame_size must be a power of two");
    frame_num << frame_size.trailing_zeros()
}

/// Returns the byte offset of `addr` within its containing frame.
///
/// Equivalent to `addr % frame_size` for power-of-two frame sizes.
#[inline(always)]
pub const fn frame_offset(addr: u64, frame_size: u64) -> u64 {
    debug_assert!(frame_size.is_power_of_two(), "frame_size must be a power of two");
    addr & (frame_size - 1)
}

/// Compute how many frames are needed to cover `byte_count` bytes.
///
/// Rounds up: one byte into the second frame counts as two frames.
#[inline(always)]
pub const fn frames_needed(byte_count: u64, frame_size: u64) -> u64 {
    debug_assert!(frame_size.is_power_of_two(), "frame_size must be a power of two");
    // align_up then shift avoids any multiplication
    align_up(byte_count, frame_size) >> frame_size.trailing_zeros()
}

/// Return `true` if the range [addr, addr + size) is entirely within a single frame.
#[inline(always)]
pub const fn fits_in_frame(addr: u64, size: u64, frame_size: u64) -> bool {
    debug_assert!(frame_size.is_power_of_two(), "frame_size must be a power of two");
    frame_number(addr, frame_size) == frame_number(addr + size - 1, frame_size)
}
```

**Usage examples**:

```rust
const PAGE_4K: u64 = 0x1000;
const PAGE_2M: u64 = 0x20_0000;
const PAGE_1G: u64 = 0x4000_0000;

// Align a bump allocator pointer to the next 4 KiB page
let next_page = align_up(bump_ptr, PAGE_4K);

// Extract the page frame number from a physical address
let pfn = frame_number(0xDEAD_B000, PAGE_4K);  // = 0xDEAD_B

// How many 2 MiB pages does a 5 MiB buffer need?
let count = frames_needed(5 * 1024 * 1024, PAGE_2M);  // = 3
```

---

## 2. General-Purpose Bit Manipulation (`bits.rs`)

```rust
/// Set bit `n` in `value`. Bit 0 is the least significant.
#[inline(always)]
pub const fn bit_set(value: u64, n: u32) -> u64 {
    debug_assert!(n < 64, "bit index out of range");
    value | (1u64 << n)
}

/// Clear bit `n` in `value`.
#[inline(always)]
pub const fn bit_clear(value: u64, n: u32) -> u64 {
    debug_assert!(n < 64, "bit index out of range");
    value & !(1u64 << n)
}

/// Toggle bit `n` in `value`.
#[inline(always)]
pub const fn bit_toggle(value: u64, n: u32) -> u64 {
    debug_assert!(n < 64, "bit index out of range");
    value ^ (1u64 << n)
}

/// Test whether bit `n` is set. Returns `true` if set.
#[inline(always)]
pub const fn bit_test(value: u64, n: u32) -> bool {
    debug_assert!(n < 64, "bit index out of range");
    (value >> n) & 1 == 1
}

/// Extract a contiguous bit field from bits `[high:low]` inclusive.
///
/// The returned value is right-justified (i.e., shifted to start at bit 0).
///
/// # Example
/// ```
/// // Extract the privilege level (bits 13:12) of an x86 segment selector
/// let cpl = bit_field_get(selector, 13, 12);
/// ```
#[inline(always)]
pub const fn bit_field_get(value: u64, high: u32, low: u32) -> u64 {
    debug_assert!(high >= low,  "high must be >= low");
    debug_assert!(high < 64,    "high bit index out of range");
    let width = high - low + 1;
    let mask = if width == 64 { u64::MAX } else { (1u64 << width) - 1 };
    (value >> low) & mask
}

/// Insert `field` into bits `[high:low]` of `value`, leaving all other bits unchanged.
///
/// `field` is treated as right-justified — it will be shifted left by `low` bits
/// before insertion. Bits of `field` above the field width are silently masked off.
#[inline(always)]
pub const fn bit_field_set(value: u64, high: u32, low: u32, field: u64) -> u64 {
    debug_assert!(high >= low, "high must be >= low");
    debug_assert!(high < 64,   "high bit index out of range");
    let width = high - low + 1;
    let mask = if width == 64 { u64::MAX } else { (1u64 << width) - 1 };
    let positioned_mask = mask << low;
    let positioned_field = (field & mask) << low;
    (value & !positioned_mask) | positioned_field
}

/// Return a bitmask with bits `[high:low]` set and all others clear.
#[inline(always)]
pub const fn bit_mask(high: u32, low: u32) -> u64 {
    debug_assert!(high >= low, "high must be >= low");
    debug_assert!(high < 64,   "high bit index out of range");
    let width = high - low + 1;
    let base = if width == 64 { u64::MAX } else { (1u64 << width) - 1 };
    base << low
}

/// Return `true` if `n` is a power of two.
#[inline(always)]
pub const fn is_power_of_two(n: u64) -> bool {
    n != 0 && (n & n.wrapping_sub(1)) == 0
}

/// Round `n` up to the next power of two.
///
/// Returns 1 for input 0. Panics in debug if the result would overflow u64.
#[inline(always)]
pub const fn next_power_of_two(n: u64) -> u64 {
    if n <= 1 { return 1; }
    let leading = (n - 1).leading_zeros();
    debug_assert!(leading > 0, "next_power_of_two would overflow u64");
    1u64 << (64 - leading)
}

/// Isolate the lowest set bit of `n` (also called the least significant set bit).
///
/// Returns 0 if `n == 0`. The result is always a power of two.
/// Maps to a single `BLSI` instruction on x86_64 with BMI1.
#[inline(always)]
pub const fn lowest_set_bit(n: u64) -> u64 {
    n & n.wrapping_neg()
}

/// Clear the lowest set bit of `n`.
///
/// Maps to a single `BLSR` instruction on x86_64 with BMI1.
#[inline(always)]
pub const fn clear_lowest_set_bit(n: u64) -> u64 {
    n & (n - 1)
}

/// Return the index (0-based) of the lowest set bit, or `None` if `n == 0`.
#[inline(always)]
pub const fn lowest_set_bit_index(n: u64) -> Option<u32> {
    if n == 0 { None } else { Some(n.trailing_zeros()) }
}

/// Return the index (0-based) of the highest set bit, or `None` if `n == 0`.
///
/// Equivalent to floor(log₂(n)). Maps to `BSR` / `LZCNT` on x86_64.
#[inline(always)]
pub const fn highest_set_bit_index(n: u64) -> Option<u32> {
    if n == 0 { None } else { Some(63 - n.leading_zeros()) }
}

/// Count the number of set bits (population count / Hamming weight).
///
/// Maps to the `POPCNT` instruction on x86_64.
#[inline(always)]
pub fn popcount(n: u64) -> u32 {
    n.count_ones()
}

/// Return `true` if `n` has an even number of set bits (even parity).
#[inline(always)]
pub fn even_parity(n: u64) -> bool {
    n.count_ones() % 2 == 0
}
```

---

## 3. Flag Registers and Status Words (`flags.rs`)

Hardware registers that pack boolean status into individual bits — RFLAGS, CR0, CR4, EFER, APIC registers — are a ubiquitous pattern. This module provides a typed interface for them.

```rust
/// A raw 64-bit flag register value with named bit operations.
///
/// Construct from a `u64`; manipulate with the methods below.
/// The underlying representation is always the raw machine word.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(transparent)]
pub struct FlagRegister(pub u64);

impl FlagRegister {
    pub const ZERO: Self = Self(0);

    #[inline(always)]
    pub const fn from_raw(raw: u64) -> Self { Self(raw) }

    #[inline(always)]
    pub const fn raw(self) -> u64 { self.0 }

    /// Test a single flag bit.
    #[inline(always)]
    pub const fn has(self, flag: u64) -> bool {
        (self.0 & flag) == flag
    }

    /// Test any of a set of flag bits.
    #[inline(always)]
    pub const fn has_any(self, flags: u64) -> bool {
        (self.0 & flags) != 0
    }

    /// Set one or more flags (OR).
    #[inline(always)]
    pub const fn set(self, flags: u64) -> Self {
        Self(self.0 | flags)
    }

    /// Clear one or more flags (AND NOT).
    #[inline(always)]
    pub const fn clear(self, flags: u64) -> Self {
        Self(self.0 & !flags)
    }

    /// Toggle one or more flags (XOR).
    #[inline(always)]
    pub const fn toggle(self, flags: u64) -> Self {
        Self(self.0 ^ flags)
    }

    /// Replace a multi-bit field. `mask` selects which bits to replace;
    /// `value` is already positioned (not right-justified).
    #[inline(always)]
    pub const fn replace_field(self, mask: u64, value: u64) -> Self {
        Self((self.0 & !mask) | (value & mask))
    }

    /// Return a new register with only the specified flags preserved.
    #[inline(always)]
    pub const fn isolate(self, mask: u64) -> Self {
        Self(self.0 & mask)
    }
}

// --- x86_64 RFLAGS bit definitions (representative subset) ---
pub mod rflags {
    pub const CF:   u64 = 1 << 0;   // Carry Flag
    pub const PF:   u64 = 1 << 2;   // Parity Flag
    pub const AF:   u64 = 1 << 4;   // Auxiliary Carry Flag
    pub const ZF:   u64 = 1 << 6;   // Zero Flag
    pub const SF:   u64 = 1 << 7;   // Sign Flag
    pub const TF:   u64 = 1 << 8;   // Trap Flag
    pub const IF:   u64 = 1 << 9;   // Interrupt Enable Flag
    pub const DF:   u64 = 1 << 10;  // Direction Flag
    pub const OF:   u64 = 1 << 11;  // Overflow Flag
    pub const IOPL: u64 = 3 << 12;  // I/O Privilege Level (2-bit field)
    pub const NT:   u64 = 1 << 14;  // Nested Task
    pub const RF:   u64 = 1 << 16;  // Resume Flag
    pub const VM:   u64 = 1 << 17;  // Virtual-8086 Mode
    pub const AC:   u64 = 1 << 18;  // Alignment Check
    pub const VIF:  u64 = 1 << 19;  // Virtual Interrupt Flag
    pub const VIP:  u64 = 1 << 20;  // Virtual Interrupt Pending
    pub const ID:   u64 = 1 << 21;  // CPUID Toggle
}

// --- x86_64 CR0 bit definitions ---
pub mod cr0 {
    pub const PE:  u64 = 1 << 0;   // Protection Enable
    pub const MP:  u64 = 1 << 1;   // Monitor Coprocessor
    pub const EM:  u64 = 1 << 2;   // Emulation (FPU absent)
    pub const TS:  u64 = 1 << 3;   // Task Switched
    pub const ET:  u64 = 1 << 4;   // Extension Type
    pub const NE:  u64 = 1 << 5;   // Numeric Error
    pub const WP:  u64 = 1 << 16;  // Write Protect (ring 0 respects page RO)
    pub const AM:  u64 = 1 << 18;  // Alignment Mask
    pub const NW:  u64 = 1 << 29;  // Not Write-through
    pub const CD:  u64 = 1 << 30;  // Cache Disable
    pub const PG:  u64 = 1 << 31;  // Paging Enable
}

// --- x86_64 CR4 bit definitions ---
pub mod cr4 {
    pub const VME:        u64 = 1 << 0;
    pub const PVI:        u64 = 1 << 1;
    pub const TSD:        u64 = 1 << 2;
    pub const DE:         u64 = 1 << 3;
    pub const PSE:        u64 = 1 << 4;   // Page Size Extension (4M pages)
    pub const PAE:        u64 = 1 << 5;   // Physical Address Extension
    pub const MCE:        u64 = 1 << 6;
    pub const PGE:        u64 = 1 << 7;   // Page Global Enable
    pub const PCE:        u64 = 1 << 8;
    pub const OSFXSR:     u64 = 1 << 9;
    pub const OSXMMEXCPT: u64 = 1 << 10;
    pub const UMIP:       u64 = 1 << 11;
    pub const LA57:       u64 = 1 << 12;  // 5-level paging
    pub const VMXE:       u64 = 1 << 13;
    pub const SMXE:       u64 = 1 << 14;
    pub const FSGSBASE:   u64 = 1 << 16;
    pub const PCIDE:      u64 = 1 << 17;
    pub const OSXSAVE:    u64 = 1 << 18;
    pub const SMEP:       u64 = 1 << 20;
    pub const SMAP:       u64 = 1 << 21;
    pub const PKE:        u64 = 1 << 22;
    pub const CET:        u64 = 1 << 23;
    pub const PKS:        u64 = 1 << 24;
}

// --- EFER (Extended Feature Enable Register, MSR 0xC000_0080) ---
pub mod efer {
    pub const SCE:  u64 = 1 << 0;   // SYSCALL Enable
    pub const LME:  u64 = 1 << 8;   // Long Mode Enable
    pub const LMA:  u64 = 1 << 10;  // Long Mode Active (read-only)
    pub const NXE:  u64 = 1 << 11;  // No-Execute Enable
    pub const SVME: u64 = 1 << 12;  // SVM Enable (AMD)
    pub const LMSLE:u64 = 1 << 13;
    pub const FFXSR:u64 = 1 << 14;
    pub const TCE:  u64 = 1 << 15;
}
```

**Usage example**:

```rust
// Enable paging and write-protect in CR0
let cr0 = FlagRegister::from_raw(read_cr0());
let new_cr0 = cr0.set(cr0::PG | cr0::WP);
unsafe { write_cr0(new_cr0.raw()); }

// Check whether we're currently in long mode
let efer = FlagRegister::from_raw(rdmsr(0xC000_0080));
assert!(efer.has(efer::LMA), "not in long mode");
```

---

## 4. Memory-Mapped I/O (`mmio.rs`)

MMIO registers **must** be accessed through volatile operations. The compiler is not allowed to cache, reorder, or eliminate volatile accesses. Wrapping them in typed functions prevents accidental use of plain pointer dereferences.

```rust
use core::ptr;

/// Read a `u8` from a memory-mapped register at `addr`.
///
/// # Safety
/// `addr` must be a valid, mapped MMIO address for a `u8`-wide register.
/// The caller is responsible for ensuring the address is correct and the
/// hardware is in an appropriate state.
#[inline(always)]
pub unsafe fn mmio_read8(addr: u64) -> u8 {
    ptr::read_volatile(addr as *const u8)
}

/// Read a `u16` from a memory-mapped register at `addr`.
///
/// # Safety
/// `addr` must be valid, mapped, and naturally aligned to 2 bytes.
#[inline(always)]
pub unsafe fn mmio_read16(addr: u64) -> u16 {
    ptr::read_volatile(addr as *const u16)
}

/// Read a `u32` from a memory-mapped register at `addr`.
#[inline(always)]
pub unsafe fn mmio_read32(addr: u64) -> u32 {
    ptr::read_volatile(addr as *const u32)
}

/// Read a `u64` from a memory-mapped register at `addr`.
#[inline(always)]
pub unsafe fn mmio_read64(addr: u64) -> u64 {
    ptr::read_volatile(addr as *const u64)
}

/// Write `value` to a memory-mapped register at `addr`.
#[inline(always)]
pub unsafe fn mmio_write8(addr: u64, value: u8) {
    ptr::write_volatile(addr as *mut u8, value)
}

#[inline(always)]
pub unsafe fn mmio_write16(addr: u64, value: u16) {
    ptr::write_volatile(addr as *mut u16, value)
}

#[inline(always)]
pub unsafe fn mmio_write32(addr: u64, value: u32) {
    ptr::write_volatile(addr as *mut u32, value)
}

#[inline(always)]
pub unsafe fn mmio_write64(addr: u64, value: u64) {
    ptr::write_volatile(addr as *mut u64, value)
}

/// Read-modify-write: set bits in `mask` at `addr` (32-bit register).
///
/// Reads the current value, ORs in `mask`, writes back. Not atomic —
/// do not use on registers that require atomic RMW (use hardware-provided
/// atomics or protect with a spinlock).
#[inline(always)]
pub unsafe fn mmio_set_bits32(addr: u64, mask: u32) {
    let current = mmio_read32(addr);
    mmio_write32(addr, current | mask);
}

/// Read-modify-write: clear bits in `mask` at `addr` (32-bit register).
#[inline(always)]
pub unsafe fn mmio_clear_bits32(addr: u64, mask: u32) {
    let current = mmio_read32(addr);
    mmio_write32(addr, current & !mask);
}

/// Read-modify-write: update a bit field in a 32-bit MMIO register.
///
/// Clears bits selected by `mask`, then ORs in `value` (pre-positioned).
#[inline(always)]
pub unsafe fn mmio_update_field32(addr: u64, mask: u32, value: u32) {
    let current = mmio_read32(addr);
    mmio_write32(addr, (current & !mask) | (value & mask));
}

/// Read-modify-write: set bits in `mask` at `addr` (64-bit register).
#[inline(always)]
pub unsafe fn mmio_set_bits64(addr: u64, mask: u64) {
    let current = mmio_read64(addr);
    mmio_write64(addr, current | mask);
}

/// Read-modify-write: clear bits in `mask` at `addr` (64-bit register).
#[inline(always)]
pub unsafe fn mmio_clear_bits64(addr: u64, mask: u64) {
    let current = mmio_read64(addr);
    mmio_write64(addr, current & !mask);
}

/// Typed MMIO register view — wraps a base address and provides offset-based access.
///
/// Useful for device register blocks where all registers are at `base + offset`.
///
/// ```rust
/// let uart = MmioBlock::new(0xFEDC_0000);
/// unsafe {
///     uart.write32(0x00, 0x0000_0001);  // enable
///     let status = uart.read32(0x18);   // read status register
/// }
/// ```
pub struct MmioBlock {
    base: u64,
}

impl MmioBlock {
    #[inline(always)]
    pub const fn new(base: u64) -> Self { Self { base } }

    #[inline(always)]
    pub unsafe fn read8(&self, offset: u64) -> u8 {
        mmio_read8(self.base + offset)
    }

    #[inline(always)]
    pub unsafe fn read16(&self, offset: u64) -> u16 {
        mmio_read16(self.base + offset)
    }

    #[inline(always)]
    pub unsafe fn read32(&self, offset: u64) -> u32 {
        mmio_read32(self.base + offset)
    }

    #[inline(always)]
    pub unsafe fn read64(&self, offset: u64) -> u64 {
        mmio_read64(self.base + offset)
    }

    #[inline(always)]
    pub unsafe fn write8(&self, offset: u64, value: u8) {
        mmio_write8(self.base + offset, value)
    }

    #[inline(always)]
    pub unsafe fn write16(&self, offset: u64, value: u16) {
        mmio_write16(self.base + offset, value)
    }

    #[inline(always)]
    pub unsafe fn write32(&self, offset: u64, value: u32) {
        mmio_write32(self.base + offset, value)
    }

    #[inline(always)]
    pub unsafe fn write64(&self, offset: u64, value: u64) {
        mmio_write64(self.base + offset, value)
    }

    #[inline(always)]
    pub unsafe fn set_bits32(&self, offset: u64, mask: u32) {
        mmio_set_bits32(self.base + offset, mask)
    }

    #[inline(always)]
    pub unsafe fn clear_bits32(&self, offset: u64, mask: u32) {
        mmio_clear_bits32(self.base + offset, mask)
    }
}
```

---

## 5. DMA Buffer Arithmetic (`dma.rs`)

DMA transfers have hard alignment requirements — both the buffer base address and the transfer length may need to be aligned to cache lines, device-specific granularities, or IOMMU page boundaries. An unaligned DMA buffer causes silent data corruption on some hardware, or an IOMMU fault on others.

```rust
/// Standard cache line size on x86_64 Intel/AMD.
/// May differ on other architectures; query CPUID leaf 0x01 EBX[15:8] if uncertain.
pub const CACHE_LINE_SIZE: u64 = 64;

/// Typical IOMMU/DMA page size (matches 4 KiB system page).
pub const DMA_PAGE_SIZE: u64 = 0x1000;

/// Check whether a physical address is suitable as a DMA buffer base.
///
/// `dma_align` is the minimum alignment required by the device (often
/// `CACHE_LINE_SIZE` for coherent DMA, or `DMA_PAGE_SIZE` for scatter-gather).
#[inline(always)]
pub const fn dma_is_aligned(phys_addr: u64, dma_align: u64) -> bool {
    is_aligned(phys_addr, dma_align)
}

/// Compute the aligned DMA buffer start and the wasted bytes before it.
///
/// Given a raw physical address and a required DMA alignment, returns:
/// - `aligned_base`: the first address ≥ `phys_addr` satisfying `dma_align`
/// - `padding_before`: bytes wasted between `phys_addr` and `aligned_base`
///
/// If `phys_addr` is already aligned, `padding_before` is 0.
#[inline(always)]
pub const fn dma_align_buffer(phys_addr: u64, dma_align: u64)
    -> (u64 /* aligned_base */, u64 /* padding_before */)
{
    let aligned = align_up(phys_addr, dma_align);
    (aligned, aligned - phys_addr)
}

/// Round a DMA transfer length up to a multiple of `granularity`.
///
/// Many DMA engines require that the transfer count be a multiple of the
/// device's bus width or burst size (e.g., 4 bytes for 32-bit PCI).
#[inline(always)]
pub const fn dma_round_length(len: u64, granularity: u64) -> u64 {
    align_up(len, granularity)
}

/// Compute the number of scatter-gather entries (segments) needed to describe
/// a buffer given a maximum segment size.
///
/// This assumes worst-case alignment — i.e., the buffer may start at any
/// offset within a segment. Use when you don't yet know the buffer's
/// physical address (e.g., during descriptor ring pre-allocation).
#[inline(always)]
pub const fn dma_sg_segments_needed(len: u64, max_segment_size: u64) -> u64 {
    // +1 for potential split at start/end boundary
    frames_needed(len, max_segment_size) + 1
}

/// Split a physically contiguous buffer into segment descriptors for a
/// scatter-gather list, where each segment may not cross a `segment_boundary`.
///
/// Returns the number of descriptors written into `out`.
///
/// `out` must have capacity ≥ `dma_sg_segments_needed(len, segment_boundary)`.
pub fn dma_build_sg(
    phys_base: u64,
    len: u64,
    segment_boundary: u64,
    out: &mut [(u64, u64)],  // (phys_addr, len) per segment
) -> usize {
    debug_assert!(segment_boundary.is_power_of_two());
    let mut remaining = len;
    let mut addr = phys_base;
    let mut count = 0;

    while remaining > 0 {
        // How many bytes until the next segment boundary?
        let boundary_end = align_up(addr + 1, segment_boundary);
        let chunk = (boundary_end - addr).min(remaining);

        out[count] = (addr, chunk);
        count += 1;

        addr += chunk;
        remaining -= chunk;
    }

    count
}

/// Convert a physical address to a bus address (identity mapping assumed).
///
/// On systems with an IOMMU, the bus address is the IOVA, not the physical
/// address. This stub is the trivial case (one-to-one mapping or no IOMMU).
/// Replace with an IOMMU lookup table in a real driver.
#[inline(always)]
pub const fn phys_to_bus(phys: u64) -> u64 {
    phys  // identity mapping stub
}

/// Check whether a physical address range fits within a 32-bit DMA window
/// (required for devices that cannot address above 4 GiB without IOMMU remapping).
#[inline(always)]
pub const fn fits_in_32bit_dma(phys: u64, len: u64) -> bool {
    phys.saturating_add(len) <= 0x1_0000_0000
}
```

---

## 6. Cache-Line Arithmetic (`cache.rs`)

False sharing, cache pollution, and cache-line splits are serious performance and correctness issues in kernel code, especially for per-CPU data and lock-free structures.

```rust
pub const CACHE_LINE_BYTES: u64 = 64;
pub const CACHE_LINE_BITS:  u64 = 6;   // log₂(64)

/// Round `size` up to a multiple of the cache line size.
///
/// Use this to pad structures that must not share a cache line
/// with adjacent data (e.g., per-CPU counters, spinlock hot fields).
#[inline(always)]
pub const fn cache_align_size(size: u64) -> u64 {
    align_up(size, CACHE_LINE_BYTES)
}

/// Return the cache line number containing `addr`.
#[inline(always)]
pub const fn cache_line_of(addr: u64) -> u64 {
    addr >> CACHE_LINE_BITS
}

/// Return the offset of `addr` within its cache line (0..63).
#[inline(always)]
pub const fn cache_line_offset(addr: u64) -> u64 {
    addr & (CACHE_LINE_BYTES - 1)
}

/// Return `true` if the range [addr, addr+size) fits in a single cache line.
///
/// A range that crosses a cache-line boundary causes two cache transactions
/// instead of one — a "cache line split" — which is a significant penalty
/// on hot paths.
#[inline(always)]
pub const fn is_cache_line_contained(addr: u64, size: u64) -> bool {
    debug_assert!(size <= CACHE_LINE_BYTES, "size exceeds one cache line");
    cache_line_of(addr) == cache_line_of(addr + size - 1)
}

/// Return the number of cache lines touched by the range [addr, addr+size).
#[inline(always)]
pub const fn cache_lines_spanned(addr: u64, size: u64) -> u64 {
    if size == 0 { return 0; }
    cache_line_of(addr + size - 1) - cache_line_of(addr) + 1
}

/// Align `addr` down to the start of its cache line.
#[inline(always)]
pub const fn cache_line_start(addr: u64) -> u64 {
    align_down(addr, CACHE_LINE_BYTES)
}
```

---

## 7. Page Table Entry Encoding (`paging.rs`)

x86_64 page table entries are 64-bit integers with tightly packed flag bits and a physical frame number. These functions encode and decode them generically — the same bit patterns apply to PML4E, PDPTE, PDE, and PTE entries, varying only in which bits are valid.

```rust
// x86_64 4-level paging entry flags (common subset, Intel Vol. 3A §4.5)
pub mod pte_flags {
    pub const PRESENT:       u64 = 1 << 0;
    pub const WRITABLE:      u64 = 1 << 1;
    pub const USER:          u64 = 1 << 2;
    pub const WRITE_THROUGH: u64 = 1 << 3;
    pub const CACHE_DISABLE: u64 = 1 << 4;
    pub const ACCESSED:      u64 = 1 << 5;
    pub const DIRTY:         u64 = 1 << 6;   // PTE and large-page entries only
    pub const HUGE_PAGE:     u64 = 1 << 7;   // PDPTE (1 GiB) and PDE (2 MiB)
    pub const GLOBAL:        u64 = 1 << 8;
    pub const NO_EXECUTE:    u64 = 1 << 63;

    // Bits 11:9 are available to the OS (software bits)
    pub const AVAIL_MASK:    u64 = 0b111 << 9;
    pub const AVAIL_SHIFT:   u32 = 9;

    // Physical frame number occupies bits 51:12 for 4 KiB pages
    pub const PFN_MASK_4K:   u64 = 0x000F_FFFF_FFFF_F000;
    // For 2 MiB pages, frame base is bits 51:21
    pub const PFN_MASK_2M:   u64 = 0x000F_FFFF_FFE0_0000;
    // For 1 GiB pages, frame base is bits 51:30
    pub const PFN_MASK_1G:   u64 = 0x000F_FFFC_0000_0000;
}

/// Build a page table entry from a physical address and flags.
///
/// `phys_addr` must be aligned to `frame_size`. The frame size determines
/// which bits carry the physical address:
///   - 4 KiB: bits 51:12
///   - 2 MiB: bits 51:21 (HUGE_PAGE flag must be set in `flags`)
///   - 1 GiB: bits 51:30 (HUGE_PAGE flag must be set in `flags`)
///
/// The `PRESENT` flag is **not** automatically added — set it in `flags`
/// explicitly. This allows construction of swap entries and guard entries
/// where bit 0 is intentionally clear.
#[inline(always)]
pub const fn pte_encode(phys_addr: u64, frame_size: u64, flags: u64) -> u64 {
    debug_assert!(frame_size.is_power_of_two());
    debug_assert!(phys_addr & (frame_size - 1) == 0, "phys_addr not aligned to frame_size");
    // Physical address bits land at their natural position (no shift needed)
    // since the page offset bits are always 0 for an aligned address.
    phys_addr | flags
}

/// Extract the physical base address from a page table entry.
///
/// `frame_size` must match the size of the mapping described by this entry.
#[inline(always)]
pub const fn pte_phys_addr(entry: u64, frame_size: u64) -> u64 {
    debug_assert!(frame_size.is_power_of_two());
    // Mask off flag bits below the frame offset and the NX bit and reserved
    // bits above bit 51. The result is the physical frame base address.
    let pfn_mask = !((frame_size - 1) | (0xFFF0_0000_0000_0000));
    entry & pfn_mask
}

/// Check whether a page table entry is present (bit 0 set).
#[inline(always)]
pub const fn pte_is_present(entry: u64) -> bool {
    entry & pte_flags::PRESENT != 0
}

/// Check whether an entry is a large/huge page (bit 7 set at PDPTE or PDE level).
#[inline(always)]
pub const fn pte_is_huge(entry: u64) -> bool {
    entry & pte_flags::HUGE_PAGE != 0
}

/// Set one or more flags in an existing entry, preserving the physical address.
#[inline(always)]
pub const fn pte_set_flags(entry: u64, flags: u64) -> u64 {
    entry | flags
}

/// Clear one or more flags in an existing entry, preserving the physical address.
#[inline(always)]
pub const fn pte_clear_flags(entry: u64, flags: u64) -> u64 {
    entry & !flags
}

/// Read the OS-available software bits (bits 11:9) from an entry.
#[inline(always)]
pub const fn pte_avail_bits(entry: u64) -> u64 {
    (entry & pte_flags::AVAIL_MASK) >> pte_flags::AVAIL_SHIFT
}

/// Write the OS-available software bits (bits 11:9) into an entry.
#[inline(always)]
pub const fn pte_set_avail_bits(entry: u64, bits: u64) -> u64 {
    debug_assert!(bits <= 0b111, "only 3 avail bits exist");
    (entry & !pte_flags::AVAIL_MASK) | ((bits << pte_flags::AVAIL_SHIFT) & pte_flags::AVAIL_MASK)
}

/// Compute the index into a page table level from a virtual address.
///
/// On x86_64 4-level paging:
/// - Level 4 (PML4):  bits 47:39
/// - Level 3 (PDPT):  bits 38:30
/// - Level 2 (PD):    bits 29:21
/// - Level 1 (PT):    bits 20:12
/// - Page offset:     bits 11:0
///
/// `level` is 1..=4 matching the above. The returned index is 0..=511.
#[inline(always)]
pub const fn vaddr_pt_index(vaddr: u64, level: u32) -> u64 {
    debug_assert!(level >= 1 && level <= 4, "level must be 1..=4");
    let shift = 12 + (level - 1) * 9;
    (vaddr >> shift) & 0x1FF
}

/// Compute the page offset within the final frame from a virtual address
/// and frame size.
#[inline(always)]
pub const fn vaddr_page_offset(vaddr: u64, frame_size: u64) -> u64 {
    frame_offset(vaddr, frame_size)
}
```

---

## 8. Endianness Conversion (`endian.rs`)

Hardware registers and network protocols often specify byte order explicitly. On x86_64 (little-endian), reading a big-endian register without byte-swapping produces a garbage value.

```rust
/// Convert a `u16` from big-endian (network) to native (little-endian on x86).
#[inline(always)]
pub const fn be16_to_cpu(x: u16) -> u16 { u16::from_be(x) }

/// Convert a `u32` from big-endian to native.
#[inline(always)]
pub const fn be32_to_cpu(x: u32) -> u32 { u32::from_be(x) }

/// Convert a `u64` from big-endian to native.
#[inline(always)]
pub const fn be64_to_cpu(x: u64) -> u64 { u64::from_be(x) }

/// Convert a `u16` from native (little-endian) to big-endian.
#[inline(always)]
pub const fn cpu_to_be16(x: u16) -> u16 { x.to_be() }

/// Convert a `u32` from native to big-endian.
#[inline(always)]
pub const fn cpu_to_be32(x: u32) -> u32 { x.to_be() }

/// Convert a `u64` from native to big-endian.
#[inline(always)]
pub const fn cpu_to_be64(x: u64) -> u64 { x.to_be() }

/// Read a `u32` from an unaligned byte slice in big-endian order.
///
/// Safe on x86 (which supports unaligned loads) but uses byte-by-byte
/// construction to make the intent explicit and avoid UB from casting.
#[inline(always)]
pub fn read_be32(bytes: &[u8]) -> u32 {
    assert!(bytes.len() >= 4);
    (bytes[0] as u32) << 24
    | (bytes[1] as u32) << 16
    | (bytes[2] as u32) << 8
    | (bytes[3] as u32)
}

/// Write a `u32` to a byte slice in big-endian order.
#[inline(always)]
pub fn write_be32(bytes: &mut [u8], value: u32) {
    assert!(bytes.len() >= 4);
    bytes[0] = (value >> 24) as u8;
    bytes[1] = (value >> 16) as u8;
    bytes[2] = (value >> 8)  as u8;
    bytes[3] = value          as u8;
}

/// Read a `u32` from a byte slice in little-endian order.
#[inline(always)]
pub fn read_le32(bytes: &[u8]) -> u32 {
    assert!(bytes.len() >= 4);
    (bytes[3] as u32) << 24
    | (bytes[2] as u32) << 16
    | (bytes[1] as u32) << 8
    | (bytes[0] as u32)
}
```

---

## 9. Port I/O (`pio.rs`)

x86/x86_64 has a separate I/O address space accessed via the `IN` and `OUT` instructions, distinct from the physical memory address space. Legacy devices (8259 PIC, PS/2, legacy serial) live here. These require inline assembly in Rust.

```rust
/// Read a byte from x86 I/O port `port`.
///
/// # Safety
/// Executing `IN` at the wrong port can crash the system, corrupt device
/// state, or cause non-maskable interrupts. Caller must ensure the port
/// is valid and the current privilege level permits port access (CPL=0
/// or IOPL ≥ CPL, or the I/O Permission Bitmap grants access).
#[inline(always)]
pub unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    core::arch::asm!(
        "in al, dx",
        in("dx") port,
        out("al") value,
        options(nomem, nostack, preserves_flags)
    );
    value
}

/// Read a word (u16) from I/O port `port`.
#[inline(always)]
pub unsafe fn inw(port: u16) -> u16 {
    let value: u16;
    core::arch::asm!(
        "in ax, dx",
        in("dx") port,
        out("ax") value,
        options(nomem, nostack, preserves_flags)
    );
    value
}

/// Read a dword (u32) from I/O port `port`.
#[inline(always)]
pub unsafe fn inl(port: u16) -> u32 {
    let value: u32;
    core::arch::asm!(
        "in eax, dx",
        in("dx") port,
        out("eax") value,
        options(nomem, nostack, preserves_flags)
    );
    value
}

/// Write a byte to I/O port `port`.
#[inline(always)]
pub unsafe fn outb(port: u16, value: u8) {
    core::arch::asm!(
        "out dx, al",
        in("dx") port,
        in("al") value,
        options(nomem, nostack, preserves_flags)
    );
}

/// Write a word to I/O port `port`.
#[inline(always)]
pub unsafe fn outw(port: u16, value: u16) {
    core::arch::asm!(
        "out dx, ax",
        in("dx") port,
        in("ax") value,
        options(nomem, nostack, preserves_flags)
    );
}

/// Write a dword to I/O port `port`.
#[inline(always)]
pub unsafe fn outl(port: u16, value: u32) {
    core::arch::asm!(
        "out dx, eax",
        in("dx") port,
        in("eax") value,
        options(nomem, nostack, preserves_flags)
    );
}

/// Read-modify-write: set bits in a port I/O register (8-bit).
#[inline(always)]
pub unsafe fn pio_set_bits8(port: u16, mask: u8) {
    outb(port, inb(port) | mask);
}

/// Read-modify-write: clear bits in a port I/O register (8-bit).
#[inline(always)]
pub unsafe fn pio_clear_bits8(port: u16, mask: u8) {
    outb(port, inb(port) & !mask);
}
```

---

## Quick Reference

```
// Alignment
align_down(addr, align)               — round down to multiple of align
align_up(addr, align)                 — round up to multiple of align
is_aligned(addr, align)               — test alignment
align_offset(addr, align)             — bytes until next boundary

// Frame/Page arithmetic
frame_number(phys, frame_size)        — physical address → frame index
frame_base(frame_num, frame_size)     — frame index → base physical address
frame_offset(addr, frame_size)        — byte offset within frame
frames_needed(bytes, frame_size)      — minimum frames to hold N bytes

// Bit manipulation
bit_set(v, n)                         — set bit n
bit_clear(v, n)                       — clear bit n
bit_toggle(v, n)                      — toggle bit n
bit_test(v, n)                        — test bit n
bit_field_get(v, high, low)           — extract right-justified field
bit_field_set(v, high, low, field)    — insert right-justified field
bit_mask(high, low)                   — build a contiguous mask

// Numeric properties
is_power_of_two(n)                    — test
next_power_of_two(n)                  — round up
lowest_set_bit(n)                     — isolate LSB (= n & -n)
clear_lowest_set_bit(n)               — strip LSB (= n & (n-1))
lowest_set_bit_index(n)               — Option<u32>, trailing_zeros
highest_set_bit_index(n)              — Option<u32>, floor(log₂ n)

// MMIO
mmio_read{8,16,32,64}(addr)           — volatile read
mmio_write{8,16,32,64}(addr, val)     — volatile write
mmio_set_bits{32,64}(addr, mask)      — RMW set
mmio_clear_bits{32,64}(addr, mask)    — RMW clear
mmio_update_field32(addr, mask, val)  — RMW field replace

// Port I/O
inb/inw/inl(port)                     — read from I/O port
outb/outw/outl(port, val)             — write to I/O port

// Page table entries
pte_encode(phys, frame_size, flags)   — build PTE
pte_phys_addr(entry, frame_size)      — extract physical address
pte_set_flags / pte_clear_flags       — flag manipulation
vaddr_pt_index(vaddr, level)          — virtual address → PT level index

// DMA
dma_align_buffer(phys, align)         — (aligned_base, padding_before)
dma_build_sg(base, len, boundary, out)— scatter-gather descriptor list
fits_in_32bit_dma(phys, len)          — 32-bit DMA window check

// Cache
cache_align_size(n)                   — round up to cache line
cache_lines_spanned(addr, size)       — count of cache lines touched
is_cache_line_contained(addr, size)   — test for cache line split

// Endianness
be{16,32,64}_to_cpu / cpu_to_be{16,32,64}
read_be32 / write_be32 / read_le32
```

---

*Intel® 64 and IA-32 Architectures Software Developer's Manual, Volume 3A: System Programming Guide, Chapters 4 (Paging), 5 (Protection), and 10 (APIC). PCI Local Bus Specification for DMA conventions. The `x86_64` crate and `volatile` crate provide higher-level wrappers over these primitives for production kernels.*
