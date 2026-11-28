ENTRY(__kernel_phys_entry)

PHYS_OFFSET = 0xFFFFFF8000000000;

SECTIONS
{
    . = 0xFFFFFF8040080000;
    __kernel_virt_start = .;

    .text ALIGN(4096) : AT(ADDR(.text) - PHYS_OFFSET) {
        KEEP(*(.text.boot))
        *(.text .text.*)
    }

    .rodata ALIGN(4096) : AT(ADDR(.rodata) - PHYS_OFFSET) {
        *(.rodata .rodata.*)
    }

    .data ALIGN(4096) : AT(ADDR(.data) - PHYS_OFFSET) {
        *(.data .data.*)
        *(.got .got.*)
    }

    .bss ALIGN(4096) (NOLOAD) : AT(ADDR(.bss) - PHYS_OFFSET) {
        . = ALIGN(16);
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        . = ALIGN(16);
        __bss_end = .;
    }

    .stack ALIGN(16) (NOLOAD) : AT(ADDR(.stack) - PHYS_OFFSET) {
        . = ALIGN(16);
        __stack_bottom = .;
        . += 65536;
        . = ALIGN(16);
        __stack_top = .;
    }

    . = ALIGN(4096);
    __kernel_virt_end = .;
    __kernel_phys_start = __kernel_virt_start - PHYS_OFFSET;
    __kernel_phys_end = __kernel_virt_end - PHYS_OFFSET;
    __kernel_phys_entry = _start - PHYS_OFFSET;

    __heap_start = .;
    . += 16 * 1024 * 1024;
    . = ALIGN(4096);
    __heap_end = .;

    /DISCARD/ : {
        *(.comment)
        *(.note*)
        *(.eh_frame*)
    }
}
