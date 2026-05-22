# Multiboot2-Compliant Bootloader Design for a Rust x86_64 OS

-----

## 1. Threat Model and Architectural Constraints

Before designing anything, understand the fundamental tension: **Multiboot2 leaves the CPU in 32-bit protected mode**. Your kernel is x86_64. This is not optional complexity — it is unavoidable. Your bootloader must:

1. Satisfy the Multiboot2 specification so GRUB can load it
1. Transition the CPU from 32-bit protected mode → 64-bit long mode independently
1. Establish a minimal, known-good machine state before handing off to Rust

You have two viable paths:

|Approach                                              |Tradeoff                                                                                    |
|------------------------------------------------------|--------------------------------------------------------------------------------------------|
|**Write the full bootloader yourself** (this document)|Full control; significant complexity in the assembly stub                                   |
|**Use `bootloader` crate or `limine`**                |Dramatically simpler; less educational; Limine is arguably superior to Multiboot2 for x86_64|

This design assumes you are **writing it yourself**, which is the only way to truly understand what you’re building.

-----

## 2. Multiboot2 Specification Compliance

### 2.1 Header Structure

The Multiboot2 header must appear within the **first 32,768 bytes** of the kernel image and be **64-bit aligned**. It consists of a fixed header followed by a sequence of tags terminated by an end tag.

```rust
// src/boot/multiboot2.rs  (compiled as a data section, not executed)
// These are emitted via inline assembly or a linker section.

// Magic: 0xE85250D6
// Architecture: 0 (i386/protected mode entry)
// Header length: computed
// Checksum: -(magic + arch + length) & 0xFFFFFFFF
```

In assembly (your `boot.asm` or `boot.s`):

```nasm
section .multiboot_header
header_start:
    dd 0xe85250d6               ; Multiboot2 magic
    dd 0                        ; architecture: i386 protected mode
    dd header_end - header_start ; header length
    dd 0x100000000 - (0xe85250d6 + 0 + (header_end - header_start))  ; checksum

    ; --- Framebuffer request tag (optional but useful) ---
    dw 5                        ; type: framebuffer
    dw 1                        ; flags: optional
    dd 20                       ; size
    dd 1280                     ; width (0 = no preference)
    dd 720                      ; height
    dd 32                       ; depth (bits per pixel)

    ; --- End tag (required) ---
    dw 0                        ; type: end
    dw 0                        ; flags
    dd 8                        ; size
header_end:
```

**Critical**: The checksum must make `magic + arch + length + checksum` wrap to zero modulo 2³². Get this wrong and GRUB silently refuses to load your kernel.

### 2.2 Information Structure Tags You Must Handle

When GRUB transfers control, `ebx` contains a physical address pointing to the Multiboot2 information structure. You must parse these tags in Rust:

|Tag Type         |Value|Contents                         |Priority     |
|-----------------|-----|---------------------------------|-------------|
|Memory map       |6    |Physical memory regions + types  |**Mandatory**|
|Boot command line|1    |Kernel cmdline string            |High         |
|ELF sections     |9    |Section headers for debugging    |High         |
|Basic memory info|4    |`mem_lower`, `mem_upper` (legacy)|Low          |
|ACPI RSDP v1     |14   |ACPI root pointer                |High         |
|ACPI RSDP v2     |15   |ACPI root pointer (XSDT)         |High         |
|Framebuffer info |8    |Address, pitch, dimensions       |Medium       |

Define these in Rust as `repr(C)` structs. The `multiboot2` crate (`crates.io`) handles parsing if you don’t want to write it by hand — but understand what it’s doing.

-----

## 3. The Assembly Stub: 32-bit → 64-bit Transition

This is the most dangerous and unforgiving part. GRUB drops you into 32-bit protected mode with:

- Interrupts disabled
- A flat 4GB address space (32-bit)
- No paging
- No long mode
- `eax` = `0x36d76289` (Multiboot2 magic, verify this)
- `ebx` = physical address of Multiboot2 info structure

### 3.1 Verification

```nasm
section .text.boot
bits 32
global _start

_start:
    ; Validate Multiboot2 magic
    cmp eax, 0x36d76289
    jne .no_multiboot

    ; Save ebx (Multiboot2 info pointer) — it will be clobbered
    mov edi, ebx        ; first argument to kernel in 64-bit SysV ABI

    ; Set up a known stack (before anything else)
    mov esp, stack_top

    ; Clear direction flag
    cld
    jmp .check_cpuid

.no_multiboot:
    ; Write error to VGA and halt — you have nothing else at this point
    mov dword [0xb8000], 0x4f524f45  ; 'ER' in red
    mov dword [0xb8004], 0x4f3a4f52  ; 'R:' in red
    hlt
```

### 3.2 CPUID Check

```nasm
.check_cpuid:
    ; CPUID availability: attempt to flip ID bit in EFLAGS
    pushfd
    pop eax
    mov ecx, eax
    xor eax, 1 << 21
    push eax
    popfd
    pushfd
    pop eax
    push ecx
    popfd
    cmp eax, ecx
    je .no_cpuid

.check_long_mode:
    ; Extended CPUID functions available?
    mov eax, 0x80000000
    cpuid
    cmp eax, 0x80000001
    jb .no_long_mode

    ; Long mode available?
    mov eax, 0x80000001
    cpuid
    test edx, 1 << 29
    jz .no_long_mode
```

### 3.3 Paging Setup (Identity Map + Higher-Half)

Before entering long mode, you need paging enabled. Design a **two-region mapping**:

1. **Identity map** the first 2MB (for the transition itself) — use huge 2MB pages to keep the setup minimal
1. **Higher-half map** at `0xFFFFFFFF80000000` (the conventional kernel virtual base) pointing to the same physical pages

```nasm
setup_page_tables:
    ; PML4[0] → PDPT_LOW   (identity)
    ; PML4[511] → PDPT_HIGH (higher half kernel)

    mov eax, pdpt_low
    or eax, 0b11            ; present + writable
    mov [pml4], eax

    mov eax, pdpt_high
    or eax, 0b11
    mov [pml4 + 511 * 8], eax

    ; PDPT_LOW[0] → PD
    mov eax, pd
    or eax, 0b11
    mov [pdpt_low], eax

    ; PDPT_HIGH[510] → PD  (maps to 0xFFFFFFFF80000000)
    mov [pdpt_high + 510 * 8], eax

    ; PD[0] → 2MB huge page at 0x0
    mov dword [pd], 0b10000011     ; present + writable + huge

    ret
```

**Allocate these tables in BSS** (zeroed, so present bits default to 0):

```nasm
section .bss
align 4096
pml4:  resb 4096
pdpt_low:  resb 4096
pdpt_high: resb 4096
pd:    resb 4096

stack_bottom:
    resb 65536          ; 64KB initial stack
stack_top:
```

### 3.4 Entering Long Mode

```nasm
enable_long_mode:
    ; Load CR3 with PML4 address
    mov eax, pml4
    mov cr3, eax

    ; Enable PAE (Physical Address Extension) — required for long mode
    mov eax, cr4
    or eax, 1 << 5
    mov cr4, eax

    ; Set LME bit in EFER MSR
    mov ecx, 0xC0000080
    rdmsr
    or eax, 1 << 8
    wrmsr

    ; Enable paging (activates long mode since LME is set)
    mov eax, cr0
    or eax, 1 << 31
    mov cr0, eax

    ; Far jump into 64-bit code segment to flush instruction pipeline
    lgdt [gdt64.pointer]
    jmp gdt64.code:long_mode_start
```

### 3.5 GDT for Long Mode

```nasm
section .rodata
gdt64:
    dq 0                            ; null descriptor
.code: equ $ - gdt64
    dq (1<<43) | (1<<44) | (1<<47) | (1<<53)  ; code, present, long mode
.pointer:
    dw $ - gdt64 - 1
    dq gdt64
```

### 3.6 64-bit Entry Point

```nasm
section .text
bits 64
long_mode_start:
    ; Reload segment registers with null (long mode ignores most of them)
    mov ax, 0
    mov ss, ax
    mov ds, ax
    mov es, ax
    mov fs, ax
    mov gs, ax

    ; edi still holds the Multiboot2 info pointer (low 32 bits)
    ; Zero-extend it for 64-bit: edi → rdi (already zero-extended by x86_64 semantics)

    ; Call Rust kernel entry
    extern kernel_main
    call kernel_main

    ; Should never return, but halt if it does
    hlt
```

-----

## 4. Linker Script

This is where many attempts collapse. The linker script must:

- Place the Multiboot2 header first
- Separate 32-bit boot code from 64-bit kernel code
- Establish the higher-half virtual addresses while maintaining correct physical offsets
- Align sections appropriately

```ld
/* kernel.ld */

OUTPUT_FORMAT(elf64-x86-64)
ENTRY(_start)

KERNEL_OFFSET = 0xFFFFFFFF80000000;

SECTIONS {
    /* Physical load address: 1MB (avoids legacy BIOS reserved regions) */
    . = 1M;

    /* Multiboot header must be near the start */
    .multiboot_header : {
        KEEP(*(.multiboot_header))
    }

    /* 32-bit boot code: physically mapped, not offset */
    .text.boot : {
        *(.text.boot)
    }

    /* Transition to higher half virtual addresses */
    . += KERNEL_OFFSET;

    .text ALIGN(4K) : AT(ADDR(.text) - KERNEL_OFFSET) {
        *(.text .text.*)
    }

    .rodata ALIGN(4K) : AT(ADDR(.rodata) - KERNEL_OFFSET) {
        *(.rodata .rodata.*)
    }

    .data ALIGN(4K) : AT(ADDR(.data) - KERNEL_OFFSET) {
        *(.data .data.*)
    }

    .bss ALIGN(4K) : AT(ADDR(.bss) - KERNEL_OFFSET) {
        *(COMMON)
        *(.bss .bss.*)
    }

    /DISCARD/ : {
        *(.eh_frame)
        *(.note .note.*)
    }
}
```

**The `AT()` directive** is what separates a working linker script from a non-working one: it specifies the physical load address (LMA) while `ADDR()` gives the virtual address (VMA). GRUB writes to physical addresses; your code runs at virtual addresses.

-----

## 5. Rust Kernel Entry and Boot Information Interface

### 5.1 Cargo Configuration

```toml
# .cargo/config.toml
[build]
target = "x86_64-unknown-none"

[target.x86_64-unknown-none]
rustflags = [
    "-C", "link-arg=-Tkernel.ld",
    "-C", "link-arg=--gc-sections",
]
```

```toml
# Cargo.toml
[profile.dev]
panic = "abort"

[profile.release]
panic = "abort"
lto = true
codegen-units = 1
```

A custom target JSON is often preferable to `x86_64-unknown-none` for fine-grained control (disabling the red zone, disabling SSE before you save/restore SSE state, etc.):

```json
{
  "llvm-target": "x86_64-unknown-none",
  "data-layout": "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-f80:128-n8:16:32:64-S128",
  "arch": "x86_64",
  "os": "none",
  "env": "",
  "vendor": "unknown",
  "linker-flavor": "ld.lld",
  "linker": "rust-lld",
  "features": "-mmx,-sse,-sse2,+soft-float",
  "disable-redzone": true,
  "panic-strategy": "abort"
}
```

**Disabling the red zone is not optional.** The x86_64 SysV ABI reserves 128 bytes below the stack pointer. Hardware interrupts do not respect this. If you enable interrupts before disabling the red zone at the compiler level, you will get silent stack corruption that manifests as inexplicable behavior.

### 5.2 The Rust Entry Point

```rust
// src/main.rs
#![no_std]
#![no_main]
#![deny(unsafe_op_in_unsafe_fn)]

use core::panic::PanicInfo;

mod boot;
mod memory;
mod arch;

/// Called from the assembly stub.
/// `multiboot2_info`: physical address of Multiboot2 info structure.
/// This is `unsafe` because we're receiving raw hardware state.
#[no_mangle]
pub extern "C" fn kernel_main(multiboot2_info: u64) -> ! {
    // SAFETY: multiboot2_info is a valid physical address provided by GRUB,
    // and paging has been configured such that the first 2MB is identity-mapped.
    let boot_info = unsafe {
        boot::multiboot2::parse(multiboot2_info as *const u8)
    };

    // Initialize architecture-level structures first
    arch::gdt::init();           // Proper GDT (not the bootstrap one)
    arch::idt::init();           // Exception/interrupt handlers

    // Initialize memory management using the Multiboot2 memory map
    let memory_map = boot_info.memory_map()
        .expect("Bootloader did not provide a memory map");

    memory::init(memory_map);

    // Now safe to enable interrupts
    x86_64::instructions::interrupts::enable();

    loop {
        x86_64::instructions::hlt();
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // At minimum, write to VGA text buffer — no heap allocation possible here
    // In debug builds, also write to serial port
    loop {
        x86_64::instructions::hlt();
    }
}
```

### 5.3 Multiboot2 Parser

```rust
// src/boot/multiboot2.rs

#[repr(C)]
struct Mb2Header {
    total_size: u32,
    _reserved: u32,
}

#[repr(C)]
struct Mb2Tag {
    tag_type: u32,
    size: u32,
}

pub struct BootInfo {
    ptr: *const u8,
    total_size: u32,
}

impl BootInfo {
    pub fn memory_map(&self) -> Option<&Mb2MemoryMap> {
        self.find_tag(6)
            .map(|tag| unsafe { &*(tag as *const Mb2Tag as *const Mb2MemoryMap) })
    }

    fn find_tag(&self, tag_type: u32) -> Option<*const Mb2Tag> {
        // Walk the tag list
        let mut offset = 8usize; // skip header
        loop {
            let tag = unsafe {
                &*((self.ptr as usize + offset) as *const Mb2Tag)
            };
            if tag.tag_type == 0 { return None; }  // end tag
            if tag.tag_type == tag_type {
                return Some(tag as *const Mb2Tag);
            }
            // Tags are 8-byte aligned
            offset += (tag.size as usize + 7) & !7;
            if offset >= self.total_size as usize { return None; }
        }
    }
}

pub unsafe fn parse(ptr: *const u8) -> BootInfo {
    let header = &*(ptr as *const Mb2Header);
    assert_eq!(
        (ptr as usize) % 8, 0,
        "Multiboot2 info structure is not 8-byte aligned"
    );
    BootInfo { ptr, total_size: header.total_size }
}
```

-----

## 6. Build System

### 6.1 The Build Problem

`cargo build` alone cannot produce a bootable image. You need:

1. The Rust kernel compiled to an ELF64 binary
1. The assembly stub compiled and linked in
1. The ELF stripped to a flat binary or left as ELF (GRUB can load ELF directly)
1. A GRUB configuration + ISO image

### 6.2 build.rs Integration

```rust
// build.rs
fn main() {
    // Compile the assembly stub
    println!("cargo:rerun-if-changed=src/boot/boot.asm");

    let output = std::process::Command::new("nasm")
        .args(["-f", "elf64", "src/boot/boot.asm", "-o", "target/boot.o"])
        .output()
        .expect("nasm not found — install nasm");

    if !output.status.success() {
        panic!("NASM failed: {}", String::from_utf8_lossy(&output.stderr));
    }

    println!("cargo:rustc-link-arg=target/boot.o");
    println!("cargo:rustc-link-arg=-Tkernel.ld");
}
```

### 6.3 GRUB Configuration and ISO Creation

```
# grub.cfg
set timeout=0
set default=0

menuentry "MyOS" {
    multiboot2 /boot/kernel.elf
    boot
}
```

```makefile
# Makefile
KERNEL := target/x86_64-unknown-none/release/myos

.PHONY: iso run

$(KERNEL):
	cargo build --release

iso: $(KERNEL)
	mkdir -p iso/boot/grub
	cp $(KERNEL) iso/boot/kernel.elf
	cp grub.cfg iso/boot/grub/grub.cfg
	grub-mkrescue -o myos.iso iso

run: iso
	qemu-system-x86_64 \
		-cdrom myos.iso \
		-serial stdio \
		-no-reboot \
		-no-shutdown \
		-m 128M
```

-----

## 7. Post-Boot Initialization Sequence

Once `kernel_main` runs, the required initialization order is strict:

```
1. Parse Multiboot2 info (before any allocations)
2. Initialize proper GDT (the bootstrap GDT has no TSS)
3. Initialize IDT with at least #PF, #GP, #DF handlers
4. Initialize frame allocator from Multiboot2 memory map
5. Initialize virtual memory manager (remap kernel, set up heap region)
6. Initialize kernel heap allocator (enables Box, Vec, etc.)
7. Initialize APIC (disable PIC first, then configure APIC)
8. Enable interrupts
9. Initialize drivers, subsystems, etc.
```

**Do not deviate from this order.** Enabling interrupts before you have an IDT will triple-fault. Allocating memory before you have a frame allocator will corrupt memory. Using ACPI before you have paging properly set up will produce incorrect reads.

-----

## 8. Common Failure Modes

|Symptom                                       |Likely Cause                                                                                      |
|----------------------------------------------|--------------------------------------------------------------------------------------------------|
|Immediate reboot after GRUB loads             |Triple fault — usually bad GDT, missing IDT, or stack not set up                                  |
|Garbled VGA output                            |Paging misconfiguration, running 64-bit code with 32-bit addresses                                |
|GRUB refuses to find kernel                   |Multiboot2 header not in first 32KB, or bad checksum                                              |
|Works in QEMU, fails on real hardware         |Red zone not disabled, or assuming BIOS memory layout                                             |
|Page fault immediately after jump to long mode|Identity map covers physical range but virtual addresses not correct — check linker script LMA/VMA|

-----

## 9. Recommended Crates

|Crate                  |Purpose                                             |
|-----------------------|----------------------------------------------------|
|`x86_64`               |Port I/O, GDT/IDT builders, paging types, MSR access|
|`multiboot2`           |Multiboot2 info structure parsing                   |
|`uart_16550`           |Serial port driver for debug output                 |
|`pic8259`              |PIC controller (needed to silence it before APIC)   |
|`acpi`                 |ACPI table parsing (for APIC, HPET, etc.)           |
|`linked_list_allocator`|Simple heap allocator to get `alloc` working        |

-----

## Summary

The design has four non-negotiable layers: the **Multiboot2 compliance header** (checksum must be correct, header must be in the first 32KB), the **32-bit assembly stub** (verifies magic, sets up page tables, enables long mode), the **linker script** (separates LMA from VMA with `AT()`, places sections correctly), and the **Rust kernel entry** (disables red zone at the target level, parses boot info before touching any hardware). Everything else — memory management, interrupt handling, device drivers — is downstream of getting these four right.

The most reliable way to validate this before writing a line of Rust is to get GRUB to boot a minimal C kernel that just writes to VGA memory. Once you can see output, you know the ABI boundary works, and you can replace the C kernel with Rust incrementally.