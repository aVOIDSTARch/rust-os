/*
 * kernel.ld — barnacle linker script for higher-half x86_64 kernels
 *
 * Physical layout (LMA): all sections loaded starting at 1MB.
 * Virtual layout (VMA):  64-bit sections mapped at KERNEL_OFFSET + physical.
 *
 * GRUB writes each ELF segment to its LMA; the CPU addresses it via its VMA
 * once paging is active.  The AT() directives encode the LMA for each
 * higher-half section so the linker produces the correct ELF program headers.
 *
 * Identity map (physical 0–2MB = virtual 0–2MB) is set up in boot.asm so
 * that the 32-bit→64-bit transition code can reference itself by physical
 * address before the CPU uses KERNEL_OFFSET addresses.
 */

OUTPUT_FORMAT(elf64-x86-64)
ENTRY(_start)

KERNEL_OFFSET = 0xFFFFFFFF80000000;

SECTIONS {
    /* Physical load base: 1MB clears BIOS/legacy reserved regions. */
    . = 1M;

    /* Multiboot2 compliance header — must be within the first 32KB. */
    .multiboot_header : {
        KEEP(*(.multiboot_header))
    }

    /* 32-bit boot stub + GDT64.
     * VMA = LMA = physical.  Referenced by 32-bit code so addresses
     * must fit in 32 bits (they do: physical ~ 1MB). */
    .text.boot : {
        *(.text.boot)
    }

    /* 64-bit long-mode trampoline (still physical ~1MB, before KERNEL_OFFSET).
     * The 32-bit far jump can address it because VMA = LMA < 4GB.
     * It then does an absolute 64-bit jump to the higher-half Rust entry. */
    .text.boot64 : {
        *(.text.boot64)
    }

    /* Bootstrap page tables + initial stack.
     * Must be in the identity-mapped window (physical < 2MB).
     * VMA = LMA = physical so 32-bit code can address them directly.
     * GRUB zeroes SHT_NOBITS sections, giving us valid "not-present" PTEs. */
    .bss.boot ALIGN(4K) : {
        *(.bss.boot)
    }

    /* ── Higher-half sections ─────────────────────────────────────────────
     * VMA jumps by KERNEL_OFFSET; LMA stays contiguous with the boot data.
     * AT(ADDR(x) - KERNEL_OFFSET) tells the linker the physical load address. */
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

    /* Physical address of the first byte past the kernel image.
     * Used by allocator_init to start the heap above the kernel. */
    _kernel_end_phys = ADDR(.bss) - KERNEL_OFFSET + SIZEOF(.bss);

    /DISCARD/ : {
        *(.eh_frame)
        *(.note .note.*)
    }
}
