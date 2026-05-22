# Allocator Integration Plan

Implementation guide for integrating the allocator stack into `aVOIDSTARch/rust-os`.
Written for a Claude agent with access to both this document and the generated source files.

---

## Repo context

```
rust-os/
  barnacle/          ← bootable kernel binary (has boot.asm, the existing trampoline)
  crusty_os/         ← kernel logic crate (target for most integration work)
  framework/         ← shared types (analogous to generated `shared/`)
  platform/          ← hardware abstraction
  isoroot/boot/      ← grub.cfg lives here
  xtask/             ← build tooling
```

The generated workspace is **not** dropped in wholesale. Individual files are
transplanted into the existing crate structure. Do not create a parallel workspace.

---

## Files to transplant

### 1. `shared/src/lib.rs` → merge into `framework/src/lib.rs`

Add these types if not already present:

| Type / Constant | Purpose |
|---|---|
| `MemoryRegion` | Protocol-agnostic physical region descriptor |
| `MemoryRegionKind` | Enum: Usable / Reserved / AcpiReclaimable / … |
| `AllocStats` | Snapshot struct returned by every allocator layer |
| `PAGE_SIZE`, `PAGE_SHIFT` | 4096, 12 |
| `BUDDY_MAX_ORDER` | 11 (covers order 0–10, 4 KiB–4 MiB) |

`framework` must compile with `#![no_std]`. Do not add `std` imports.
If `framework` already defines a memory region type, adapt `MemoryRegion` to
match rather than introducing a duplicate.

---

### 2. `kernel/src/buddy.rs` → `crusty_os/src/allocator/buddy.rs`

No changes required to the file itself.

Add to `crusty_os/src/allocator/mod.rs`:
```rust
pub mod buddy;
```

The file depends on:
- `shared::{AllocStats, BUDDY_MAX_ORDER, PAGE_SIZE}` — adjust import path to
  match wherever `framework` exports these
- `spin::Mutex` — confirm `spin` is already in `barnacle/Cargo.toml` or
  `crusty_os/Cargo.toml`; add `spin = { version = "0.9", features = ["mutex"] }`
  if not

**Public API used by other modules:**
```rust
pub static BUDDY: Mutex<BuddyAllocator>
pub fn alloc_pages(order: usize) -> Option<*mut u8>          // convenience wrapper
pub unsafe fn dealloc_pages(ptr: *mut u8, order: usize)      // convenience wrapper
// On the BuddyAllocator itself:
pub unsafe fn add_region(&mut self, virt_base: usize, page_count: usize)
pub fn alloc_pages(&mut self, order: usize) -> Option<*mut u8>
pub unsafe fn dealloc_pages(&mut self, ptr: *mut u8, order: usize)
pub fn stats(&self) -> AllocStats
```

---

### 3. `kernel/src/slab.rs` → `crusty_os/src/allocator/slab.rs`

No changes required to the file itself.

Add to `crusty_os/src/allocator/mod.rs`:
```rust
pub mod slab;
```

The file depends on:
- `crate::buddy` (via `use crate::buddy`) — confirm the module path matches
  the location you chose in step 2
- `shared::{AllocStats, PAGE_SIZE}`

**Public API:**
```rust
pub struct SlabCache<T: Send>
impl<T: Send> SlabCache<T> {
    pub const fn new(slab_order: usize) -> Self
    pub fn alloc(&self) -> Option<NonNull<T>>
    pub unsafe fn dealloc(&self, ptr: NonNull<T>)
    pub fn stats(&self) -> AllocStats
}
pub const fn assert_slab_compatible<T>()   // call in const _: () = ... per type
```

For each kernel object type you want slab-allocated:
```rust
// In the relevant module, not in slab.rs itself:
static FOO_CACHE: SlabCache<Foo> = SlabCache::new(0);
const _: () = assert_slab_compatible::<Foo>();
```

---

### 4. `kernel/src/tlsf.rs` → `crusty_os/src/allocator/tlsf.rs`

No changes required to the file itself.

Add to `crusty_os/src/allocator/mod.rs`:
```rust
pub mod tlsf;
```

Depends on:
- `crate::buddy` for `alloc_pages`
- `shared::PAGE_SIZE`

**Public API:**
```rust
pub static TLSF: TlsfAllocator
impl TlsfAllocator {
    pub const fn new() -> Self
    pub unsafe fn init(&self, buddy_order: usize)  // call ONCE after buddy is populated
}
// Implements GlobalAlloc — registered via #[global_allocator]
```

Register in `barnacle/src/main.rs` (or wherever `#[global_allocator]` lives):
```rust
use crusty_os::allocator::tlsf::TLSF;
#[global_allocator]
static GLOBAL: crusty_os::allocator::tlsf::TlsfAllocator = TLSF;
```

If `barnacle` already has a `#[global_allocator]`, remove it first.

---

### 5. `kernel/src/boot/multiboot2.rs` → `barnacle/src/boot/multiboot2.rs`

This file owns the `kernel_main` symbol. **`boot.asm` already declares
`extern kernel_main` and calls it** — this Rust function is the direct target.

**Do not** add a second `kernel_main` anywhere else in the crate. One definition,
one object file, one symbol.

Add to `barnacle/src/boot/mod.rs` (create if absent):
```rust
pub mod multiboot2;
```

The file depends on:
- `multiboot2` crate (add to `barnacle/Cargo.toml`):
  ```toml
  multiboot2 = "0.15"
  ```
- `shared::{MemoryRegion, MemoryRegionKind, PAGE_SIZE}` — adjust import path
- `super::KernelBootInfo` — see step 6

**Constants defined here (used by `allocator_init`):**
```rust
pub const HHDM_OFFSET: usize = 0xFFFF_FFFF_8000_0000;
pub const BOOT_MAPPED_PHYS: u64 = 2 * 1024 * 1024;
```

`kernel_main` calls, in order:
1. `crate::allocator_init(&boot_info)` — must be defined in `barnacle` or
   re-exported from `crusty_os`
2. `crate::kernel_main_post_heap()` — diverges (`-> !`)

---

### 6. `kernel/src/boot/mod.rs` → `barnacle/src/boot/mod.rs`

Contains `KernelBootInfo` struct and the compile-time mutual-exclusion guards
for boot features.

If `barnacle` uses only Multiboot2, the feature-flag guards are optional —
keep `KernelBootInfo` and remove the `#[cfg(feature = ...)]` compile errors
if you are not building a multi-protocol binary.

```rust
pub struct KernelBootInfo {
    pub memory_regions:  &'static [MemoryRegion],
    pub hhdm_offset:     usize,
    pub kernel_phys_base: u64,
}
```

---

### 7. `kernel/src/main.rs` — functions to add to `barnacle/src/main.rs`

Do **not** copy `main.rs` wholesale. Extract and add these two functions:

```rust
pub unsafe fn allocator_init(boot_info: &boot::KernelBootInfo) { … }
pub fn kernel_main_post_heap() -> ! { … }
```

`allocator_init` does:
1. Locks `BUDDY`, calls `add_region` for each `MemoryRegionKind::Usable` entry
2. For Multiboot2: skips regions with `base >= BOOT_MAPPED_PHYS` (not yet mapped)
3. Drops the lock
4. Calls `TLSF.init(TLSF_POOL_ORDER)` — use order 8 (1 MiB) as the default

`kernel_main_post_heap` is your existing post-boot kernel entry. Replace or
call your current equivalent. Heap is live when this is entered.

---

### 8. Userspace — independent of kernel integration

These two crates are self-contained. Drop them into the repo as sibling crates
or use them standalone.

**`userspace/app/`** — binary using mimalloc as `#[global_allocator]`:
```toml
mimalloc = { version = "0.1", default-features = false, features = ["secure"] }
```
```rust
use mimalloc::MiMalloc;
#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;
```
That is the entire change. Every `Box`/`Vec`/`String` now uses mimalloc.

**`userspace/profiled/`** — library for heap introspection via jemalloc:
```toml
tikv-jemallocator = { version = "0.5", features = ["profiling", "stats"] }
tikv-jemalloc-ctl = { version = "0.5", features = ["use_std"] }
```
This is a **secondary** allocator instance for profiling only. It does not
replace mimalloc as the global allocator. Use `HeapProfiler::new()` to take
snapshots and `ScopedAlloc::new("label")` for RAII attribution.

---

## Initialisation sequence (strict order)

```
boot.asm _start
  │  validates magic, sets up page tables, enters long mode
  └─► kernel_main(mbi_phys: u64)          [multiboot2.rs]
        │  parses Multiboot2 memory map → &[MemoryRegion]
        │  constructs KernelBootInfo
        └─► allocator_init(&boot_info)     [barnacle/src/main.rs]
              │  BUDDY.lock().add_region() for each usable region < 2 MB
              │  drop lock
              └─► TLSF.init(8)            [tlsf.rs — ONCE]
                    └─► kernel_main_post_heap()
                          Box / Vec / SlabCache<T> all available here
```

Violating this order causes a guaranteed panic or silent memory corruption:
- Calling `TLSF.init()` before `add_region()` → TLSF pool allocation returns
  `None` → panic in `expect()`
- Calling `TLSF.init()` twice → second call overwrites the pool head pointer,
  corrupting the free list silently
- Allocating from a `SlabCache` before `TLSF.init()` → `alloc_pages` inside
  slab calls into uninitialised buddy → `None` → panic

---

## Common mistakes to avoid

**1. Defining `kernel_main` twice.**
`boot.asm` has `extern kernel_main`. If `barnacle` also has a `#[no_mangle] pub extern "C" fn kernel_main` anywhere else, the linker emits a duplicate symbol error or silently picks one. Search the entire crate for `kernel_main` before adding the new definition.

**2. Taking the buddy lock inside an interrupt handler without disabling IRQs.**
`spin::Mutex` is not reentrant. If an IRQ fires while `BUDDY` is locked on the
same CPU and the handler also calls `alloc_pages`, it spins forever. Disable
interrupts (`cli`) before locking, re-enable after (`sti`). Wrap allocator calls
in an `without_interrupts` guard once your IDT is live.

**3. Calling `TLSF.init()` with a pool order that exceeds available buddy pages.**
Order 8 = 1 MiB. If the Multiboot2 memory map gives you less than 1 MiB of
usable RAM below `BOOT_MAPPED_PHYS` (after excluding the kernel image), `init`
panics. Check `BUDDY.lock().stats().free_bytes` before calling `init` if you
are uncertain. Drop to order 7 (512 KiB) if necessary.

**4. Handing the buddy allocator virtual addresses that are not HHDM-mapped.**
`add_region` takes a *virtual* address. For Multiboot2, physical address `p`
maps to virtual `HHDM_OFFSET + p`. Passing the raw physical address as the
virtual address will write page-table node data into physical memory at ~1 MB,
corrupting the Multiboot2 info structure or kernel image.

**5. Registering `TLSF` as `#[global_allocator]` and also keeping an existing one.**
Rust requires exactly one `#[global_allocator]` per binary. Remove or comment
out any existing registration before adding the new one. The compiler error
is clear but easy to miss if the existing registration is in a dependency.

**6. Using `SlabCache<T>` for `T` smaller than 2 bytes.**
The slab free list stores a `u16` index inside each free slot. `T` must be
`size_of::<T>() >= 2`. The `assert_slab_compatible::<T>()` const assertion
catches this at compile time — call it for every type you cache.

**7. Adding usable regions above `BOOT_MAPPED_PHYS` to the buddy before extending page tables.**
The buddy hands out virtual addresses. If a region is above 2 MB physical,
its HHDM virtual address (`0xFFFF_FFFF_8020_0000` and above) is not mapped
by the boot page tables. The buddy will write free-list node data to an
unmapped address → page fault → triple fault → reboot with no diagnostic.
The `allocator_init` function in `main.rs` already guards against this for the
`boot-multiboot2` feature; do not remove that guard.

**8. Forgetting `#![feature(alloc_error_handler)]` with a custom OOM handler.**
If you define `#[alloc_error_handler]`, the crate must be compiled with
`#![feature(alloc_error_handler)]` on nightly. Without it the feature gate
error message correctly identifies the problem but appears far from the handler
definition. Add the feature flag at the top of `barnacle/src/main.rs`.

**9. Import path drift between `framework` and `shared`.**
The generated files import from `shared::`. After transplanting, these become
`framework::` (or whatever your crate is named). A global search-replace of
`use shared::` → `use framework::` (or `use crate::framework::` depending on
structure) is required. Missing one import produces a cryptic "unresolved import"
error that does not mention the rename.

**10. Assuming `boot.asm`'s `rdi = ebx` zero-extension is automatic everywhere.**
It is — on x86_64, a 32-bit register write (`mov edi, ebx`) always
zero-extends to 64 bits. This is architectural, not a NASM quirk. Do not add
sign-extension logic in `kernel_main`. The physical MBI address from GRUB fits
in 32 bits (GRUB places it below 4 GiB), so zero-extension is correct.

---

## Build commands

```bash
# Kernel — Multiboot2 (your existing boot path)
cargo build -p kernel --features boot-multiboot2 \
  --target kernel/x86_64-bare-metal.json -Z build-std=core,alloc

# Kernel — Limine (alternative)
cargo build -p kernel --features boot-limine \
  --target kernel/x86_64-bare-metal.json -Z build-std=core,alloc

# Userspace — mimalloc binary
cargo build -p app --release

# Userspace — profiled library (requires jemalloc C toolchain)
cargo build -p profiled --release
```

For the existing `xtask` build system: add the `--features boot-multiboot2`
flag to whichever `cargo build` invocation produces the kernel ELF that GRUB
loads. The feature selects `multiboot2.rs` as the entry point and pulls in the
`multiboot2` crate dependency.

---

## Verification checklist

Before declaring integration complete:

- [ ] `nm barnacle.elf | grep kernel_main` shows exactly one symbol
- [ ] QEMU boots to `kernel_main_post_heap` without triple-fault
- [ ] `BUDDY.lock().stats().used_bytes > 0` after `allocator_init`
- [ ] `Box::new(42u64)` in `kernel_main_post_heap` does not panic
- [ ] `SlabCache<TaskControlBlock>::alloc()` returns `Some(_)`
- [ ] `BUDDY.lock().stats()` and `TLSF` stats are readable via serial
- [ ] Userspace: `cargo run -p app` prints throughput numbers
- [ ] Userspace: `HeapProfiler::new().snapshot()` returns non-zero `used_bytes`
