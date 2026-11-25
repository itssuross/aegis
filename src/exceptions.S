.section .text.exceptions, "ax"

.macro vector_entry handler
    b \handler
    .balign 0x80
.endm

.macro save_trap_frame
    // Allocate 272-byte (0x110, 16-byte aligned) TrapFrame on the kernel stack.
    sub sp, sp, #272
    stp x0,  x1,  [sp, #0]
    stp x2,  x3,  [sp, #16]
    stp x4,  x5,  [sp, #32]
    stp x6,  x7,  [sp, #48]
    stp x8,  x9,  [sp, #64]
    stp x10, x11, [sp, #80]
    stp x12, x13, [sp, #96]
    stp x14, x15, [sp, #112]
    stp x16, x17, [sp, #128]
    stp x18, x19, [sp, #144]
    stp x20, x21, [sp, #160]
    stp x22, x23, [sp, #176]
    stp x24, x25, [sp, #192]
    stp x26, x27, [sp, #208]
    stp x28, x29, [sp, #224]
    mrs x9,  sp_el0
    stp x30, x9,  [sp, #240]
    mrs x9,  elr_el1
    mrs x10, spsr_el1
    stp x9,  x10, [sp, #256]
.endm

.macro unhandled_stub idx
unhandled_vec_\idx:
    save_trap_frame
    mov x0, sp
    mov x1, #\idx
    bl handle_unhandled_exception
    b exception_return
.endm

.balign 0x800
.global exception_vector_table
exception_vector_table:
    // Current EL with SP_EL0 (entries 0..3)
    vector_entry unhandled_vec_0
    vector_entry unhandled_vec_1
    vector_entry unhandled_vec_2
    vector_entry unhandled_vec_3

    // Current EL with SP_ELx (entries 4..7)
    vector_entry el1h_sync
    vector_entry el1h_irq
    vector_entry unhandled_vec_6
    vector_entry unhandled_vec_7

    // Lower EL using AArch64 (entries 8..11)
    vector_entry unhandled_vec_8
    vector_entry unhandled_vec_9
    vector_entry unhandled_vec_10
    vector_entry unhandled_vec_11

    // Lower EL using AArch32 (entries 12..15)
    vector_entry unhandled_vec_12
    vector_entry unhandled_vec_13
    vector_entry unhandled_vec_14
    vector_entry unhandled_vec_15

el1h_sync:
    save_trap_frame
    mov x0, sp
    bl handle_sync_exception
    b exception_return

el1h_irq:
    save_trap_frame
    mov x0, sp
    bl handle_irq_exception
    b exception_return

unhandled_stub 0
unhandled_stub 1
unhandled_stub 2
unhandled_stub 3
unhandled_stub 6
unhandled_stub 7
unhandled_stub 8
unhandled_stub 9
unhandled_stub 10
unhandled_stub 11
unhandled_stub 12
unhandled_stub 13
unhandled_stub 14
unhandled_stub 15

exception_return:
    ldp x9,  x10, [sp, #256]
    msr elr_el1, x9
    msr spsr_el1, x10
    ldp x30, x9,  [sp, #240]
    msr sp_el0, x9
    ldp x28, x29, [sp, #224]
    ldp x26, x27, [sp, #208]
    ldp x24, x25, [sp, #192]
    ldp x22, x23, [sp, #176]
    ldp x20, x21, [sp, #160]
    ldp x18, x19, [sp, #144]
    ldp x16, x17, [sp, #128]
    ldp x14, x15, [sp, #112]
    ldp x12, x13, [sp, #96]
    ldp x10, x11, [sp, #80]
    ldp x8,  x9,  [sp, #64]
    ldp x6,  x7,  [sp, #48]
    ldp x4,  x5,  [sp, #32]
    ldp x2,  x3,  [sp, #16]
    ldp x0,  x1,  [sp, #0]
    add sp, sp, #272
    eret
