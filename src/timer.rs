use core::arch::asm;
use core::sync::atomic::{AtomicU64, Ordering};

static TICK_INTERVAL: AtomicU64 = AtomicU64::new(0);
static TICKS: AtomicU64 = AtomicU64::new(0);

#[inline]
fn read_cntfrq() -> u64 {
    let freq: u64;
    // SAFETY: Reading `CNTFRQ_EL0` is valid at EL1 on AArch64 with no side effects.
    unsafe {
        asm!("mrs {}, cntfrq_el0", out(reg) freq, options(nomem, nostack, preserves_flags));
    }
    freq
}

#[inline]
fn write_cntp_tval(ticks: u64) {
    // SAFETY: Writing `CNTP_TVAL_EL0` programs the EL1 physical timer countdown value.
    unsafe {
        asm!(
            "msr cntp_tval_el0, {}",
            "isb",
            in(reg) ticks,
            options(nomem, nostack, preserves_flags),
        );
    }
}

#[inline]
fn write_cntp_ctl(val: u64) {
    // SAFETY: Writing `CNTP_CTL_EL0` configures the EL1 physical timer enable and mask bits.
    unsafe {
        asm!(
            "msr cntp_ctl_el0, {}",
            "isb",
            in(reg) val,
            options(nomem, nostack, preserves_flags),
        );
    }
}

pub fn init() {
    let freq = read_cntfrq();
    TICK_INTERVAL.store(freq, Ordering::Relaxed);
    write_cntp_tval(freq);
    // Enable physical timer (ENABLE = 1, IMASK = 0).
    write_cntp_ctl(1);
}

pub fn handle_tick() {
    let interval = TICK_INTERVAL.load(Ordering::Relaxed);
    write_cntp_tval(interval);
    let tick = TICKS.fetch_add(1, Ordering::Relaxed) + 1;
    crate::kprintln!("[timer] tick #{}", tick);
}
