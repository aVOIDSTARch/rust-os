# barnacle — Multiboot2 Bootloader Library

## Context

`barnacle` will be a reusable no_std Rust library that any x86_64 kernel can depend on to become Multiboot2-compliant and bootable via GRUB. When a kernel adds `barnacle` as a dependency, barnacle's `build.rs` automatically links in:
- A Multiboot2 compliance header (readable by GRUB)
- A 32-bit assembly stub that transitions the CPU from protected mode → long mode
- A linker script that places the header first and splits LMA/VMA for a higher-half kernel

The kernel defines its entry point with `barnacle::entry_point!(my_fn)` and receives a `&'static BootInfo` wrapping the Multiboot2 information structure GRUB provides.

**Scope**: barnacle standalone + a minimal `test_kernel` for end-to-end validation + an `xtask` for ISO building. crusty_os migration is a separate follow-up.

**Decisions confirmed:**
- `multiboot2 = "0.21"` crate for info-structure parsing (no_std compatible)
- `xtask` workspace member for ISO building with macOS Docker fallback
- barnacle standalone first; no crusty_os changes in this plan

---

## Architecture

```
barnacle (lib)
  ├── build.rs            ← compiles boot.asm via nasm, emits link args
  ├── kernel.ld           ← linker script: LMA/VMA split, KERNEL_OFFSET = 0xFFFFFFFF80000000
  └── src/
      ├── lib.rs          ← #![no_std], entry_point! macro, init()
      ├── info/mod.rs     ← BootInfo wrapping multiboot2::BootInformation<'static>
      ├── info/memory.rs  ← re-exports MemoryMapTag, MemoryArea, MemoryAreaType
      └── boot/boot.asm   ← .multiboot_header + 32-bit _start stub + BSS page tables/stack

test_kernel (bin)         ← minimal demo: entry_point!, VGA write, halt loop
xtask (bin)               ← cargo xtask iso / run / check-deps
```

**Target**: the workspace `x86_64-crusty_os.json` is reused (already has `disable-redzone`, no SSE, `rust-lld`, `panic = "abort"`).
`bootimage runner` is never invoked for barnacle or test_kernel (both suppress tests; xtask handles running).

---

## Critical Files

| File | Action |
|------|--------|
| `barnacle/Cargo.toml` | MODIFY — lib crate, add `multiboot2` dep |
| `barnacle/src/main.rs` | DELETE — replaced by lib |
| `barnacle/build.rs` | CREATE |
| `barnacle/kernel.ld` | CREATE |
| `barnacle/src/lib.rs` | CREATE |
| `barnacle/src/info/mod.rs` | CREATE |
| `barnacle/src/info/memory.rs` | CREATE |
| `barnacle/src/boot/boot.asm` | CREATE |
| `barnacle/.cargo/config.toml` | CREATE — silence bootimage runner for the lib |
| `test_kernel/Cargo.toml` | CREATE |
| `test_kernel/src/main.rs` | CREATE |
| `test_kernel/.cargo/config.toml` | CREATE — override runner |
| `xtask/Cargo.toml` | CREATE |
| `xtask/src/main.rs` | CREATE |
| `Cargo.toml` (workspace root) | MODIFY — add xtask, test_kernel to members |

---

## Step 1 — Modify `barnacle/Cargo.toml`

Convert from bin → lib. Add `multiboot2` dependency and suppress tests/doctests:

```toml
[package]
name    = "barnacle"
version = "0.1.0"
edition = "2024"

[lib]
name    = "barnacle"
path    = "src/lib.rs"
test    = false
doctest = false

[dependencies]
multiboot2 = { version = "0.21", default-features = false }

[build-dependencies]
# none — build.rs uses std only
```

Delete `barnacle/src/main.rs`.

---

## Step 2 — Create `barnacle/build.rs`

Invokes `nasm -f elf64` on `src/boot/boot.asm` and emits three link args:

```rust
use std::{path::PathBuf, process::Command};

fn main() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let out      = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let asm      = manifest.join("src/boot/boot.asm");
    let obj      = out.join("boot.o");
    let ld       = manifest.join("kernel.ld");

    println!("cargo:rerun-if-changed=src/boot/boot.asm");
    println!("cargo:rerun-if-changed=kernel.ld");

    let ok = Command::new("nasm")
        .args(["-f", "elf64", asm.to_str().unwrap(), "-o", obj.to_str().unwrap()])
        .status()
        .expect("nasm not found — install: brew install nasm");
    assert!(ok.success(), "nasm failed to assemble boot.asm");

    println!("cargo:rustc-link-arg={}", obj.display());
    println!("cargo:rustc-link-arg=-T{}", ld.display());
    println!("cargo:rustc-link-arg=--gc-sections");
}
```

---

## Step 3 — Create `barnacle/kernel.ld`

Two-region layout: 32-bit boot code physically at 1MB, 64-bit kernel at `0xFFFFFFFF80000000` VMA.
`AT()` directives ensure GRUB writes to physical addresses while Rust code runs at virtual addresses.

```ld
OUTPUT_FORMAT(elf64-x86-64)
ENTRY(_start)

KERNEL_OFFSET = 0xFFFFFFFF80000000;

SECTIONS {
    . = 1M;

    .multiboot_header : { KEEP(*(.multiboot_header)) }
    .text.boot        : { *(.text.boot) }

    . += KERNEL_OFFSET;

    .text   ALIGN(4K) : AT(ADDR(.text)   - KERNEL_OFFSET) { *(.text   .text.*)   }
    .rodata ALIGN(4K) : AT(ADDR(.rodata) - KERNEL_OFFSET) { *(.rodata .rodata.*) }
    .data   ALIGN(4K) : AT(ADDR(.data)   - KERNEL_OFFSET) { *(.data   .data.*)   }
    .bss    ALIGN(4K) : AT(ADDR(.bss)    - KERNEL_OFFSET) { *(COMMON) *(.bss .bss.*) }

    /DISCARD/ : { *(.eh_frame) *(.note .note.*) }
}
```

---

## Step 4 — Create `barnacle/src/boot/boot.asm`

NASM source compiled to elf64 but containing 32-bit boot sections.

**`.multiboot_header` section** — must appear in first 32KB of kernel image:
- Magic: `0xE85250D6`
- Architecture: `0` (i386 protected mode)
- Length: `header_end - header_start`
- Checksum: `dd -(0xe85250d6 + 0 + (header_end - header_start))` — NASM truncates to 32 bits automatically
- Framebuffer tag (type 5, flags=1 optional): 1280×720×32
- End tag (type 0, size 8)

**`.text.boot` section** — `bits 32`, global `_start`:
1. Compare `eax` to `0x36d76289` — write `ER:` in red to VGA 0xB8000 and `hlt` on mismatch
2. `mov edi, ebx` — save MB2 info ptr (zero-extended to `rdi` in 64-bit, SysV arg 1)
3. `mov esp, stack_top` — establish bootstrap stack
4. `cld`
5. CPUID check (flip EFLAGS.ID, compare before/after)
6. Long-mode check (`cpuid 0x80000001`, EDX bit 29)
7. `call setup_page_tables`:
   - PML4[0] → PDPT_LOW (present + writable) — identity map
   - PML4[511] → PDPT_HIGH (present + writable) — higher-half
   - PDPT_LOW[0] → PD (present + writable)
   - PDPT_HIGH[510] → PD — maps `0xFFFFFFFF80000000` to same PD
   - PD[0] = `0b10000011` — huge 2MB page at physical 0 (present + writable + huge)
8. `call enable_long_mode`:
   - `mov cr3, pml4`
   - Set `cr4.PAE` (bit 5)
   - Set EFER.LME (MSR `0xC0000080`, bit 8)
   - Set `cr0.PG` (bit 31)
   - `lgdt [gdt64.pointer]`
   - Far jump to `gdt64.code:long_mode_start`

**GDT64** in `.rodata`: null descriptor + 64-bit code descriptor + GDTR pointer.

**`.text` section** — `bits 64`, `long_mode_start`:
1. Zero `ax`, reload `ss`, `ds`, `es`, `fs`, `gs`
2. `call kernel_main` (`rdi` = MB2 info addr)
3. `hlt` loop

**`.bss` section** — `align 4096`:
- `pml4`, `pdpt_low`, `pdpt_high`, `pd` — 4KB each (zero = not present by default)
- `stack_bottom` + 65536 bytes → `stack_top`

---

## Step 5 — Create `barnacle/src/lib.rs`

```rust
#![no_std]

pub mod info;
pub use info::BootInfo;

use core::{mem::MaybeUninit, sync::atomic::{AtomicBool, Ordering}};

static TAKEN: AtomicBool = AtomicBool::new(false);
static mut STORAGE: MaybeUninit<BootInfo> = MaybeUninit::uninit();

/// Parse the Multiboot2 info structure and return a `'static` reference.
///
/// # Safety
/// `addr` must be the physical address in `rbx` provided by GRUB.
/// Must be called exactly once. The MB2 structure must remain valid for `'static`.
pub unsafe fn init(addr: u64) -> &'static BootInfo {
    assert!(!TAKEN.swap(true, Ordering::SeqCst), "barnacle::init called twice");
    let raw = unsafe { multiboot2::load(addr as usize) }
        .expect("invalid Multiboot2 info structure");
    // Safety: MB2 structure is in identity-mapped memory valid for the kernel lifetime.
    let raw_static: multiboot2::BootInformation<'static> =
        unsafe { core::mem::transmute(raw) };
    unsafe {
        STORAGE.write(BootInfo::new(raw_static));
        STORAGE.assume_init_ref()
    }
}

/// Define the kernel entry point called by barnacle's assembly stub.
///
/// The provided function must have signature `fn(&'static BootInfo) -> !`.
#[macro_export]
macro_rules! entry_point {
    ($path:path) => {
        #[unsafe(no_mangle)]
        pub extern "C" fn kernel_main(multiboot2_addr: u64) -> ! {
            let f: fn(&'static $crate::BootInfo) -> ! = $path;
            let boot_info = unsafe { $crate::init(multiboot2_addr) };
            f(boot_info)
        }
    };
}
```

---

## Step 6 — Create `barnacle/src/info/mod.rs` and `memory.rs`

**`mod.rs`** — `BootInfo` ergonomic wrapper around `multiboot2::BootInformation<'static>`:
- `memory_map() -> Option<&MemoryMapTag>`
- `command_line() -> Option<&str>`
- `framebuffer() -> Option<&FramebufferTag>`
- `rsdp_v1() -> Option<&RsdpV1Tag>`
- `rsdp_v2() -> Option<&RsdpV2Tag>`

`physical_memory_offset` is intentionally absent — Multiboot2 does not establish a physical memory
mapping; the kernel must build one from the memory map. (Addressed in the crusty_os migration.)

**`memory.rs`** — re-export types kernels need:
```rust
pub use multiboot2::{MemoryMapTag, MemoryArea, MemoryAreaType};
```

---

## Step 7 — Create `barnacle/.cargo/config.toml`

Prevent the workspace `bootimage runner` from applying to this crate:

```toml
[target.'cfg(target_os = "none")']
runner = ""
```

---

## Step 8 — Create `test_kernel`

**`test_kernel/Cargo.toml`**:
```toml
[package]
name    = "test_kernel"
version = "0.1.0"
edition = "2024"

[[bin]]
name = "test_kernel"
path = "src/main.rs"

[dependencies]
barnacle = { path = "../barnacle" }
```

**`test_kernel/.cargo/config.toml`**:
```toml
[target.'cfg(target_os = "none")']
runner = ""
```

**`test_kernel/src/main.rs`** — writes "OK" to VGA and halts:
```rust
#![no_std]
#![no_main]

use barnacle::BootInfo;
use core::panic::PanicInfo;

barnacle::entry_point!(kernel_main);

fn kernel_main(_boot_info: &'static BootInfo) -> ! {
    // Physical 0xB8000 mapped at identity (0xB8000) and higher-half (KERNEL_OFFSET + 0xB8000)
    let vga = (0xFFFFFFFF80000000usize + 0xB8000) as *mut u16;
    unsafe {
        vga.write_volatile(0x0F4F); // 'O' white-on-black
        vga.add(1).write_volatile(0x0F4B); // 'K'
    }
    loop { unsafe { core::arch::asm!("hlt"); } }
}

#[panic_handler]
fn panic(_: &PanicInfo) -> ! {
    loop { unsafe { core::arch::asm!("hlt"); } }
}
```

---

## Step 9 — Create `xtask`

**`xtask/Cargo.toml`**:
```toml
[package]
name    = "xtask"
version = "0.1.0"
edition = "2024"
```

**`xtask/src/main.rs`** — three subcommands using `std::process::Command`:

`cargo xtask iso --kernel <elf_path>` (default: `target/.../test_kernel`):
1. `mkdir -p isoroot/boot/grub`
2. Copy ELF → `isoroot/boot/kernel.elf`
3. Write `grub.cfg`: `set timeout=0; menuentry "barnacle" { multiboot2 /boot/kernel.elf; boot }`
4. Run `grub-mkrescue -o barnacle.iso isoroot` (native) or Docker fallback:
   ```
   docker run --rm -v $PWD:/work ubuntu:22.04 \
       sh -c "apt-get -qq install -y grub-pc-bin xorriso 2>/dev/null && \
              grub-mkrescue -o /work/barnacle.iso /work/isoroot"
   ```

`cargo xtask run [--kernel <elf_path>]`:
1. `cargo build -p test_kernel`
2. Build ISO (above)
3. `qemu-system-x86_64 -cdrom barnacle.iso -serial stdio -no-reboot -no-shutdown -m 128M`

`cargo xtask check-deps` — exits non-zero with a helpful message if `nasm`,
`qemu-system-x86_64`, or `grub-mkrescue`/`docker` are missing.

---

## Step 10 — Update Workspace `Cargo.toml`

```toml
[workspace]
members = ["framework", "platform", "crusty_os", "bitwise", "barnacle", "xtask", "test_kernel"]
resolver = "3"
```

---

## Constraints and Warnings

| Item | Detail |
|------|--------|
| **NASM required** | Hard build-time dep: `brew install nasm` / `apt install nasm` |
| **32-bit code in elf64** | `bits 32` inside `-f elf64` is valid NASM. All page table addresses must be physical (< 4GB) |
| **Checksum** | `dd -(magic + arch + len)` — NASM truncates to 32 bits automatically; do not use `& 0xFFFFFFFF` |
| **`edi` → `rdi` hand-off** | Storing `ebx` in `edi` before long mode means low 32 bits of `rdi` = MB2 info addr. x86_64 zero-extends on 32-bit write — correct for physical addresses < 4GB |
| **Identity map window** | Only first 2MB is identity-mapped. BSS (page tables + stack at ~1MB + ~17KB) falls within that window |
| **`physical_memory_offset` absent** | Multiboot2 gives a memory map, not a pre-built physical→virtual mapping. crusty_os migration must rework `memory::init` |
| **grub-mkrescue on macOS** | Requires `i386-elf-grub` toolchain or Docker. xtask detects and falls back automatically |

---

## Verification Sequence

```bash
# 1. Confirm nasm is installed
nasm --version

# 2. barnacle library compiles (build.rs invokes nasm, links boot.o)
cargo build -p barnacle

# 3. test_kernel ELF builds against barnacle
cargo build -p test_kernel

# 4. Verify ISO tooling is present
cargo xtask check-deps

# 5. Build ISO and boot in QEMU — VGA top-left should show "OK"
cargo xtask run

# 6. QEMU should NOT triple-fault reboot; Ctrl+C exits cleanly
#    Serial output will be empty (test_kernel has no serial driver)
```
