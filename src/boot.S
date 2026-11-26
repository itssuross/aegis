.section .text.boot
.global _start

_start:
    mrs x0, mpidr_el1
    and x0, x0, #0xFF
    cbnz x0, park

    mrs x0, CurrentEL
    lsr x0, x0, #2
    cmp x0, #2
    b.ne el1_entry

    mrs x0, hcr_el2
    orr x0, x0, #(1 << 31) // HCR_EL2.RW
    msr hcr_el2, x0

    mov x0, #0x3c5 // DAIF masked, EL1h
    msr spsr_el2, x0

    adr x0, el1_entry
    msr elr_el2, x0
    eret

el1_entry:
    mov x0, #(3 << 20) // CPACR_EL1.FPEN
    msr cpacr_el1, x0
    isb

    adrp x0, __stack_top
    add x0, x0, :lo12:__stack_top
    mov sp, x0

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
    bl kmain

park:
    wfe
    b park
