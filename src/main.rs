#![no_std]
#![no_main]

extern crate alloc;

use core::arch::{asm, global_asm};
use core::panic::PanicInfo;

mod allocator;
pub mod blk;
mod exceptions;
pub mod fs;
mod gic;
mod mmu;
pub mod sync;
mod scheduler;
mod shell;
mod timer;
mod uart;
pub mod user;
pub mod virtio;

global_asm!(include_str!("boot.S"));
core::arch::global_asm!(include_str!("exceptions.S"));
core::arch::global_asm!(include_str!("context.S"));

unsafe extern "C" {
    static __heap_start: u8;
    static __heap_end: u8;
}

fn current_el() -> u64 {
    let el: u64;
    // SAFETY: `CurrentEL` is readable at EL1.
    unsafe {
        asm!("mrs {}, CurrentEL", out(reg) el, options(nomem, nostack, preserves_flags));
    }
    (el >> 2) & 0b11
}

#[unsafe(no_mangle)]
pub extern "C" fn kmain() -> ! {
    uart::init();

    let el = current_el();
    assert_eq!(el, 1, "expected kernel to run at EL1");

    kprintln!("Aegis Kernel v0.1.0 booted on AArch64 (EL{})", el);

    exceptions::init();
    mmu::init();

    let heap_start = &raw const __heap_start as usize;
    let heap_end = &raw const __heap_end as usize;
    // SAFETY: The linker reserves this page-aligned region exclusively for the heap.
    // No allocations have occurred yet.
    unsafe {
        allocator::init(heap_start, heap_end - heap_start);
    }

    // SAFETY: The EL1 breakpoint handler advances `ELR_EL1` past `brk #0`.
    unsafe {
        core::arch::asm!("brk #0");
    }

    let mmu_enabled = mmu::is_enabled();
    assert!(mmu_enabled, "MMU failed to enable");
    kprintln!(
        "[mmu] enabled={} (4KB granule, 39-bit VA, identity map, I/D caches on)",
        mmu_enabled
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
    } else {
        kprintln!("[blk] no virtio-blk device found");
    }

    scheduler::init();
    scheduler::spawn("worker-alpha", worker_alpha);
    scheduler::spawn("worker-beta", worker_beta);
    scheduler::spawn("shell", shell::shell_main);

    gic::init();
    timer::init();
    exceptions::enable_irqs();
    kprintln!("Aegis interrupts & {}Hz timer active", timer::TICK_HZ);
    idle();
}

extern "C" fn worker_alpha() -> ! {
    for step in 1..=3 {
        kprintln!("[thread:alpha] step {}", step);
        if step < 3 {
            scheduler::sleep_ms(250);
        }
    }
    scheduler::scheduler_thread_exit();
}

extern "C" fn worker_beta() -> ! {
    for step in 1..=3 {
        kprintln!("[thread:beta] step {}", step);
        if step < 3 {
            scheduler::sleep_ms(400);
        }
    }
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
