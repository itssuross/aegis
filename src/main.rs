#![no_std]
#![no_main]

extern crate alloc;

use core::arch::{asm, global_asm};
use core::panic::PanicInfo;

mod allocator;
mod exceptions;
mod gic;
mod mmu;
mod timer;
mod uart;

global_asm!(include_str!("boot.S"));
core::arch::global_asm!(include_str!("exceptions.S"));

unsafe extern "C" {
    static __heap_start: u8;
    static __heap_end: u8;
}

fn current_el() -> u64 {
    let el: u64;
    // SAFETY: Reading `CurrentEL` is valid at EL1 and above on AArch64 with no side effects.
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
    // SAFETY: `__heap_start..__heap_end` is a linker-reserved, page-aligned region past the end
    // of the kernel image that nothing else uses, and no allocation has happened yet.
    unsafe {
        allocator::init(heap_start, heap_end - heap_start);
    }

    gic::init();
    timer::init();
    exceptions::enable_irqs();

    // SAFETY: `brk #0` triggers a synchronous EL1 debug trap handled by `handle_sync_exception`,
    // which logs the trap and advances `ELR_EL1` past the 4-byte instruction.
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

    kprintln!("Aegis interrupts & 1Hz timer active");

    loop {
        // SAFETY: `wfe` places the core in a low-power wait state until an event occurs.
        unsafe {
            asm!("wfe", options(nomem, nostack, preserves_flags));
        }
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
        // SAFETY: `wfe` halts the core in a low-power wait loop after a fatal panic.
        unsafe {
            asm!("wfe", options(nomem, nostack, preserves_flags));
        }
    }
}
