use core::arch::asm;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

pub const MAX_CPUS: usize = 4;

const PSCI_CPU_ON: u64 = 0xC400_0003; // PSCI 64-bit CPU_ON function ID
const BOOT_SPIN_LIMIT: usize = 10_000_000;

static ONLINE_CPUS: AtomicUsize = AtomicUsize::new(0);

#[repr(C, align(64))]
pub struct CpuLocal {
    pub cpu_id: usize,
    pub online: AtomicBool,
    pub ticks: AtomicU64,
    pub ctx_switches: AtomicU64,
    pub idle_ticks: AtomicU64,
}

impl CpuLocal {
    const fn new(cpu_id: usize) -> Self {
        Self {
            cpu_id,
            online: AtomicBool::new(false),
            ticks: AtomicU64::new(0),
            ctx_switches: AtomicU64::new(0),
            idle_ticks: AtomicU64::new(0),
        }
    }
}

static CPU_LOCALS: [CpuLocal; MAX_CPUS] = [
    CpuLocal::new(0),
    CpuLocal::new(1),
    CpuLocal::new(2),
    CpuLocal::new(3),
];

unsafe extern "C" {
    fn secondary_entry();
}

pub fn init_cpu_local(cpu_id: usize) {
    if cpu_id == 0 {
        mark_cpu_online(0);
    }
    let address = &CPU_LOCALS[cpu_id] as *const CpuLocal as u64;
    // SAFETY: This EL1 core stores the address of a static CpuLocal in its TPIDR_EL1.
    unsafe {
        asm!(
            "msr tpidr_el1, {}",
            "isb",
            in(reg) address,
            options(nomem, nostack, preserves_flags),
        );
    }
}

pub fn current_cpu() -> &'static CpuLocal {
    let address: u64;
    // SAFETY: TPIDR_EL1 is readable by the current core at EL1.
    unsafe {
        asm!("mrs {}, tpidr_el1", out(reg) address, options(nomem, nostack, preserves_flags));
    }
    CPU_LOCALS
        .iter()
        .find(|&cpu| core::ptr::from_ref(cpu).addr() as u64 == address)
        .unwrap_or(&CPU_LOCALS[0])
}

pub fn cpu_local(cpu_id: usize) -> Option<&'static CpuLocal> {
    CPU_LOCALS.get(cpu_id)
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
    if id < MAX_CPUS && !CPU_LOCALS[id].online.swap(true, Ordering::AcqRel) {
        ONLINE_CPUS.fetch_add(1, Ordering::Release);
    }
}

pub fn record_ctx_switch() {
    current_cpu().ctx_switches.fetch_add(1, Ordering::Relaxed);
}

pub fn record_idle_tick() {
    current_cpu().idle_ticks.fetch_add(1, Ordering::Relaxed);
}

fn psci_cpu_on(target_cpu: u64, entry_point: u64, context_id: u64) -> i64 {
    let ret: i64;
    // SAFETY: PSCI CPU_ON takes the physical trampoline address after Core 0 has
    // initialized the boot page tables and secondary stacks.
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
    let entry = crate::platform::virt_to_phys(secondary_entry as *const () as usize) as u64;

    for cpu in 1..MAX_CPUS {
        let ret = psci_cpu_on(cpu as u64, entry, cpu as u64);
        if ret != 0 {
            crate::kprintln!("[smp] PSCI CPU_ON failed for core {}: {}", cpu, ret);
            continue;
        }

        for _ in 0..BOOT_SPIN_LIMIT {
            if CPU_LOCALS[cpu].online.load(Ordering::Acquire) {
                break;
            }
            core::hint::spin_loop();
        }
    }
}
