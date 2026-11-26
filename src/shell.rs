use core::arch::asm;
use core::sync::atomic::Ordering;

use crate::{kprint, kprintln};

const LINE_BUF_LEN: usize = 128;

const PSCI_SYSTEM_OFF: u64 = 0x8400_0008;
const PSCI_SYSTEM_RESET: u64 = 0x8400_0009;

unsafe extern "C" {
    static __heap_start: u8;
    static __heap_end: u8;
}

#[inline]
fn read_current_el() -> u64 {
    let el: u64;
    // SAFETY: `CurrentEL` is readable at EL1.
    unsafe {
        asm!("mrs {}, CurrentEL", out(reg) el, options(nomem, nostack, preserves_flags));
    }
    (el >> 2) & 0b11
}

#[inline]
fn read_midr_el1() -> u64 {
    let midr: u64;
    // SAFETY: `MIDR_EL1` is readable at EL1.
    unsafe {
        asm!("mrs {}, midr_el1", out(reg) midr, options(nomem, nostack, preserves_flags));
    }
    midr
}

fn psci_call(function_id: u64) -> ! {
    // SAFETY: Callers pass a PSCI system function ID supported by QEMU's HVC conduit.
    unsafe {
        asm!(
            "hvc #0",
            inout("x0") function_id => _,
            lateout("x1") _,
            lateout("x2") _,
            lateout("x3") _,
            options(nomem, nostack),
        );
    }
    loop {
        unsafe {
            asm!("wfe", options(nomem, nostack, preserves_flags));
        }
    }
}

fn execute(line: &str) {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return;
    }

    let mut parts = trimmed.split_whitespace();
    let cmd = parts.next().unwrap_or("");
    let arg = parts.next();
    let extra = parts.next();

    match cmd {
        "help" => {
            kprintln!("Available commands:");
            kprintln!("  help            Show this command list");
            kprintln!("  info            Show kernel version, EL, MIDR_EL1, MMU, and timer freq");
            kprintln!("  cpuinfo         Show per-core timer and scheduler counters");
            kprintln!("  uptime          Show uptime in seconds");
            kprintln!("  mem             Show heap bounds and allocated/free bytes");
            kprintln!("  ps              List active kernel threads");
            kprintln!("  blkinfo         Show virtio-blk device slot, MMIO base, and capacity");
            kprintln!("  ls              List files and directories");
            kprintln!("  cat <file>      Show file contents");
            kprintln!("  exec <file>     Load and run an EL0 ELF64 binary");
            kprintln!("  trace <on|off>  Toggle live timer and scheduler trace logging");
            kprintln!("  brk             Trigger a live BRK #0 synchronous trap");
            kprintln!("  clear           Clear the serial terminal");
            kprintln!("  reboot          Reboot via ARM PSCI SYSTEM_RESET");
            kprintln!("  halt            Power off via ARM PSCI SYSTEM_OFF");
        }
        "info" => {
            let mmu_status = if crate::mmu::is_enabled() {
                "enabled"
            } else {
                "disabled"
            };
            kprintln!("Aegis v0.2.0 (aarch64-unknown-none)");
            kprintln!("  CurrentEL  : EL{}", read_current_el());
            kprintln!("  MIDR_EL1   : {:#010x}", read_midr_el1());
            kprintln!("  MMU        : {}", mmu_status);
            kprintln!("  CNTFRQ_EL0 : {} Hz", crate::timer::frequency_hz());
        }
        "cpuinfo" => {
            use crate::smp::{MAX_CPUS, cpu_local, online_cpus};

            kprintln!("online: {}/{}", online_cpus(), MAX_CPUS);
            kprintln!(
                "{:<3} {:<7} {:>12} {:>12} {:>12}",
                "CPU",
                "STATE",
                "TICKS",
                "IDLE",
                "CTX_SW"
            );
            for id in 0..MAX_CPUS {
                let cpu = cpu_local(id).unwrap();
                let state = if cpu.online.load(Ordering::Acquire) {
                    "ONLINE"
                } else {
                    "OFFLINE"
                };
                let idle = cpu.idle_ticks.load(Ordering::Relaxed);
                kprintln!(
                    "{:<3} {:<7} {:>12} {:>12} {:>12}",
                    id,
                    state,
                    cpu.ticks.load(Ordering::Relaxed),
                    idle,
                    cpu.ctx_switches.load(Ordering::Relaxed)
                );
            }
        }
        "uptime" => {
            let ms = crate::timer::uptime_ms();
            kprintln!("uptime: {}.{:02}s", ms / 1000, (ms % 1000) / 10);
        }
        "mem" => {
            let heap_start = &raw const __heap_start as usize;
            let heap_end = &raw const __heap_end as usize;
            let (allocated, free) = crate::allocator::stats();
            kprintln!(
                "heap: {:#x}..{:#x} ({} bytes)",
                heap_start,
                heap_end,
                heap_end - heap_start
            );
            kprintln!("  allocated : {} bytes", allocated);
            kprintln!("  free      : {} bytes", free);
        }
        "ps" => {
            kprintln!("{:<4} {:<9} {:<3} {}", "TID", "STATE", "CPU", "NAME");
            for info in crate::scheduler::snapshot().into_iter().flatten() {
                let cpu = info.cpu.map_or('-', |id| char::from(b'0' + id as u8));
                kprintln!("{:<4} {:<9} {:<3} {}", info.id, info.state, cpu, info.name);
            }
        }
        "blkinfo" => match crate::blk::info() {
            Some((slot, base, sectors)) => {
                kprintln!("virtio-blk:");
                kprintln!("  slot     : {}", slot);
                kprintln!("  mmio     : {:#010x}", base);
                kprintln!("  sectors  : {} (512 B each)", sectors);
                kprintln!("  capacity : {} KiB", sectors / 2);
            }
            None => {
                kprintln!("blkinfo: no virtio-blk device attached");
            }
        },
        "ls" => {
            if let Err(error) = crate::fs::list_entries(|entry| {
                let kind = if entry.is_dir { "DIR " } else { "FILE" };
                kprintln!("{} {:>8} {}", kind, entry.size, entry.name());
            }) {
                kprintln!("ls: {}", error);
            }
        }
        "cat" => {
            let path = trimmed[cmd.len()..].trim();
            if path.is_empty() {
                kprintln!("usage: cat <file>");
                return;
            }
            match crate::fs::read_file(path) {
                Ok(bytes) => {
                    for chunk in bytes.utf8_chunks() {
                        for ch in chunk.valid().chars() {
                            if !ch.is_control() || matches!(ch, '\n' | '\t') {
                                kprint!("{}", ch);
                            }
                        }
                        if !chunk.invalid().is_empty() {
                            kprint!("\u{FFFD}");
                        }
                    }
                    if !bytes.ends_with(b"\n") {
                        kprintln!();
                    }
                }
                Err(error) => kprintln!("cat: {}", error),
            }
        }
        "exec" => {
            let path = trimmed[cmd.len()..].trim();
            if path.is_empty() {
                kprintln!("usage: exec <file>");
                return;
            }
            match crate::fs::read_file(path) {
                Ok(bytes) => match crate::user::exec_elf(&bytes) {
                    Ok(code) => kprintln!("[exec] exited with code {}", code),
                    Err(error) => kprintln!("exec: {}", error),
                },
                Err(error) => kprintln!("exec: {}", error),
            }
        }
        "trace" => match (arg, extra) {
            (Some("on"), None) => {
                crate::timer::set_trace(true);
                crate::scheduler::set_trace(true);
                kprintln!(
                    "trace: timer={} sched={}",
                    crate::timer::is_trace_enabled(),
                    crate::scheduler::is_trace_enabled()
                );
            }
            (Some("off"), None) => {
                crate::timer::set_trace(false);
                crate::scheduler::set_trace(false);
                kprintln!(
                    "trace: timer={} sched={}",
                    crate::timer::is_trace_enabled(),
                    crate::scheduler::is_trace_enabled()
                );
            }
            (None, None) => {
                kprintln!(
                    "trace: timer={} sched={}",
                    crate::timer::is_trace_enabled(),
                    crate::scheduler::is_trace_enabled()
                );
            }
            _ => kprintln!("usage: trace <on|off>"),
        },
        "brk" => {
            // SAFETY: The EL1 breakpoint handler advances `ELR_EL1` past `brk #0`.
            unsafe {
                core::arch::asm!("brk #0");
            }
        }
        "clear" => {
            kprint!("\x1b[2J\x1b[H");
        }
        "reboot" => {
            kprintln!("Rebooting via PSCI...");
            psci_call(PSCI_SYSTEM_RESET);
        }
        "halt" => {
            kprintln!("Halting via PSCI...");
            psci_call(PSCI_SYSTEM_OFF);
        }
        _ => {
            kprintln!("unknown command: '{}' (type 'help')", cmd);
        }
    }
}

pub extern "C" fn shell_main() -> ! {
    while crate::DEMO_WORKERS_DONE.load(core::sync::atomic::Ordering::Acquire) < 2 {
        crate::scheduler::sleep_ms(20);
    }

    let mut buf = [0u8; LINE_BUF_LEN];
    let mut len = 0usize;
    let mut last_cr = false;

    kprint!("aegis> ");

    loop {
        let Some(byte) = crate::uart::try_read_byte() else {
            crate::uart::wait_for_input();
            continue;
        };

        if byte == b'\n' && last_cr {
            last_cr = false;
            continue;
        }

        match byte {
            b'\r' | b'\n' => {
                last_cr = byte == b'\r';
                kprintln!();
                if let Ok(line) = core::str::from_utf8(&buf[..len]) {
                    execute(line);
                }
                len = 0;
                kprint!("aegis> ");
            }
            0x08 | 0x7F => {
                last_cr = false;
                if len > 0 {
                    len -= 1;
                    kprint!("\x08 \x08");
                }
            }
            0x20..=0x7E => {
                last_cr = false;
                if len < buf.len() {
                    buf[len] = byte;
                    len += 1;
                    crate::uart::write_byte(byte);
                }
            }
            _ => {
                last_cr = false;
            }
        }
    }
}
