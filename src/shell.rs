use core::arch::asm;
use core::sync::atomic::Ordering;

use crate::fs;
use crate::{kprint, kprintln};

const LINE_BUF_LEN: usize = 128;

const PSCI_SYSTEM_OFF: u64 = 0x8400_0008;
const PSCI_SYSTEM_RESET: u64 = 0x8400_0009;

unsafe extern "C" {
    static __kernel_virt_start: u8;
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
            kprintln!("  info            Show kernel version, EL, CPU ID, MMU, VMA, and timer freq");
            kprintln!("  cpuinfo         Show per-core timer and scheduler counters");
            kprintln!("  uptime          Show uptime in seconds");
            kprintln!("  mem             Show heap bounds and allocated/free bytes");
            kprintln!("  meminfo         Show 4 KiB physical frame and kernel heap stats");
            kprintln!("  ps              List active kernel threads");
            kprintln!("  blkinfo         Show virtio-blk device slot, MMIO base, and capacity");
            kprintln!("  fsinfo          Show AegisFS sector and file counts");
            kprintln!("  ls              List files");
            kprintln!("  cat <file>      Show file contents");
            kprintln!("  touch <file>    Create an empty file if missing");
            kprintln!("  write <file> <text>  Create or replace a file");
            kprintln!("  append <file> <text> Add text to a file");
            kprintln!("  rm <file>       Remove a file");
            kprintln!("  exec <file> [&] Load and run an EL0 ELF64 binary (& for background)");
            kprintln!("  exec-stress [N] Run N concurrent EL0 hello.elf processes (1..=8)");
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
            let kernel_vma = &raw const __kernel_virt_start as usize;
            kprintln!("Aegis v0.7.0 (aarch64-unknown-none)");
            kprintln!("  CurrentEL  : EL{}", read_current_el());
            kprintln!("  MIDR_EL1   : {:#010x}", read_midr_el1());
            kprintln!("  MMU        : {}", mmu_status);
            kprintln!(
                "  KernelVMA  : 0x{:04X}_{:04X}_{:04X}_{:04X} (TTBR1_EL1)",
                kernel_vma >> 48,
                (kernel_vma >> 32) & 0xffff,
                (kernel_vma >> 16) & 0xffff,
                kernel_vma & 0xffff
            );
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
        "meminfo" => {
            let pmm = crate::pmm::stats();
            let free_kib = pmm.free_frames * (crate::platform::PAGE_SIZE / 1024);
            let free_mib = free_kib / 1024;
            let (allocated, free) = crate::allocator::stats();
            kprintln!("pmm:");
            kprintln!(
                "  managed   : {:#x}..{:#x}",
                pmm.managed_start,
                pmm.managed_end
            );
            kprintln!("  total     : {} frames", pmm.total_frames);
            kprintln!("  used      : {} frames", pmm.used_frames);
            kprintln!(
                "  free      : {} frames ({} KiB / {} MiB)",
                pmm.free_frames,
                free_kib,
                free_mib
            );
            kprintln!("heap:");
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
        "fsinfo" => {
            if arg.is_some() {
                kprintln!("usage: fsinfo");
                return;
            }
            let stats = fs::stats();
            if stats.total_sectors == 0 {
                kprintln!("fsinfo: AegisFS not mounted");
                return;
            }
            kprintln!(
                "AegisFS: {} sectors ({} used, {} free), {}/{} files",
                stats.total_sectors,
                stats.used_sectors,
                stats.free_sectors,
                stats.active_files,
                stats.max_files
            );
        }
        "ls" => {
            if fs::stats().total_sectors == 0 {
                kprintln!("ls: AegisFS not mounted");
                return;
            }
            fs::list(|name, size| kprintln!("FILE {:>8} {}", size, name));
        }
        "touch" | "rm" => {
            let (Some(name), None) = (arg, extra) else {
                kprintln!("usage: {} <file>", cmd);
                return;
            };
            let result = if cmd == "touch" {
                fs::append_file(name, &[])
            } else {
                fs::remove_file(name)
            };
            if let Err(error) = result {
                kprintln!("{}: {}", cmd, error);
            }
        }
        "write" | "append" => {
            let (Some(name), Some(_)) = (arg, extra) else {
                kprintln!("usage: {} <file> <text>", cmd);
                return;
            };
            let args = trimmed[cmd.len()..].trim_start();
            let text = args[name.len()..].trim_start().as_bytes();
            let result = if cmd == "write" {
                fs::write_file(name, text)
            } else {
                fs::append_file(name, text)
            };
            if let Err(error) = result {
                kprintln!("{}: {}", cmd, error);
            }
        }
        "cat" => {
            let path = trimmed[cmd.len()..].trim();
            if path.is_empty() {
                kprintln!("usage: cat <file>");
                return;
            }
            match fs::read_file(path) {
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
            let raw = trimmed[cmd.len()..].trim();
            let (path, background) = match raw.strip_suffix('&') {
                Some(prefix) => (prefix.trim(), true),
                None => (raw, false),
            };
            if path.is_empty() {
                kprintln!("usage: exec <file> [&]");
                return;
            }
            if background {
                match crate::user::spawn_elf(path) {
                    Ok(pid) => kprintln!("[exec] spawned pid={}", pid),
                    Err(error) => kprintln!("exec: {}", error),
                }
            } else {
                match fs::read_file(path) {
                    Ok(bytes) => match crate::user::exec_elf(&bytes) {
                        Ok(code) => kprintln!("[exec] exited with code {}", code),
                        Err(error) => kprintln!("exec: {}", error),
                    },
                    Err(error) => kprintln!("exec: {}", error),
                }
            }
        }
        "exec-stress" => {
            if extra.is_some() {
                kprintln!("usage: exec-stress [N]");
                return;
            }
            let count = match arg {
                Some(raw) => match raw.parse::<usize>() {
                    Ok(n) => n.clamp(1, 8),
                    Err(_) => {
                        kprintln!("usage: exec-stress [N]");
                        return;
                    }
                },
                None => 4,
            };

            let mut pids = [0usize; 8];
            let mut spawned = 0usize;
            let mut err_count = 0usize;
            for _ in 0..count {
                match crate::user::spawn_elf("hello.elf") {
                    Ok(pid) => {
                        pids[spawned] = pid;
                        spawned += 1;
                    }
                    Err(error) => {
                        kprintln!("exec-stress: spawn failed: {}", error);
                        err_count += 1;
                    }
                }
            }

            let mut completed = 0usize;
            let mut ok_count = 0usize;
            for &pid in &pids[..spawned] {
                match crate::user::wait_pid(pid) {
                    Ok(0) => {
                        completed += 1;
                        ok_count += 1;
                    }
                    Ok(code) => {
                        completed += 1;
                        err_count += 1;
                        kprintln!("exec-stress: pid={} exited with code {}", pid, code);
                    }
                    Err(error) => {
                        err_count += 1;
                        kprintln!("exec-stress: wait pid={} failed: {}", pid, error);
                    }
                }
            }

            kprintln!(
                "[exec-stress] completed {}/{} processes (ok={}, err={})",
                completed,
                count,
                ok_count,
                err_count
            );
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
