ENTRY(_start)

SECTIONS
{
    /* QEMU virt -kernel load address */
    . = 0x40080000;

    .text : ALIGN(4096) {
        KEEP(*(.text.boot))
        *(.text .text.*)
    }

    .rodata : ALIGN(4096) {
        *(.rodata .rodata.*)
    }

    .data : ALIGN(4096) {
        *(.data .data.*)
    }

    .bss (NOLOAD) : ALIGN(16) {
        . = ALIGN(16);
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        . = ALIGN(16);
        __bss_end = .;
    }

    /* 64 KiB (65536 bytes) 16-byte aligned kernel stack */
    .stack (NOLOAD) : ALIGN(16) {
        . = ALIGN(16);
        __stack_bottom = .;
        . += 65536;
        . = ALIGN(16);
        __stack_top = .;
    }

    . = ALIGN(4096);
    __kernel_end = .;

    /* 16 MiB page-aligned kernel heap */
    . = ALIGN(4096);
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
