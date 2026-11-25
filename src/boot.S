.section .text.boot
.global _start

_start:
    // Read MPIDR_EL1 and park any secondary cores (Aff0 != 0)
    mrs x0, mpidr_el1
    and x0, x0, #0xFF
    cbnz x0, park

    // Check CurrentEL (bits 3:2) and transition from EL2 to EL1 if needed
    mrs x0, CurrentEL
    lsr x0, x0, #2
    cmp x0, #2
    b.ne el1_entry

    // Configure HCR_EL2.RW = 1 so EL1 executes in AArch64
    mrs x0, hcr_el2
    orr x0, x0, #(1 << 31)
    msr hcr_el2, x0

    // Set SPSR_EL2 for EL1h with DAIF interrupts masked (0x3c5)
    mov x0, #0x3c5
    msr spsr_el2, x0

    // Return into EL1 at el1_entry
    adr x0, el1_entry
    msr elr_el2, x0
    eret

el1_entry:
    // Enable FP/SIMD access at EL1 (CPACR_EL1.FPEN = 0b11)
    mov x0, #(3 << 20)
    msr cpacr_el1, x0
    isb

    // Initialize stack pointer to the top of the 64 KiB kernel stack
    adrp x0, __stack_top
    add x0, x0, :lo12:__stack_top
    mov sp, x0

    // Zero out the .bss section using 64-bit stores
    adrp x0, __bss_start
    add x0, x0, :lo12:__bss_start
    adrp x1, __bss_end
    add x1, x1, :lo12:__bss_end

bss_zero_loop:
    cmp x0, x1
    b.hs bss_zero_done
    str xzr, [x0], #8
    b bss_zero_loop

bss_zero_done:
    // Jump into Rust kernel entry point
    bl kmain

park:
    wfe
    b park
