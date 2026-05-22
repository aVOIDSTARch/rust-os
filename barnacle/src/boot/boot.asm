;;; barnacle/src/boot/boot.asm
;;;
;;; Multiboot2 compliance header + 32-bit → 64-bit transition stub.
;;;
;;; GRUB loads the kernel ELF, validates the Multiboot2 header,
;;; then jumps to _start in 32-bit protected mode with:
;;;   eax = 0x36d76289  (Multiboot2 magic)
;;;   ebx = physical address of Multiboot2 information structure
;;;   interrupts disabled, flat 4GB address space, no paging
;;;
;;; This stub:
;;;   1. Verifies the magic value (halts with VGA "MB"/"NI"/"NL" on failure)
;;;   2. Checks CPUID and long-mode availability
;;;   3. Sets up 4-level page tables (identity + higher-half, 2MB huge pages)
;;;   4. Enables PAE, sets EFER.LME, enables paging → enters long mode
;;;   5. Far-jumps to the 64-bit trampoline (still identity-mapped at ~1MB)
;;;   6. Absolute 64-bit jump to the higher-half Rust entry point
;;;
;;; Register convention at Rust entry (kernel_main):
;;;   rdi = physical address of Multiboot2 info structure (SysV ABI arg 1)

global _start
extern kernel_main

;;; ─── Multiboot2 compliance header ────────────────────────────────────────────
;;;
;;; Must appear within the first 32,768 bytes of the kernel image and be
;;; 64-bit aligned.  GRUB validates magic + arch + length + checksum = 0 mod 2³².
;;; NASM truncates the checksum expression to 32 bits automatically.

section .multiboot_header progbits alloc noexec nowrite
align 8
header_start:
    dd 0xe85250d6                                           ; Multiboot2 magic
    dd 0                                                    ; architecture: i386 (protected mode)
    dd header_end - header_start                            ; header length
    dd -(0xe85250d6 + 0 + (header_end - header_start))     ; checksum (mod 2^32)

    ; Framebuffer request tag (type=5, flags=1 means optional)
    dw 5                ; type
    dw 1                ; flags: optional
    dd 20               ; size of this tag
    dd 1280             ; preferred width  (0 = no preference)
    dd 720              ; preferred height
    dd 32               ; preferred depth (bits per pixel)

    ; End tag (type=0, required)
    align 8
    dw 0                ; type: end
    dw 0                ; flags
    dd 8                ; size
header_end:

;;; ─── 32-bit boot stub ────────────────────────────────────────────────────────
;;;
;;; All labels here are global (no dot prefix) to avoid NASM local-label
;;; scoping confusion across bits-32 / bits-64 boundaries.

section .text.boot progbits alloc exec nowrite
bits 32

_start:
    ; Verify that GRUB loaded us via Multiboot2.
    cmp eax, 0x36d76289
    jne halt_no_multiboot

    ; Save the Multiboot2 info pointer.
    ; In x86_64 SysV ABI the first integer argument is rdi.
    ; Writing edi zero-extends to rdi on x86_64.
    mov edi, ebx

    ; Set up the bootstrap stack (in .bss.boot, identity-mapped).
    mov esp, stack_top

    cld

    call check_cpuid
    call check_long_mode
    call setup_page_tables
    call enable_long_mode
    ; unreachable: enable_long_mode far-jumps to long_mode_trampoline

halt_no_multiboot:
    ; Write "MB" in white-on-red to VGA cell 0 and halt.
    mov dword [0xb8000], 0x4f424f4d   ; 'MB'
    hlt

;;; ─── CPUID check ─────────────────────────────────────────────────────────────

check_cpuid:
    pushfd
    pop eax
    mov ecx, eax
    xor eax, 1 << 21        ; flip EFLAGS ID bit
    push eax
    popfd
    pushfd
    pop eax
    push ecx
    popfd
    cmp eax, ecx
    je halt_no_cpuid
    ret

halt_no_cpuid:
    mov dword [0xb8000], 0x4f494f4e   ; 'NI' (No CPUID)
    hlt

;;; ─── Long-mode check ─────────────────────────────────────────────────────────

check_long_mode:
    mov eax, 0x80000000
    cpuid
    cmp eax, 0x80000001
    jb halt_no_long_mode

    mov eax, 0x80000001
    cpuid
    test edx, 1 << 29       ; LM bit
    jz halt_no_long_mode
    ret

halt_no_long_mode:
    mov dword [0xb8000], 0x4f4c4f4e   ; 'NL' (No Long mode)
    hlt

;;; ─── Page table setup ────────────────────────────────────────────────────────
;;;
;;; Maps two regions via a single shared 2MB huge-page PD entry:
;;;   Virtual [0x0000000000000000, 0x0000000000200000) → physical [0, 2MB)
;;;   Virtual [0xFFFFFFFF80000000, 0xFFFFFFFF80200000) → physical [0, 2MB)
;;;
;;; pml4/pdpt_low/pdpt_high/pd are in .bss.boot (VMA = LMA = physical ~1MB).
;;; Their addresses fit in 32 bits, so 32-bit code can reference them directly.

setup_page_tables:
    ; PML4[0] → pdpt_low  (identity map)
    mov eax, pdpt_low
    or eax, 0b11            ; present + writable
    mov [pml4], eax

    ; PML4[511] → pdpt_high  (higher-half: 0xFFFFFFFF80000000)
    mov eax, pdpt_high
    or eax, 0b11
    mov [pml4 + 511 * 8], eax

    ; pdpt_low[0] → pd
    mov eax, pd
    or eax, 0b11
    mov [pdpt_low], eax

    ; pdpt_high[510] → pd  (index 510 maps 0xFFFFFFFF80000000)
    mov eax, pd
    or eax, 0b11
    mov [pdpt_high + 510 * 8], eax

    ; pd[0] = 2MB huge page at physical 0  (present + writable + huge)
    mov dword [pd], 0b10000011

    ret

;;; ─── Long-mode activation ────────────────────────────────────────────────────

enable_long_mode:
    ; Point CR3 at the PML4.
    mov eax, pml4
    mov cr3, eax

    ; Enable Physical Address Extension (PAE).
    mov eax, cr4
    or eax, 1 << 5
    mov cr4, eax

    ; Set Long Mode Enable (LME) in EFER MSR.
    mov ecx, 0xC0000080
    rdmsr
    or eax, 1 << 8
    wrmsr

    ; Enable paging → activates long mode (LME already set).
    mov eax, cr0
    or eax, 1 << 31
    mov cr0, eax

    ; Load the minimal 64-bit GDT and far-jump into the 64-bit trampoline.
    ; long_mode_trampoline is in .text.boot64 at physical ~1MB (fits in 32 bits).
    lgdt [gdt64_pointer]
    jmp gdt64_code_sel:long_mode_trampoline

;;; ─── Minimal GDT for long mode ───────────────────────────────────────────────
;;;
;;; gdt64_code_sel is the byte offset of the code descriptor, which doubles as
;;; the segment selector (RPL=0, TI=0).  In long mode the CPU ignores
;;; base/limit; only type bits matter.

gdt64:
    dq 0                                                    ; null descriptor
gdt64_code_sel equ $ - gdt64                               ; = 8 (segment selector)
    dq (1 << 43) | (1 << 44) | (1 << 47) | (1 << 53)      ; 64-bit code: execute, present, L=1
gdt64_pointer:
    dw $ - gdt64 - 1                                        ; GDT limit
    dd gdt64                                                ; GDT base (32-bit physical)

;;; ─── 64-bit long-mode trampoline ─────────────────────────────────────────────
;;;
;;; Separate section so bits-64 assembly does not mix with bits-32 in
;;; .text.boot (avoids NASM label-redef-late errors from bits switching).
;;; Still placed at physical ~1MB by the linker (before KERNEL_OFFSET) so the
;;; 32-bit far jump above can encode its address in 32 bits.

section .text.boot64 progbits alloc exec nowrite
bits 64
long_mode_trampoline:
    ; Reload data segments with the null descriptor (long mode ignores them).
    mov ax, 0
    mov ss, ax
    mov ds, ax
    mov es, ax
    mov fs, ax
    mov gs, ax

    ; rdi already holds the Multiboot2 info physical address (set from edi in
    ; 32-bit mode; x86_64 zero-extends 32-bit writes to 64-bit registers).

    ; Absolute 64-bit jump to the higher-half Rust entry point.
    ; A relative near jump cannot span the gap from ~1MB to ~0xFFFFFFFF80100000.
    mov rax, long_mode_start
    jmp rax

;;; ─── Higher-half 64-bit entry ────────────────────────────────────────────────
;;;
;;; In .text (VMA = KERNEL_OFFSET + LMA).  Rust-compiled code starts here.

section .text
bits 64
long_mode_start:
    ; rdi = Multiboot2 info physical address
    ; rsp = identity-mapped bootstrap stack (valid via both page-table regions)
    call kernel_main

halt_loop:
    hlt
    jmp halt_loop

;;; ─── Bootstrap page tables + stack ──────────────────────────────────────────
;;;
;;; In .bss.boot (SHT_NOBITS, VMA = LMA = physical ~1MB).
;;; GRUB zeroes NOBITS sections: all PTE present-bits start at 0.
;;; Physical addresses of these symbols fit in 32 bits — required by 32-bit
;;; setup code that references them directly via mov eax, <label>.

section .bss.boot nobits
align 4096
pml4:
    resb 4096
pdpt_low:
    resb 4096
pdpt_high:
    resb 4096
pd:
    resb 4096
stack_bottom:
    resb 65536          ; 64 KB bootstrap stack
stack_top:
