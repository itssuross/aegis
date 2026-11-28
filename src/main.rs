#![no_std]
#![no_main]

extern crate alloc;

use core::arch::{asm, global_asm};
use core::panic::PanicInfo;

mod allocator;
pub mod blk;
mod exceptions;
pub mod fd;
pub mod fs;
mod gic;
mod mmu;
pub mod pipe;
pub mod platform;
pub mod pmm;
pub mod sync;
mod scheduler;
mod shell;
pub mod smp;
mod timer;
#[allow(dead_code, reason = "shared UART readers use the wait queue in uart")]
#[path = "uart.rs"]
mod uart_driver;
pub mod user;
pub mod virtio;

mod uart {
    use crate::scheduler::WaitQueue;
    use crate::sync::IrqSpinLock;
    use crate::uart_driver as driver;

    pub use driver::{_print, init, write_byte};

    static RX_LOCK: IrqSpinLock<()> = IrqSpinLock::new(());
    static RX_WAITQ: WaitQueue = WaitQueue::new();

    pub fn try_read_byte() -> Option<u8> {
        let _guard = RX_LOCK.lock();
        driver::try_read_byte()
    }

    pub fn wait_for_input() {
        RX_WAITQ.wait_until(driver::has_pending_input);
    }

    pub fn handle_irq() {
        driver::handle_irq();
        if driver::has_pending_input() {
            RX_WAITQ.wake_all();
        }
    }
}

global_asm!(include_str!("boot.S"));
core::arch::global_asm!(include_str!("exceptions.S"));
core::arch::global_asm!(include_str!("context.S"));

unsafe extern "C" {
    static __kernel_virt_start: u8;
    static __heap_start: u8;
    static __heap_end: u8;
}

pub static DEMO_WORKERS_DONE: core::sync::atomic::AtomicU8 = core::sync::atomic::AtomicU8::new(0);

fn current_el() -> u64 {
    let el: u64;
    // SAFETY: `CurrentEL` is readable at EL1.
    unsafe {
        asm!("mrs {}, CurrentEL", out(reg) el, options(nomem, nostack, preserves_flags));
    }
    (el >> 2) & 0b11
}

fn verify_page_table_lifecycle() {
    const TEST_RX_VA: usize = 0x0040_0000;
    const TEST_RW_VA: usize = 0x0040_1000;

    let before = pmm::stats().used_frames;
    {
        let mut pt = mmu::PageTable::new().expect("failed to allocate L1 page table");
        assert_eq!(pt.root_phys() & (platform::PAGE_SIZE - 1), 0);

        let rx_phys = pmm::alloc_frame().expect("failed to allocate UserRx frame");
        let rw_phys = pmm::alloc_frame().expect("failed to allocate UserRw frame");

        pt.map_4k(TEST_RX_VA, rx_phys, mmu::PageFlags::UserRx)
            .expect("failed to map UserRx page");
        pt.map_4k(TEST_RW_VA, rw_phys, mmu::PageFlags::UserRw)
            .expect("failed to map UserRw page");

        let (tr_rx, pte_rx) = pt.translate(TEST_RX_VA).expect("missing UserRx mapping");
        assert_eq!(tr_rx, rx_phys);
        assert_eq!(pte_rx & mmu::UXN, 0, "UserRx page must clear UXN");

        let (tr_rw, pte_rw) = pt.translate(TEST_RW_VA).expect("missing UserRw mapping");
        assert_eq!(tr_rw, rw_phys);
        assert_ne!(pte_rw & mmu::UXN, 0, "UserRw page must set UXN");

        assert_eq!(pt.unmap_4k(TEST_RX_VA), Some(rx_phys));
        assert_eq!(pt.unmap_4k(TEST_RW_VA), Some(rw_phys));
        assert!(pt.translate(TEST_RX_VA).is_none());
        assert!(pt.translate(TEST_RW_VA).is_none());

        pmm::free_frame(rx_phys);
        pmm::free_frame(rw_phys);
    }
    assert_eq!(
        pmm::stats().used_frames,
        before,
        "page table lifecycle leaked physical frames"
    );
    kprintln!("[mmu] 3-level 4 KiB page table self-test passed (0 frames leaked)");
}

#[unsafe(no_mangle)]
pub extern "C" fn kmain() -> ! {
    uart::init();

    let el = current_el();
    assert_eq!(el, 1, "expected kernel to run at EL1");
    smp::init_cpu_local(0);

    kprintln!("Aegis Kernel v0.7.0 booted on AArch64 (EL{})", el);

    exceptions::init();
    mmu::init();

    let heap_start = &raw const __heap_start as usize;
    let heap_end = &raw const __heap_end as usize;
    // SAFETY: The linker reserves this page-aligned region exclusively for the heap.
    // No allocations have occurred yet.
    unsafe {
        allocator::init(heap_start, heap_end - heap_start);
    }

    pmm::init();
    let pmm_stats = pmm::stats();
    kprintln!(
        "[pmm] managed {:#x}..{:#x} ({} frames, {} MiB free)",
        pmm_stats.managed_start,
        pmm_stats.managed_end,
        pmm_stats.total_frames,
        (pmm_stats.free_frames * platform::PAGE_SIZE) / (1024 * 1024)
    );
    verify_page_table_lifecycle();

    gic::init();
    scheduler::init();
    smp::boot_secondary_cores();
    let online_cpus = smp::online_cpus();
    if online_cpus == smp::MAX_CPUS {
        // SAFETY: Core 0's PC, stack, and vectors are mapped by TTBR1, and all secondaries are online.
        unsafe {
            mmu::drop_boot_identity_map();
        }
    }
    kprintln!("[smp] active cores: {}/{}", online_cpus, smp::MAX_CPUS);

    // SAFETY: The EL1 breakpoint handler advances `ELR_EL1` past `brk #0`.
    unsafe {
        core::arch::asm!("brk #0");
    }

    let kernel_start = &raw const __kernel_virt_start as usize;
    kprintln!(
        "[mmu] enabled={} (4KB granule, 39-bit VA, TTBR1 kernel at {:#x}, I/D caches on)",
        mmu::is_enabled(),
        kernel_start
    );

    let (base_allocated, base_free) = allocator::stats();
    kprintln!(
        "[heap] region {:#x}..{:#x}: allocated={} free={}",
        heap_start,
        heap_end,
        base_allocated,
        base_free
    );

    {
        let boxed = alloc::boxed::Box::new(0xDEAD_BEEF_u64);
        let mut squares: alloc::vec::Vec<u64> = alloc::vec::Vec::new();
        for i in 0..64u64 {
            squares.push(i * i);
        }
        let sum: u64 = squares.iter().sum();
        assert_eq!(*boxed, 0xDEAD_BEEF);
        assert_eq!(sum, 85_344);

        let (allocated, free) = allocator::stats();
        kprintln!(
            "[heap] Box={:#x} @ {:p}, Vec len={} sum={}: allocated={} free={}",
            *boxed,
            &*boxed,
            squares.len(),
            sum,
            allocated,
            free
        );
    }

    let (allocated, free) = allocator::stats();
    assert_eq!(
        (allocated, free),
        (base_allocated, base_free),
        "heap did not return to its initial state after drop"
    );
    kprintln!(
        "[heap] after drop: allocated={} free={} (fully coalesced)",
        allocated,
        free
    );

    if blk::init() {
        if let Some((slot, base, sectors)) = blk::info() {
            kprintln!(
                "[blk] virtio-blk slot={} mmio={:#x} sectors={} ({} KiB)",
                slot,
                base,
                sectors,
                sectors / 2
            );
        }
        match fs::init() {
            Ok(()) => kprintln!("[fs] mounted AegisFS"),
            Err(error) => kprintln!("[fs] mount failed: {}", error),
        }
    } else {
        kprintln!("[blk] no virtio-blk device found");
    }

    scheduler::spawn("worker-alpha", worker_alpha);
    scheduler::spawn("worker-beta", worker_beta);
    scheduler::spawn("shell", shell::shell_main);

    timer::init();
    exceptions::enable_irqs();
    kprintln!("Aegis interrupts & {}Hz timer active", timer::TICK_HZ);
    idle();
}

#[unsafe(no_mangle)]
pub extern "C" fn secondary_main(cpu_id: usize) -> ! {
    // SAFETY: The trampoline installed this core's higher-half stack and vectors before branching here.
    unsafe {
        mmu::init_secondary();
    }

    assert_eq!(smp::cpu_id(), cpu_id, "secondary MPIDR mismatch");
    smp::init_cpu_local(cpu_id);
    scheduler::init_secondary(cpu_id);
    gic::init_cpu();
    timer::init_cpu();
    kprintln!("[smp] core {} online (EL1, MMU enabled)", cpu_id);
    smp::mark_cpu_online(cpu_id);

    // SAFETY: This core has initialized its vectors, banked GIC interface, and timer.
    unsafe {
        core::arch::asm!("msr daifclr, #2", options(nomem, nostack));
    }

    loop {
        // SAFETY: IRQs are enabled and can wake this core from WFI.
        unsafe {
            core::arch::asm!("wfi", options(nomem, nostack));
        }
    }
}

extern "C" fn worker_alpha() -> ! {
    for step in 1..=3 {
        kprintln!("[thread:alpha] step {}", step);
        if step < 3 {
            scheduler::sleep_ms(250);
        }
    }
    DEMO_WORKERS_DONE.fetch_add(1, core::sync::atomic::Ordering::Release);
    scheduler::scheduler_thread_exit();
}

extern "C" fn worker_beta() -> ! {
    for step in 1..=3 {
        kprintln!("[thread:beta] step {}", step);
        if step < 3 {
            scheduler::sleep_ms(400);
        }
    }
    DEMO_WORKERS_DONE.fetch_add(1, core::sync::atomic::Ordering::Release);
    scheduler::scheduler_thread_exit();
}

fn idle() -> ! {
    loop {
        // SAFETY: IRQs are enabled and can wake the idle core from WFI.
        unsafe {
            asm!("wfi", options(nomem, nostack, preserves_flags));
        }
        scheduler::yield_now();
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    if let Some(location) = info.location() {
        kprintln!(
            "KERNEL PANIC at {}:{}: {}",
            location.file(),
            location.line(),
            info.message()
        );
    } else {
        kprintln!("KERNEL PANIC: {}", info.message());
    }

    loop {
        unsafe {
            asm!("wfe", options(nomem, nostack, preserves_flags));
        }
    }
}
