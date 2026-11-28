.section .text.boot
.global _start

_start:
    mrs x1, mpidr_el1
    and x1, x1, #0xff // Aff0 core ID
    cbnz x1, .Lpark_core

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
    bl boot_enable_higher_half_mmu
    bl .Lset_higher_half_addresses
    ldr x1, =kmain
    br x1

.global secondary_entry
secondary_entry:
    msr daifset, #0xf // mask D, A, I, F
    mov x19, x0

    mov x1, #(3 << 20) // CPACR_EL1.FPEN
    msr cpacr_el1, x1
    isb

    sub x1, x0, #1
    lsl x1, x1, #14 // 16384 bytes per core
    adrp x2, secondary_stacks
    add x2, x2, :lo12:secondary_stacks
    add x2, x2, x1
    add x2, x2, #16384
    mov sp, x2

    bl boot_enable_higher_half_mmu
    bl .Lset_higher_half_addresses
    mov x0, x19
    ldr x1, =secondary_main
    br x1

.Lset_higher_half_addresses:
    mov x2, #0xffffff8000000000 // PHYS_OFFSET
    add sp, sp, x2
    adrp x1, exception_vector_table
    add x1, x1, :lo12:exception_vector_table
    add x1, x1, x2
    msr vbar_el1, x1
    isb
    ret

.Lpark_core:
    wfe
    b .Lpark_core

.section .bss
.balign 16
secondary_stacks:
    .space 3 * 16384
