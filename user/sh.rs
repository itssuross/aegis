#![no_std]
#![no_main]

use core::arch::asm;
use core::fmt::{self, Write};
use core::panic::PanicInfo;

const SYS_CLOSE: u64 = 57;
const SYS_READ: u64 = 63;
const SYS_WRITE: u64 = 64;
const SYS_EXIT: u64 = 93;
const SYS_OPEN: u64 = 201;
const SYS_PIPE: u64 = 202;
const SYS_SPAWN: u64 = 203;
const SYS_WAITPID: u64 = 204;
const SYS_UPTIME_MS: u64 = 205;
const SYS_UNLINK: u64 = 206;
const SYS_FSINFO: u64 = 207;

const O_RDONLY: usize = 0;
const O_WRONLY: usize = 1;
const O_CREAT: usize = 0x40;
const O_TRUNC: usize = 0x200;
const O_APPEND: usize = 0x400;

const STDIN: usize = 0;
const STDOUT: usize = 1;
const LINE_BUF_LEN: usize = 128;

// SAFETY: Callers must supply arguments and valid buffers for the selected syscall.
unsafe fn syscall(number: u64, arg0: usize, arg1: usize, arg2: usize) -> i64 {
    let result: i64;
    // SAFETY: Arguments follow the syscall ABI, which preserves registers other than x0.
    unsafe {
        asm!(
            "svc #0",
            in("x8") number,
            inlateout("x0") arg0 => result,
            in("x1") arg1,
            in("x2") arg2,
            options(nostack),
        );
    }
    result
}

fn sys_read(fd: usize, buf: &mut [u8]) -> i64 {
    // SAFETY: The syscall can write up to buf.len() bytes into this writable slice.
    unsafe { syscall(SYS_READ, fd, buf.as_mut_ptr() as usize, buf.len()) }
}

fn sys_write(fd: usize, buf: &[u8]) -> i64 {
    // SAFETY: This slice is readable for the duration of the syscall.
    unsafe { syscall(SYS_WRITE, fd, buf.as_ptr() as usize, buf.len()) }
}

fn sys_open(path: &str, flags: usize) -> i64 {
    // SAFETY: The path points to readable UTF-8 bytes for the duration of the syscall.
    unsafe { syscall(SYS_OPEN, path.as_ptr() as usize, path.len(), flags) }
}

fn sys_unlink(path: &str) -> i64 {
    // SAFETY: The path points to readable UTF-8 bytes for the duration of the syscall.
    unsafe { syscall(SYS_UNLINK, path.as_ptr() as usize, path.len(), 0) }
}

fn sys_fsinfo(stats: &mut [u32; 5]) -> i64 {
    // SAFETY: The syscall writes five u32 counters into this writable array.
    unsafe { syscall(SYS_FSINFO, stats.as_mut_ptr() as usize, 0, 0) }
}

fn sys_close(fd: usize) -> i64 {
    // SAFETY: The kernel checks the descriptor number and takes no pointer arguments.
    unsafe { syscall(SYS_CLOSE, fd, 0, 0) }
}

fn sys_pipe(pair: &mut [u32; 2]) -> i64 {
    // SAFETY: The syscall writes two u32 descriptors into this writable array.
    unsafe { syscall(SYS_PIPE, pair.as_mut_ptr() as usize, 0, 0) }
}

fn sys_spawn(path: &str) -> i64 {
    // SAFETY: The path points to readable UTF-8 bytes for the duration of the syscall.
    unsafe { syscall(SYS_SPAWN, path.as_ptr() as usize, path.len(), 0) }
}

fn sys_waitpid(pid: usize) -> i64 {
    // SAFETY: The kernel checks the PID and takes no pointer arguments.
    unsafe { syscall(SYS_WAITPID, pid, 0, 0) }
}

fn sys_uptime_ms() -> u64 {
    // SAFETY: This syscall takes no arguments.
    unsafe { syscall(SYS_UPTIME_MS, 0, 0, 0) as u64 }
}

fn sys_exit(code: i32) -> ! {
    // SAFETY: SYS_EXIT accepts the exit status in x0 and never returns.
    unsafe {
        asm!(
            "svc #0",
            in("x8") SYS_EXIT,
            in("x0") code as u64,
            options(noreturn, nostack),
        );
    }
}

fn write_all(fd: usize, mut buf: &[u8]) -> Result<(), fmt::Error> {
    while !buf.is_empty() {
        let written = sys_write(fd, buf);
        if written <= 0 || written as usize > buf.len() {
            return Err(fmt::Error);
        }
        buf = &buf[written as usize..];
    }
    Ok(())
}

struct Stdout;

impl Write for Stdout {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        write_all(STDOUT, text.as_bytes())
    }
}

fn print(text: &str) {
    let _ = Stdout.write_str(text);
}

fn cat(path: &str) {
    if path.is_empty() {
        print("usage: cat <file>\n");
        return;
    }

    let fd = sys_open(path, O_RDONLY);
    if fd < 0 {
        let _ = writeln!(Stdout, "cat: cannot open {path} ({fd})");
        return;
    }
    let fd = fd as usize;
    let mut buf = [0; 256];
    let mut last = None;
    loop {
        let count = sys_read(fd, &mut buf);
        if count == 0 {
            break;
        }
        if count < 0 || write_all(STDOUT, &buf[..count as usize]).is_err() {
            print("cat: I/O failed\n");
            break;
        }
        last = Some(buf[count as usize - 1]);
    }
    let _ = sys_close(fd);
    if last.is_some_and(|byte| byte != b'\n') {
        print("\n");
    }
}

fn fsinfo() {
    let mut stats = [0; 5];
    let result = sys_fsinfo(&mut stats);
    let [total, used, free, active, max] = stats;
    if result < 0 || total == 0 {
        print("fsinfo: AegisFS not mounted\n");
        return;
    }
    let _ = writeln!(
        Stdout,
        "AegisFS: {total} sectors ({used} used, {free} free), {active}/{max} files"
    );
}

fn write_file(command: &str, path: &str, flags: usize, text: &str) {
    let fd = sys_open(path, O_WRONLY | O_CREAT | flags);
    if fd < 0 {
        let _ = writeln!(Stdout, "{command}: cannot open {path} ({fd})");
        return;
    }
    let fd = fd as usize;
    let wrote = write_all(fd, text.as_bytes());
    let closed = sys_close(fd);
    if wrote.is_err() || closed < 0 {
        let _ = writeln!(Stdout, "{command}: I/O failed");
    }
}

fn run(path: &str) {
    if path.is_empty() {
        print("usage: run <file>\n");
        return;
    }

    let pid = sys_spawn(path);
    if pid < 0 {
        let _ = writeln!(Stdout, "run: cannot spawn {path} ({pid})");
        return;
    }
    let code = sys_waitpid(pid as usize);
    let _ = writeln!(Stdout, "[sh] exited with code {code}");
}

fn pipe_matches(read_fd: usize, mut expected: &[u8]) -> bool {
    let mut buf = [0; 8];
    let mut matches = true;
    loop {
        let count = sys_read(read_fd, &mut buf);
        if count <= 0 {
            return count == 0 && matches && expected.is_empty();
        }
        let bytes = &buf[..count as usize];
        if let Some(rest) = expected.strip_prefix(bytes) {
            expected = rest;
        } else {
            matches = false;
        }
    }
}

fn pipetest() {
    let mut pair = [0; 2];
    if sys_pipe(&mut pair) != 0 {
        print("[pipetest] could not create pipe\n");
        return;
    }

    let [read_fd, write_fd] = pair.map(|fd| fd as usize);
    let message = b"ping-over-pipe";
    let wrote = write_all(write_fd, message).is_ok();
    let closed = sys_close(write_fd) == 0;
    let matches = closed && pipe_matches(read_fd, message);
    let read_closed = sys_close(read_fd) == 0;

    if wrote && matches && read_closed {
        print("[pipetest] ok\n");
    } else {
        print("[pipetest] failed\n");
    }
}

fn execute(line: &str) {
    let line = line.trim_start();
    let end = line.find(char::is_whitespace).unwrap_or(line.len());
    let (command, rest) = line.split_at(end);
    let arg = rest.trim_start();

    match command {
        "" => {}
        "help" => print(concat!(
            "help\necho <text>\nfsinfo\ncat <file>\ntouch <file>\n",
            "write <file> <text>\nappend <file> <text>\nrm <file>\n",
            "uptime\nrun <file>\npipetest\nexit [code]\n",
        )),
        "echo" => {
            print(arg);
            print("\n");
        }
        "fsinfo" if arg.is_empty() => fsinfo(),
        "fsinfo" => print("usage: fsinfo\n"),
        "cat" => cat(arg.trim_end()),
        "touch" | "rm" => {
            let path = arg.trim_end();
            if path.is_empty() || path.contains(char::is_whitespace) {
                let _ = writeln!(Stdout, "usage: {command} <file>");
            } else if command == "touch" {
                write_file(command, path, 0, "");
            } else {
                let result = sys_unlink(path);
                if result < 0 {
                    let _ = writeln!(Stdout, "rm: cannot remove {path} ({result})");
                }
            }
        }
        "write" | "append" => {
            if let Some((path, text)) = arg.trim_end().split_once(char::is_whitespace) {
                let flags = if command == "write" { O_TRUNC } else { O_APPEND };
                write_file(command, path, flags, text.trim_start());
            } else {
                let _ = writeln!(Stdout, "usage: {command} <file> <text>");
            }
        }
        "uptime" => {
            let _ = writeln!(Stdout, "uptime: {} ms", sys_uptime_ms());
        }
        "run" => run(arg.trim_end()),
        "pipetest" => pipetest(),
        "exit" if arg.is_empty() => sys_exit(0),
        "exit" => match arg.trim_end().parse() {
            Ok(code) => sys_exit(code),
            Err(_) => print("usage: exit [code]\n"),
        },
        _ => print("unknown command (type 'help')\n"),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    print("Aegis EL0 shell (sh.elf) — type 'help'\n");
    print("sh> ");

    let mut line = [0; LINE_BUF_LEN];
    let mut len = 0;
    let mut last_cr = false;
    let mut input = [0];

    loop {
        match sys_read(STDIN, &mut input) {
            1 => {}
            0 => sys_exit(0),
            _ => {
                print("[sh] input failed\n");
                sys_exit(1);
            }
        }

        let byte = input[0];
        if byte == b'\n' && last_cr {
            last_cr = false;
            continue;
        }
        last_cr = byte == b'\r';
        match byte {
            b'\r' | b'\n' => {
                print("\n");
                if let Ok(command) = core::str::from_utf8(&line[..len]) {
                    execute(command);
                }
                len = 0;
                print("sh> ");
            }
            0x08 | 0x7f if len > 0 => {
                len -= 1;
                print("\x08 \x08");
            }
            0x20..=0x7e if len < line.len() => {
                line[len] = byte;
                len += 1;
                let _ = write_all(STDOUT, &input);
            }
            _ => {}
        }
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys_exit(1);
}
