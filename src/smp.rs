use core::arch::asm;
use core::sync::atomic::{AtomicUsize, Ordering};

pub const MAX_CPUS: usize = 4;

const PSCI_CPU_ON: u64 = 0xC400_0003; // PSCI 64-bit CPU_ON function ID
const BOOT_SPIN_LIMIT: usize = 10_000_000;

static ONLINE_CPUS: AtomicUsize = AtomicUsize::new(1);

unsafe extern "C" {
    fn secondary_entry();
}

pub fn cpu_id() -> usize {
    let mpidr: u64;
    // SAFETY: `MPIDR_EL1` is readable at EL1 with no side effects.
    unsafe {
        asm!("mrs {}, mpidr_el1", out(reg) mpidr, options(nomem, nostack, preserves_flags));
    }
    (mpidr & 0xff) as usize // Aff0 core ID
}

pub fn online_cpus() -> usize {
    ONLINE_CPUS.load(Ordering::Acquire)
}

pub fn mark_cpu_online(id: usize) {
    if id > 0 && id < MAX_CPUS {
        ONLINE_CPUS.fetch_add(1, Ordering::Release);
    }
}

fn psci_cpu_on(target_cpu: u64, entry_point: u64, context_id: u64) -> i64 {
    let ret: i64;
    // SAFETY: Invokes PSCI CPU_ON via QEMU's EL1 HVC conduit with `secondary_entry` as the entry
    // point after Core 0 has initialized page tables and `secondary_stacks`.
    unsafe {
        asm!(
            "hvc #0",
            inout("x0") PSCI_CPU_ON => ret,
            inout("x1") target_cpu => _,
            inout("x2") entry_point => _,
            inout("x3") context_id => _,
            options(nostack),
        );
    }
    ret
}

pub fn boot_secondary_cores() {
    let entry = secondary_entry as *const () as usize as u64;

    for cpu in 1..MAX_CPUS {
        let before = ONLINE_CPUS.load(Ordering::Acquire);
        let ret = psci_cpu_on(cpu as u64, entry, cpu as u64);
        if ret != 0 {
            crate::kprintln!("[smp] PSCI CPU_ON failed for core {}: {}", cpu, ret);
            continue;
        }

        for _ in 0..BOOT_SPIN_LIMIT {
            if ONLINE_CPUS.load(Ordering::Acquire) > before {
                break;
            }
            core::hint::spin_loop();
        }
    }
}
