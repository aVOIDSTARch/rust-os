/* crusty_os/limine.ld
 *
 * Higher-half linker script for the Limine boot protocol.
 * Limine handles HHDM setup before calling _start, so no LMA/VMA split needed.
 */

BASE_ADDRESS = 0xFFFFFFFF80000000;

ENTRY(_start)

SECTIONS {
    . = BASE_ADDRESS;

    .text : {
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
