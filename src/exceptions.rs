use core::arch::{asm, naked_asm};

const EC_SVC_AARCH64: u64 = 0x15;
const EC_BRK_AARCH64: u64 = 0x3C;

const SYS_WRITE: u64 = 64;
const SYS_EXIT: u64 = 93;
const SYS_SCHED_YIELD: u64 = 124;
const SYS_SLEEP_MS: u64 = 200;

const EBADF: i64 = -9;
const EFAULT: i64 = -14;
const ENOSYS: i64 = -38;

#[repr(C)]
#[allow(dead_code)]
pub struct TrapFrame {
    pub regs: [u64; 31],
    pub sp_el0: u64,
    pub elr_el1: u64,
    pub spsr_el1: u64,
}

#[repr(C)]
#[allow(dead_code)]
pub struct El0TrapFrame {
    pub regs: [u64; 31],
    pub elr_el1: u64,
    pub spsr_el1: u64,
    pub sp_el0: u64,
}

const _: () = assert!(core::mem::size_of::<El0TrapFrame>() == 272);
const _: () = assert!(core::mem::offset_of!(El0TrapFrame, elr_el1) == 248);
const _: () = assert!(core::mem::offset_of!(El0TrapFrame, spsr_el1) == 256);

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

fn user_ptr_readable(ptr: usize, len: usize) -> bool {
    const USER_VA_LIMIT: usize = crate::platform::GICD_BASE;
    let Some(end) = ptr.checked_add(len) else {
        return false;
    };
    if ptr >= USER_VA_LIMIT || end > USER_VA_LIMIT {
        return false;
    }
    if len == 0 {
        return true;
    }

    let page_mask = !(crate::platform::PAGE_SIZE - 1);
    let mut va = ptr & page_mask;
    let last_page = (end - 1) & page_mask;

    let daif = crate::sync::irq_save_disable();
    let mut ok = true;
    loop {
        let par: u64;
        // SAFETY: `at s1e0r` queries EL0 stage-1 read translation under the active `TTBR0_EL1`;
        // IRQs are masked so `PAR_EL1` is not clobbered before being read.
        unsafe {
            asm!(
                "at s1e0r, {va}",
                "isb",
                "mrs {par}, par_el1",
                va = in(reg) va,
                par = out(reg) par,
                options(nomem, nostack, preserves_flags),
            );
        }
        if (par & 1) != 0 {
            ok = false;
            break;
        }
        if va == last_page {
            break;
        }
        va += crate::platform::PAGE_SIZE;
    }
    crate::sync::irq_restore(daif);
    ok
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
pub extern "C" fn handle_el0_sync(frame: &mut El0TrapFrame) {
    let esr = read_esr_el1();
    let ec = (esr >> 26) & 0x3F;

    if ec == EC_SVC_AARCH64 {
        match frame.regs[8] {
            SYS_WRITE => {
                let fd = frame.regs[0];
                let ptr = frame.regs[1] as usize;
                let len = frame.regs[2] as usize;
                if fd != 1 && fd != 2 {
                    frame.regs[0] = EBADF as u64;
                    return;
                }
                if !user_ptr_readable(ptr, len) {
                    frame.regs[0] = EFAULT as u64;
                    return;
                }
                if len == 0 {
                    frame.regs[0] = 0;
                    return;
                }
                // SAFETY: `user_ptr_readable` verified every 4 KiB page in `ptr..ptr + len` is
                // mapped EL0-readable under the active `TTBR0_EL1`, and `len > 0` implies `ptr != 0`.
                let bytes = unsafe { core::slice::from_raw_parts(ptr as *const u8, len) };
                for &byte in bytes {
                    crate::uart::write_byte(byte);
                }
                frame.regs[0] = len as u64;
            }
            SYS_EXIT => {
                crate::user::exit_to_el1(frame.regs[0] as i32);
            }
            SYS_SCHED_YIELD => {
                crate::scheduler::yield_now();
                frame.regs[0] = 0;
            }
            SYS_SLEEP_MS => {
                crate::scheduler::sleep_ms(frame.regs[0]);
                frame.regs[0] = 0;
            }
            _ => {
                frame.regs[0] = ENOSYS as u64;
            }
        }
        return;
    }

    let far = read_far_el1();
    crate::kprintln!(
        "[user] EL0 fault EC={:#x} ESR={:#x} ELR={:#x} FAR={:#x}",
        ec,
        esr,
        frame.elr_el1,
        far
    );
    crate::user::exit_to_el1(-1);
}

#[unsafe(export_name = "handle_irq_exception")]
#[unsafe(naked)]
pub extern "C" fn irq_entry(_frame: &mut TrapFrame) {
    naked_asm!(
        "sub sp, sp, #544",
        "stp q0, q1, [sp, #0]",
        "stp q2, q3, [sp, #32]",
        "stp q4, q5, [sp, #64]",
        "stp q6, q7, [sp, #96]",
        "stp q8, q9, [sp, #128]",
        "stp q10, q11, [sp, #160]",
        "stp q12, q13, [sp, #192]",
        "stp q14, q15, [sp, #224]",
        "stp q16, q17, [sp, #256]",
        "stp q18, q19, [sp, #288]",
        "stp q20, q21, [sp, #320]",
        "stp q22, q23, [sp, #352]",
        "stp q24, q25, [sp, #384]",
        "stp q26, q27, [sp, #416]",
        "stp q28, q29, [sp, #448]",
        "stp q30, q31, [sp, #480]",
        "mrs x9, fpcr",
        "mrs x10, fpsr",
        "str x9, [sp, #512]",
        "str x10, [sp, #520]",
        "str x30, [sp, #528]",
        "bl {dispatch}",
        "ldr x30, [sp, #528]",
        "ldr x9, [sp, #512]",
        "ldr x10, [sp, #520]",
        "msr fpcr, x9",
        "msr fpsr, x10",
        "ldp q30, q31, [sp, #480]",
        "ldp q28, q29, [sp, #448]",
        "ldp q26, q27, [sp, #416]",
        "ldp q24, q25, [sp, #384]",
        "ldp q22, q23, [sp, #352]",
        "ldp q20, q21, [sp, #320]",
        "ldp q18, q19, [sp, #288]",
        "ldp q16, q17, [sp, #256]",
        "ldp q14, q15, [sp, #224]",
        "ldp q12, q13, [sp, #192]",
        "ldp q10, q11, [sp, #160]",
        "ldp q8, q9, [sp, #128]",
        "ldp q6, q7, [sp, #96]",
        "ldp q4, q5, [sp, #64]",
        "ldp q2, q3, [sp, #32]",
        "ldp q0, q1, [sp, #0]",
        "add sp, sp, #544",
        "ret",
        dispatch = sym handle_irq_exception,
    );
}

extern "C" fn handle_irq_exception(_frame: &mut TrapFrame) {
    let iar = crate::gic::acknowledge();
    let irq_id = iar & 0x3FF;

    match irq_id {
        crate::gic::TIMER_IRQ_ID => crate::timer::handle_irq(iar),
        crate::gic::UART0_IRQ_ID => {
            crate::uart::handle_irq();
            crate::gic::end_of_interrupt(iar);
        }
        0..=1019 => crate::gic::end_of_interrupt(iar),
        _ => {}
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
