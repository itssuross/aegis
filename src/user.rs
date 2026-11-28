use alloc::vec::Vec;
use core::arch::{asm, naked_asm};
use core::sync::atomic::{AtomicU16, AtomicUsize, Ordering};

use crate::fd::{
    EBADF, FILE_NAME_SIZE, FdKind, FdTable, O_APPEND, O_CREAT, O_RDONLY, O_RDWR, O_TRUNC, O_WRONLY,
};
use crate::mmu::{PageFlags, PageTable};
use crate::pipe::{self, PipeHandle};
use crate::platform::{PAGE_SIZE, phys_to_virt};
use crate::scheduler::WaitQueue;
use crate::sync::IrqSpinLock;

const ELF_HDR_SIZE: usize = 64;
const ELF_PHDR_SIZE: usize = 56;
const ELF_MAGIC: [u8; 4] = *b"\x7FELF";
const ELFCLASS64: u8 = 2;
const ELFDATA2LSB: u8 = 1;
const ET_EXEC: u16 = 2;
const EM_AARCH64: u16 = 0xB7;
const PT_LOAD: u32 = 1;
const PF_X: u32 = 1;
const PF_W: u32 = 2;

const USER_VA_BASE: usize = 0x0040_0000;
const USER_STACK_GUARD: usize = 0x007F_E000; // 4 KiB unmapped guard page
const USER_STACK_BASE: usize = 0x007F_F000;
const USER_STACK_TOP: usize = 0x0080_0000;
const MAX_PROCS: usize = 16;
const EXIT_HISTORY_LEN: usize = 32;

const ENOENT: i64 = -2;
const ESRCH: i64 = -3;
const EIO: i64 = -5;
const ENOEXEC: i64 = -8;
const ECHILD: i64 = -10;
const EAGAIN: i64 = -11;
const ENOMEM: i64 = -12;
const EINVAL: i64 = -22;
const ENFILE: i64 = -23;
const EMFILE: i64 = -24;
const ENOSPC: i64 = -28;

const _: () = assert!(USER_STACK_GUARD + PAGE_SIZE == USER_STACK_BASE);
const _: () = assert!(USER_STACK_BASE + PAGE_SIZE == USER_STACK_TOP);

static NEXT_ASID: AtomicU16 = AtomicU16::new(1);
static NEXT_PID: AtomicUsize = AtomicUsize::new(1);
static REAPER_STARTED: IrqSpinLock<bool> = IrqSpinLock::new(false);
static PROC_WAITQ: WaitQueue = WaitQueue::new();

struct UserAddressSpace {
    asid: u16,
    pt: Option<PageTable>,
    frames: Vec<usize>,
}

impl UserAddressSpace {
    fn ttbr0(&self) -> u64 {
        let root = self.pt.as_ref().expect("page table missing").root_phys() as u64;
        ((self.asid as u64) << 48) | root
    }
}

impl Drop for UserAddressSpace {
    fn drop(&mut self) {
        if self.asid != 0 {
            crate::mmu::flush_asid(self.asid);
        }
        drop(self.pt.take());
        for phys in self.frames.drain(..) {
            crate::pmm::free_frame(phys);
        }
    }
}

struct Process {
    pid: usize,
    parent_pid: Option<usize>,
    tid: Option<usize>,
    asid: u16,
    entry: usize,
    user_sp: usize,
    addr_space: Option<UserAddressSpace>,
    fds: Option<FdTable>,
    exit_code: Option<i64>,
    completed: bool,
    reaping: bool,
}

#[derive(Clone, Copy)]
struct ExitRecord {
    pid: usize,
    parent_pid: Option<usize>,
    code: i64,
}

struct ProcessTable {
    procs: [Option<Process>; MAX_PROCS],
    exits: [Option<ExitRecord>; EXIT_HISTORY_LEN],
    exit_cursor: usize,
}

impl ProcessTable {
    const fn new() -> Self {
        Self {
            procs: [const { None }; MAX_PROCS],
            exits: [None; EXIT_HISTORY_LEN],
            exit_cursor: 0,
        }
    }

    fn by_tid(&mut self, tid: usize) -> Option<&mut Process> {
        self.procs
            .iter_mut()
            .flatten()
            .find(|proc| !proc.completed && proc.tid == Some(tid))
    }

    fn record_exit(&mut self, exit: ExitRecord) {
        self.exits[self.exit_cursor] = Some(exit);
        self.exit_cursor = (self.exit_cursor + 1) % EXIT_HISTORY_LEN;
    }

    fn take_exit(&mut self, pid: usize) -> Option<i64> {
        for slot in &mut self.exits {
            if slot.is_some_and(|exit| exit.pid == pid) {
                return slot.take().map(|exit| exit.code);
            }
        }
        None
    }

    fn has_exit(&self, pid: usize) -> bool {
        self.exits.iter().flatten().any(|exit| exit.pid == pid)
    }

    fn has_child(&self, parent_pid: usize, pid: usize) -> bool {
        self.procs
            .iter()
            .flatten()
            .any(|proc| proc.pid == pid && proc.parent_pid == Some(parent_pid))
            || self
                .exits
                .iter()
                .flatten()
                .any(|exit| exit.pid == pid && exit.parent_pid == Some(parent_pid))
    }
}

static PROCESSES: IrqSpinLock<ProcessTable> = IrqSpinLock::new(ProcessTable::new());

pub enum ElfBytes<'a> {
    Borrowed(&'a [u8]),
    Owned(Vec<u8>),
}

impl ElfBytes<'_> {
    fn as_slice(&self) -> &[u8] {
        match self {
            Self::Borrowed(slice) => slice,
            Self::Owned(vec) => vec.as_slice(),
        }
    }
}

pub trait ElfSource<'a> {
    fn into_elf_bytes(self) -> Result<ElfBytes<'a>, &'static str>;
}

impl<'a> ElfSource<'a> for &'a str {
    fn into_elf_bytes(self) -> Result<ElfBytes<'a>, &'static str> {
        crate::fs::read_file(self).map(ElfBytes::Owned)
    }
}

impl<'a> ElfSource<'a> for &'a [u8] {
    fn into_elf_bytes(self) -> Result<ElfBytes<'a>, &'static str> {
        Ok(ElfBytes::Borrowed(self))
    }
}

impl<'a> ElfSource<'a> for &'a Vec<u8> {
    fn into_elf_bytes(self) -> Result<ElfBytes<'a>, &'static str> {
        Ok(ElfBytes::Borrowed(self.as_slice()))
    }
}

#[inline]
fn read_u16(buf: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([buf[offset], buf[offset + 1]])
}

#[inline]
fn read_u32(buf: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        buf[offset],
        buf[offset + 1],
        buf[offset + 2],
        buf[offset + 3],
    ])
}

#[inline]
fn read_u64(buf: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes([
        buf[offset],
        buf[offset + 1],
        buf[offset + 2],
        buf[offset + 3],
        buf[offset + 4],
        buf[offset + 5],
        buf[offset + 6],
        buf[offset + 7],
    ])
}

fn alloc_asid(procs: &[Option<Process>; MAX_PROCS]) -> u16 {
    loop {
        let asid = loop {
            let cur = NEXT_ASID.load(Ordering::Relaxed);
            let next = if cur == u16::MAX { 1 } else { cur + 1 };
            if NEXT_ASID
                .compare_exchange_weak(cur, next, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                break cur;
            }
        };
        if !procs.iter().flatten().any(|proc| proc.asid == asid) {
            return asid;
        }
    }
}

// SAFETY: Caller guarantees active `TTBR0_EL1` maps `entry` as `UserRx` and `user_sp` as the top
// of a mapped `UserRw` stack for the current EL0 task.
#[unsafe(naked)]
unsafe extern "C" fn enter_el0(entry: usize, user_sp: usize) -> ! {
    naked_asm!(
        "msr daifset, #2",
        "isb",
        "msr sp_el0, x1",
        "msr elr_el1, x0",
        "msr spsr_el1, xzr", // EL0t, DAIF unmasked
        "mov x0, xzr",
        "mov x1, xzr",
        "mov x2, xzr",
        "mov x3, xzr",
        "mov x4, xzr",
        "mov x5, xzr",
        "mov x6, xzr",
        "mov x7, xzr",
        "mov x8, xzr",
        "mov x9, xzr",
        "mov x10, xzr",
        "mov x11, xzr",
        "mov x12, xzr",
        "mov x13, xzr",
        "mov x14, xzr",
        "mov x15, xzr",
        "mov x16, xzr",
        "mov x17, xzr",
        "mov x18, xzr",
        "mov x19, xzr",
        "mov x20, xzr",
        "mov x21, xzr",
        "mov x22, xzr",
        "mov x23, xzr",
        "mov x24, xzr",
        "mov x25, xzr",
        "mov x26, xzr",
        "mov x27, xzr",
        "mov x28, xzr",
        "mov x29, xzr",
        "mov x30, xzr",
        "eret",
    );
}

extern "C" fn el0_task_entry(proc_idx: usize) -> ! {
    let tid = crate::scheduler::current_tid();
    let (entry, user_sp) = {
        let mut table = PROCESSES.lock();
        let proc = table.procs[proc_idx]
            .as_mut()
            .expect("el0_task_entry: missing process control block");
        proc.tid = Some(tid);
        (proc.entry, proc.user_sp)
    };
    // SAFETY: `scheduler::spawn_with_ttbr0` activated this process's `TTBR0_EL1`, which maps
    // `entry` as `UserRx` and `user_sp - PAGE_SIZE..user_sp` as `UserRw`.
    unsafe { enter_el0(entry, user_sp) }
}

extern "C" fn el0_reaper(_arg: usize) -> ! {
    loop {
        PROC_WAITQ.wait_until(|| {
            let table = PROCESSES.lock();
            table
                .procs
                .iter()
                .flatten()
                .any(|proc| proc.completed && !proc.reaping)
        });

        loop {
            let candidate = {
                let mut table = PROCESSES.lock();
                let mut picked = None;
                for (idx, slot) in table.procs.iter_mut().enumerate() {
                    if let Some(proc) = slot.as_mut()
                        && proc.completed
                        && !proc.reaping
                    {
                        proc.reaping = true;
                        picked = Some((
                            idx,
                            proc.tid,
                            ExitRecord {
                                pid: proc.pid,
                                parent_pid: proc.parent_pid,
                                code: proc.exit_code.unwrap_or(-1),
                            },
                        ));
                        break;
                    }
                }
                picked
            };

            let Some((proc_idx, tid, exit)) = candidate else {
                break;
            };

            if let Some(tid) = tid {
                crate::scheduler::join(tid);
            }

            let reaped = {
                let mut table = PROCESSES.lock();
                let taken = if table.procs[proc_idx]
                    .as_ref()
                    .is_some_and(|proc| proc.pid == exit.pid)
                {
                    table.procs[proc_idx].take()
                } else {
                    None
                };
                table.record_exit(exit);
                taken
            };
            drop(reaped);
            PROC_WAITQ.wake_all();
        }
    }
}

fn ensure_reaper_started() -> Result<(), &'static str> {
    let mut started = REAPER_STARTED.lock();
    if !*started {
        crate::scheduler::spawn_with_ttbr0("reaper", el0_reaper, 0, crate::mmu::kernel_ttbr0())
            .ok_or("user: scheduler task slots exhausted")?;
        *started = true;
    }
    Ok(())
}

pub fn exit_to_el1(code: i32) -> ! {
    let tid = crate::scheduler::current_tid();
    // SAFETY: Clearing `SP_EL0` at EL1 before exiting the EL0 thread has no memory side effects.
    unsafe {
        asm!("msr sp_el0, xzr", options(nomem, nostack, preserves_flags));
    }
    crate::scheduler::set_current_ttbr0(crate::mmu::kernel_ttbr0());

    let fds = PROCESSES
        .lock()
        .by_tid(tid)
        .and_then(|proc| proc.fds.take());
    if let Some(mut fds) = fds {
        fds.close_all();
    }

    let addr_space = {
        let mut table = PROCESSES.lock();
        table.by_tid(tid).and_then(|proc| {
            proc.exit_code = Some(i64::from(code));
            proc.completed = true;
            proc.addr_space.take()
        })
    };
    drop(addr_space);
    PROC_WAITQ.wake_all();

    crate::scheduler::exit_current();
}

fn load_elf_image(image: &[u8]) -> Result<(UserAddressSpace, usize), &'static str> {
    if image.len() < ELF_HDR_SIZE {
        return Err("elf: image shorter than 64-byte header");
    }
    if image[0..4] != ELF_MAGIC {
        return Err("elf: invalid ELF magic");
    }
    if image[4] != ELFCLASS64 {
        return Err("elf: expected 64-bit ELF");
    }
    if image[5] != ELFDATA2LSB {
        return Err("elf: expected little-endian ELF");
    }
    if read_u16(image, 16) != ET_EXEC {
        return Err("elf: expected ET_EXEC image");
    }
    if read_u16(image, 18) != EM_AARCH64 {
        return Err("elf: expected AArch64 machine");
    }

    let entry = read_u64(image, 24) as usize;
    if !(USER_VA_BASE..USER_STACK_GUARD).contains(&entry) {
        return Err("elf: entry point outside user memory");
    }

    let phoff = read_u64(image, 32) as usize;
    let phentsize = usize::from(read_u16(image, 54));
    let phnum = usize::from(read_u16(image, 56));
    if phnum == 0 {
        return Err("elf: no program headers");
    }
    if phentsize < ELF_PHDR_SIZE {
        return Err("elf: invalid program header entry size");
    }

    let ph_table_end = phnum
        .checked_mul(phentsize)
        .and_then(|size| phoff.checked_add(size))
        .ok_or("elf: program header table overflow")?;
    if ph_table_end > image.len() {
        return Err("elf: program header table out of bounds");
    }

    let mut load_segments = 0usize;
    let mut entry_executable = false;
    for index in 0..phnum {
        let ph = &image[phoff + index * phentsize..][..ELF_PHDR_SIZE];
        if read_u32(ph, 0) != PT_LOAD {
            continue;
        }
        let p_flags = read_u32(ph, 4);
        let p_offset = read_u64(ph, 8) as usize;
        let p_vaddr = read_u64(ph, 16) as usize;
        let p_filesz = read_u64(ph, 32) as usize;
        let p_memsz = read_u64(ph, 40) as usize;

        if (p_flags & PF_W) != 0 && (p_flags & PF_X) != 0 {
            return Err("elf: segment violates W^X");
        }
        if p_filesz > p_memsz {
            return Err("elf: segment filesz exceeds memsz");
        }
        let file_end = p_offset
            .checked_add(p_filesz)
            .ok_or("elf: segment file offset overflow")?;
        if file_end > image.len() {
            return Err("elf: segment exceeds image bounds");
        }
        let seg_end = p_vaddr
            .checked_add(p_memsz)
            .ok_or("elf: segment address overflow")?;
        if p_vaddr < USER_VA_BASE
            || seg_end > USER_STACK_GUARD
            || (p_memsz == 0 && p_vaddr >= USER_STACK_GUARD)
        {
            return Err("elf: segment outside user memory");
        }
        if (p_flags & PF_X) != 0 && (p_vaddr..seg_end).contains(&entry) {
            entry_executable = true;
        }
        load_segments += 1;
    }
    if load_segments == 0 {
        return Err("elf: no PT_LOAD segments");
    }
    if !entry_executable {
        return Err("elf: entry point not in executable segment");
    }

    let pt = PageTable::new().ok_or("elf: out of physical memory")?;
    let mut addr_space = UserAddressSpace {
        asid: 0,
        pt: Some(pt),
        frames: Vec::new(),
    };

    let page_mask = !(PAGE_SIZE - 1);
    let mut mapped_pages: Vec<(usize, usize, u32)> = Vec::new();
    for index in 0..phnum {
        let ph = &image[phoff + index * phentsize..][..ELF_PHDR_SIZE];
        if read_u32(ph, 0) != PT_LOAD {
            continue;
        }
        let p_flags = read_u32(ph, 4);
        let p_offset = read_u64(ph, 8) as usize;
        let p_vaddr = read_u64(ph, 16) as usize;
        let p_filesz = read_u64(ph, 32) as usize;
        let p_memsz = read_u64(ph, 40) as usize;
        if p_memsz == 0 {
            continue;
        }

        let first_page = p_vaddr & page_mask;
        let last_page = (p_vaddr + p_memsz - 1) & page_mask;
        let file_end = p_vaddr + p_filesz;

        let mut va_page = first_page;
        loop {
            let phys = if let Some(entry) = mapped_pages.iter_mut().find(|p| p.0 == va_page) {
                let combined = entry.2 | p_flags;
                if (combined & PF_W) != 0 && (combined & PF_X) != 0 {
                    return Err("elf: overlapping W+X segments on same page");
                }
                entry.2 = combined;
                entry.1
            } else {
                let phys = crate::pmm::alloc_frame().ok_or("elf: out of physical memory")?;
                addr_space.frames.push(phys);
                mapped_pages.push((va_page, phys, p_flags));
                phys
            };

            let copy_lo = va_page.max(p_vaddr);
            let copy_hi = (va_page + PAGE_SIZE).min(file_end);
            if copy_lo < copy_hi {
                let dst_off = copy_lo - va_page;
                let src_off = p_offset + (copy_lo - p_vaddr);
                let copy_len = copy_hi - copy_lo;
                // SAFETY: `src_off..src_off + copy_len` was bounds-checked against `image`, and
                // the destination lies within the direct map of our exclusively owned 4 KiB frame.
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        image.as_ptr().add(src_off),
                        (phys_to_virt(phys) + dst_off) as *mut u8,
                        copy_len,
                    );
                }
            }

            if va_page == last_page {
                break;
            }
            va_page += PAGE_SIZE;
        }
    }

    let pt = addr_space.pt.as_mut().expect("page table missing");
    for &(va_page, phys, p_flags) in &mapped_pages {
        let flags = if (p_flags & PF_X) != 0 {
            PageFlags::UserRx
        } else if (p_flags & PF_W) != 0 {
            PageFlags::UserRw
        } else {
            PageFlags::UserRo
        };
        if flags == PageFlags::UserRx {
            let page_virt = phys_to_virt(phys);
            let mut line = page_virt;
            while line < page_virt + PAGE_SIZE {
                // SAFETY: `line` is a 64-byte aligned address in the direct map of an owned frame.
                unsafe {
                    asm!("dc cvau, {}", in(reg) line, options(nostack, preserves_flags));
                }
                line += 64;
            }
        }
        pt.map_4k(va_page, phys, flags)?;
    }

    let stack_phys = crate::pmm::alloc_frame().ok_or("elf: out of physical memory")?;
    addr_space.frames.push(stack_phys);
    addr_space.pt.as_mut().expect("page table missing").map_4k(
        USER_STACK_BASE,
        stack_phys,
        PageFlags::UserRw,
    )?;

    // SAFETY: Ensures PTE and code writes are visible and invalidates stale I-cache lines across SMP cores.
    unsafe {
        asm!(
            "dsb ish",
            "ic ialluis",
            "dsb ish",
            "isb",
            options(nostack, preserves_flags),
        );
    }

    Ok((addr_space, entry))
}

fn spawn_process(image: &[u8], parent_pid: Option<usize>) -> Result<usize, &'static str> {
    let (mut addr_space, entry) = load_elf_image(image)?;

    let (proc_idx, pid, ttbr0) = {
        let mut table = PROCESSES.lock();
        let Some(slot_idx) = table.procs.iter().position(Option::is_none) else {
            drop(table);
            drop(addr_space);
            return Err("user: process table full");
        };
        let pid = NEXT_PID.fetch_add(1, Ordering::Relaxed);
        let asid = alloc_asid(&table.procs);
        addr_space.asid = asid;
        let ttbr0 = addr_space.ttbr0();
        table.procs[slot_idx] = Some(Process {
            pid,
            parent_pid,
            tid: None,
            asid,
            entry,
            user_sp: USER_STACK_TOP,
            addr_space: Some(addr_space),
            fds: Some(FdTable::new_stdio()),
            exit_code: None,
            completed: false,
            reaping: false,
        });
        (slot_idx, pid, ttbr0)
    };

    let Some(tid) = crate::scheduler::spawn_with_ttbr0("el0", el0_task_entry, proc_idx, ttbr0)
    else {
        let failed = {
            let mut table = PROCESSES.lock();
            if table.procs[proc_idx]
                .as_ref()
                .is_some_and(|proc| proc.pid == pid)
            {
                table.procs[proc_idx].take()
            } else {
                None
            }
        };
        drop(failed);
        return Err("user: scheduler task slots exhausted");
    };

    {
        let mut table = PROCESSES.lock();
        if let Some(proc) = table.procs[proc_idx].as_mut()
            && proc.pid == pid
        {
            proc.tid = Some(tid);
        }
    }

    Ok(pid)
}

pub fn wait_pid(pid: usize) -> Result<i64, &'static str> {
    loop {
        PROC_WAITQ.wait_until(|| {
            let table = PROCESSES.lock();
            if table.has_exit(pid) {
                return true;
            }
            match table.procs.iter().flatten().find(|proc| proc.pid == pid) {
                Some(proc) => proc.completed && !proc.reaping,
                None => true,
            }
        });

        let claim = {
            let mut table = PROCESSES.lock();
            if let Some(exit_code) = table.take_exit(pid) {
                return Ok(exit_code);
            }
            let mut found = None;
            let mut exists = false;
            for (idx, slot) in table.procs.iter_mut().enumerate() {
                if let Some(proc) = slot.as_mut()
                    && proc.pid == pid
                {
                    exists = true;
                    if proc.completed && !proc.reaping {
                        proc.reaping = true;
                        found = Some((idx, proc.tid, proc.exit_code.unwrap_or(-1)));
                    }
                    break;
                }
            }
            if !exists {
                return Err("user: unknown pid");
            }
            found
        };

        let Some((proc_idx, tid, exit_code)) = claim else {
            continue;
        };

        if let Some(tid) = tid {
            crate::scheduler::join(tid);
        }

        let reaped = {
            let mut table = PROCESSES.lock();
            if table.procs[proc_idx]
                .as_ref()
                .is_some_and(|proc| proc.pid == pid)
            {
                table.procs[proc_idx].take()
            } else {
                None
            }
        };
        drop(reaped);
        PROC_WAITQ.wake_all();
        return Ok(exit_code);
    }
}

pub fn exec_elf<'a, S: ElfSource<'a>>(source: S) -> Result<i64, &'static str> {
    let bytes = source.into_elf_bytes()?;
    let pid = spawn_process(bytes.as_slice(), None)?;
    drop(bytes);
    wait_pid(pid)
}

pub fn spawn_elf(filename: &str) -> Result<usize, &'static str> {
    let tid = crate::scheduler::current_tid();
    let parent_pid = PROCESSES.lock().by_tid(tid).map(|proc| proc.pid);
    let bytes = crate::fs::read_file(filename)?;
    ensure_reaper_started()?;
    spawn_process(&bytes, parent_pid)
}

fn with_current_fds<T>(op: impl FnOnce(&mut FdTable) -> Result<T, i64>) -> Result<T, i64> {
    let tid = crate::scheduler::current_tid();
    // Each process has one thread. Blocking I/O runs with its table outside PROCESSES.
    let mut fds = PROCESSES
        .lock()
        .by_tid(tid)
        .ok_or(ESRCH)?
        .fds
        .take()
        .expect("current process descriptor table in use");
    let result = op(&mut fds);
    PROCESSES
        .lock()
        .by_tid(tid)
        .expect("missing current process")
        .fds = Some(fds);
    result
}

pub fn sys_read(fd: usize, buf: &mut [u8]) -> i64 {
    with_current_fds(|fds| match fds.get_mut(fd) {
        Some(FdKind::ConsoleIn) => {
            let mut count = 0;
            while let Some(dst) = buf.get_mut(count) {
                if let Some(byte) = crate::uart::try_read_byte() {
                    *dst = byte;
                    count += 1;
                } else if count == 0 {
                    crate::uart::wait_for_input();
                } else {
                    break;
                }
            }
            Ok(count)
        }
        Some(FdKind::AegisFile {
            name,
            name_len,
            readable: true,
            offset,
            ..
        }) => {
            let path = core::str::from_utf8(&name[..*name_len]).expect("validated file name");
            let count = crate::fs::read_named_at(path, *offset, buf).map_err(file_errno)?;
            *offset += count;
            Ok(count)
        }
        Some(FdKind::PipeRead(end)) => pipe::read(end, buf),
        _ => Err(EBADF),
    })
    .map_or_else(|errno| errno, |count| count as i64)
}

pub fn sys_write(fd: usize, buf: &[u8]) -> i64 {
    with_current_fds(|fds| match fds.get_mut(fd) {
        Some(FdKind::ConsoleOut) => {
            for &byte in buf {
                crate::uart::write_byte(byte);
            }
            Ok(buf.len())
        }
        Some(FdKind::AegisFile {
            name,
            name_len,
            writable: true,
            append,
            offset,
            ..
        }) => {
            let path = core::str::from_utf8(&name[..*name_len]).expect("validated file name");
            if *append {
                let end = crate::fs::append_at_end(path, buf).map_err(file_errno)?;
                if !buf.is_empty() {
                    *offset = end;
                }
                Ok(buf.len())
            } else {
                let count = crate::fs::write_at(path, *offset, buf).map_err(file_errno)?;
                *offset += count;
                Ok(count)
            }
        }
        Some(FdKind::PipeWrite(end)) => pipe::write(end, buf),
        _ => Err(EBADF),
    })
    .map_or_else(|errno| errno, |count| count as i64)
}

fn file_errno(error: &str) -> i64 {
    match error {
        "fs: file not found" => ENOENT,
        "fs: file too large for available memory" => ENOMEM,
        "fs: invalid filename" => EINVAL,
        "fs: directory full" | "fs: no contiguous space" | "fs: file too large" => ENOSPC,
        _ => EIO,
    }
}

pub fn sys_open(path: &str, flags: u32) -> i64 {
    const ACCESS_MODE: u32 = O_WRONLY | O_RDWR;
    if flags & !(ACCESS_MODE | O_CREAT | O_TRUNC | O_APPEND) != 0 {
        return EINVAL;
    }
    let (readable, writable) = match flags & ACCESS_MODE {
        O_RDONLY => (true, false),
        O_WRONLY => (false, true),
        O_RDWR => (true, true),
        _ => return EINVAL,
    };
    let create = flags & O_CREAT != 0;
    let truncate = flags & O_TRUNC != 0;
    if truncate && !writable {
        return EINVAL;
    }

    with_current_fds(|fds| {
        if !fds.has_capacity(1) {
            return Err(EMFILE);
        }
        let path = path.strip_prefix("./").unwrap_or(path);
        if !create && !truncate {
            crate::fs::find(path).ok_or(ENOENT)?;
        } else if create && truncate {
            crate::fs::write_file(path, &[]).map_err(file_errno)?;
        } else {
            crate::fs::open_file(path, create, truncate).map_err(file_errno)?;
        }
        let mut name = [0; FILE_NAME_SIZE];
        name[..path.len()].copy_from_slice(path.as_bytes());
        fds.alloc(FdKind::AegisFile {
            name,
            name_len: path.len(),
            readable,
            writable,
            append: flags & O_APPEND != 0,
            offset: 0,
        })
        .ok_or(EMFILE)
    })
    .map_or_else(|errno| errno, |fd| fd as i64)
}

pub fn sys_unlink(path: &str) -> i64 {
    let path = path.strip_prefix("./").unwrap_or(path);
    crate::fs::remove_file(path).map_or_else(file_errno, |()| 0)
}

pub fn sys_close(fd: usize) -> i64 {
    with_current_fds(|fds| fds.close(fd)).map_or_else(|errno| errno, |()| 0)
}

pub fn sys_pipe() -> Result<(usize, usize), i64> {
    with_current_fds(|fds| {
        if !fds.has_capacity(2) {
            return Err(EMFILE);
        }
        let (read, write) = PipeHandle::create().ok_or(ENFILE)?;
        let read_fd = fds.alloc(FdKind::PipeRead(read)).expect("free pipe fd");
        let write_fd = fds.alloc(FdKind::PipeWrite(write)).expect("free pipe fd");
        Ok((read_fd, write_fd))
    })
}

pub fn sys_spawn(path: &str) -> i64 {
    match spawn_elf(path) {
        Ok(pid) => pid as i64,
        Err("user: process table full" | "user: scheduler task slots exhausted") => EAGAIN,
        Err("elf: out of physical memory") => ENOMEM,
        Err(error) if error.starts_with("elf:") => ENOEXEC,
        Err(error) => file_errno(error),
    }
}

pub fn sys_waitpid(pid: usize) -> i64 {
    let tid = crate::scheduler::current_tid();
    let mut table = PROCESSES.lock();
    let Some(parent_pid) = table.by_tid(tid).map(|proc| proc.pid) else {
        return ESRCH;
    };
    if !table.has_child(parent_pid, pid) {
        return ECHILD;
    }
    drop(table);
    wait_pid(pid).unwrap_or(ECHILD)
}
