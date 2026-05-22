/* kernel/linker.ld
 *
 * Bare-metal x86_64 linker script.
 * Loads the kernel at the higher-half virtual address expected by Limine
 * (0xFFFFFFFF80000000 + physical load offset).  Adjust BASE_ADDRESS to match
 * your Limine config's kernel_address directive.
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

    /* Discard debug info in release; keep for dev. */
    /DISCARD/ : {
        *(.comment)
        *(.eh_frame)
        *(.note .note.*)
    }
}
