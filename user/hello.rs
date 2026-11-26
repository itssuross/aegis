#![no_std]
#![no_main]

use core::arch::asm;
use core::panic::PanicInfo;

const SYS_WRITE: u64 = 64;
const SYS_EXIT: u64 = 93;
const SYS_SLEEP_MS: u64 = 200;

fn sys_write(fd: u64, buf: &[u8]) -> isize {
    let ret: isize;
    // SAFETY: Invokes Aegis EL0 `SYS_WRITE` (`x8 = 64`) with a valid read-only slice in user memory.
    unsafe {
        asm!(
            "svc #0",
            in("x8") SYS_WRITE,
            inlateout("x0") fd as usize => ret,
            in("x1") buf.as_ptr(),
            in("x2") buf.len(),
            options(nostack),
        );
    }
    ret
}

fn sys_sleep_ms(ms: u64) {
    // SAFETY: Invokes Aegis EL0 `SYS_SLEEP_MS` (`x8 = 200`), blocking the caller for `ms` milliseconds.
    unsafe {
        asm!(
            "svc #0",
            in("x8") SYS_SLEEP_MS,
            inout("x0") ms => _,
            options(nostack),
        );
    }
}

fn sys_exit(code: i32) -> ! {
    // SAFETY: Invokes Aegis EL0 `SYS_EXIT` (`x8 = 93`), terminating EL0 execution without returning.
    unsafe {
        asm!(
            "svc #0",
            in("x8") SYS_EXIT,
            in("x0") code as u64,
            options(noreturn, nostack),
        );
    }
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    let _ = sys_write(1, b"Hello from EL0 userspace!\n");
    sys_sleep_ms(150);
    let _ = sys_write(1, b"Syscall sleep & exit verified.\n");
    sys_exit(0);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys_exit(1);
}
