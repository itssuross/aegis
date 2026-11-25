use core::arch::asm;

const EC_BRK_AARCH64: u64 = 0x3C;

/// Saved EL1 exception register frame matching the 272-byte stack layout in `exceptions.S`.
#[repr(C)]
#[allow(dead_code)]
pub struct TrapFrame {
    pub regs: [u64; 31],
    pub sp_el0: u64,
    pub elr_el1: u64,
    pub spsr_el1: u64,
}

unsafe extern "C" {
    static exception_vector_table: u8;
}

#[inline]
fn read_esr_el1() -> u64 {
    let esr: u64;
    // SAFETY: Reading `ESR_EL1` is valid at EL1 on AArch64 with no side effects.
    unsafe {
        asm!("mrs {}, esr_el1", out(reg) esr, options(nomem, nostack, preserves_flags));
    }
    esr
}

#[inline]
fn read_far_el1() -> u64 {
    let far: u64;
    // SAFETY: Reading `FAR_EL1` is valid at EL1 on AArch64 with no side effects.
    unsafe {
        asm!("mrs {}, far_el1", out(reg) far, options(nomem, nostack, preserves_flags));
    }
    far
}

pub fn init() {
    // SAFETY: `exception_vector_table` is a 2048-byte aligned EL1 vector table defined in `exceptions.S`.
    // Loading its address into `VBAR_EL1` followed by `isb` activates EL1 exception dispatch.
    unsafe {
        let table_addr = &raw const exception_vector_table as usize;
        asm!(
            "msr vbar_el1, {}",
            "isb",
            in(reg) table_addr,
            options(nomem, nostack, preserves_flags),
        );
    }
}

pub fn enable_irqs() {
    // SAFETY: Clearing DAIF.I unmasks IRQs at EL1 once the vector table and GICv2 are configured.
    unsafe {
        asm!(
            "msr daifclr, #2",
            "isb",
            options(nomem, nostack, preserves_flags),
        );
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn handle_sync_exception(frame: &mut TrapFrame) {
    let esr = read_esr_el1();
    let ec = (esr >> 26) & 0x3F;

    if ec == EC_BRK_AARCH64 {
        crate::kprintln!(
            "[exceptions] caught BRK trap at {:#x}, advancing ELR_EL1",
            frame.elr_el1
        );
        frame.elr_el1 += 4;
        return;
    }

    let far = read_far_el1();
    panic!(
        "synchronous EL1 exception: EC={:#x}, ESR_EL1={:#018x}, FAR_EL1={:#018x}, ELR_EL1={:#018x}",
        ec, esr, far, frame.elr_el1
    );
}

#[unsafe(no_mangle)]
pub extern "C" fn handle_irq_exception(_frame: &mut TrapFrame) {
    let iar = crate::gic::acknowledge();
    let irq_id = iar & 0x3FF;

    if irq_id == crate::gic::TIMER_IRQ_ID {
        crate::timer::handle_tick();
    }

    if irq_id < 1020 {
        crate::gic::end_of_interrupt(iar);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn handle_unhandled_exception(frame: &mut TrapFrame, vector: u64) {
    let esr = read_esr_el1();
    let far = read_far_el1();
    panic!(
        "unhandled exception vector {}: ESR_EL1={:#018x}, FAR_EL1={:#018x}, ELR_EL1={:#018x}, SPSR_EL1={:#018x}",
        vector, esr, far, frame.elr_el1, frame.spsr_el1
    );
}
