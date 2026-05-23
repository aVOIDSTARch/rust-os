/* crusty_os/limine.ld
 *
 * Higher-half linker script for the Limine boot protocol.
 * Limine handles HHDM setup before calling _start, so no LMA/VMA split needed.
 *
 * The .limine_requests section MUST precede .text so the bootloader can scan
 * for request structures before transferring control to the kernel entry point.
 */

OUTPUT_FORMAT("elf64-x86-64")
OUTPUT_ARCH(x86_64)

BASE_ADDRESS = 0xFFFFFFFF80000000;

ENTRY(_start)

SECTIONS {
    . = BASE_ADDRESS;

    /* Limine protocol: all request statics must be in this section so the
     * bootloader can locate them via the start/end markers. */
    .limine_requests : ALIGN(8) {
        KEEP(*(.limine_requests_start))
        KEEP(*(.limine_requests))
        KEEP(*(.limine_requests_end))
    }

    .text ALIGN(4096) : {
        *(.text .text.*)
    }

    . = ALIGN(4096);
    .rodata : {
        *(.rodata .rodata.*)
    }

    . = ALIGN(4096);
    .data : {
        *(.data .data.*)
    }

    . = ALIGN(4096);
    .bss : {
        _bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        _bss_end = .;
    }

    /DISCARD/ : {
        *(.comment)
        *(.eh_frame)
        *(.note .note.*)
    }
}
