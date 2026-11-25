#![no_std]
#![no_main]

use core::arch::{asm, global_asm};
use core::panic::PanicInfo;

mod uart;

global_asm!(include_str!("boot.S"));

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
