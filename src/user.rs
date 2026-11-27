use alloc::vec::Vec;
use core::arch::{asm, naked_asm};
use core::sync::atomic::{AtomicU16, Ordering};

use crate::mmu::{PageFlags, PageTable};
use crate::platform::PAGE_SIZE;
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

const _: () = assert!(USER_STACK_GUARD + PAGE_SIZE == USER_STACK_BASE);
const _: () = assert!(USER_STACK_BASE + PAGE_SIZE == USER_STACK_TOP);

static NEXT_ASID: AtomicU16 = AtomicU16::new(1);

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
        for phys in self.frames.drain(..) {
            crate::pmm::free_frame(phys);
        }
        drop(self.pt.take());
        if self.asid != 0 {
            crate::mmu::flush_asid(self.asid);
        }
    }
}

struct Process {
    tid: Option<usize>,
    entry: usize,
    user_sp: usize,
    addr_space: UserAddressSpace,
    exit_code: Option<i64>,
    completed: bool,
    detached: bool,
}

static PROCESSES: IrqSpinLock<[Option<Process>; MAX_PROCS]> =
    IrqSpinLock::new([const { None }; MAX_PROCS]);

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
        if !procs
            .iter()
            .flatten()
            .any(|proc| proc.addr_space.asid == asid)
        {
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
        let mut procs = PROCESSES.lock();
        let proc = procs[proc_idx]
            .as_mut()
            .expect("el0_task_entry: missing process control block");
        proc.tid = Some(tid);
        (proc.entry, proc.user_sp)
    };
    // SAFETY: `scheduler::spawn_with_ttbr0` activated this process's `TTBR0_EL1`, which maps
    // `entry` as `UserRx` and `user_sp - PAGE_SIZE..user_sp` as `UserRw`.
    unsafe { enter_el0(entry, user_sp) }
}

pub fn exit_to_el1(code: i32) -> ! {
    let tid = crate::scheduler::current_tid();
    // SAFETY: Clearing `SP_EL0` at EL1 before exiting the EL0 thread has no memory side effects.
    unsafe {
        asm!("msr sp_el0, xzr", options(nomem, nostack, preserves_flags));
    }
    crate::scheduler::set_current_ttbr0(crate::mmu::kernel_ttbr0());

    let detached_proc = {
        let mut procs = PROCESSES.lock();
        let mut found = None;
        for slot in procs.iter_mut() {
            if let Some(proc) = slot.as_mut()
                && proc.tid == Some(tid)
            {
                proc.exit_code = Some(i64::from(code));
                proc.completed = true;
                if proc.detached {
                    found = slot.take();
                }
                break;
            }
        }
        found
    };
    drop(detached_proc);

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

    let mut pt = PageTable::new().ok_or("elf: out of physical memory")?;
    pt.map_kernel_regions()?;
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
                // `phys + dst_off..phys + dst_off + copy_len` lies within our exclusively owned 4 KiB frame.
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        image.as_ptr().add(src_off),
                        (phys + dst_off) as *mut u8,
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
            let mut line = phys;
            while line < phys + PAGE_SIZE {
                // SAFETY: `line` is a 64-byte aligned address in an owned identity-mapped RAM frame.
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
    addr_space
        .pt
        .as_mut()
        .expect("page table missing")
        .map_4k(USER_STACK_BASE, stack_phys, PageFlags::UserRw)?;

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

fn spawn_process(image: &[u8], detached: bool) -> Result<(usize, usize), &'static str> {
    let (mut addr_space, entry) = load_elf_image(image)?;

    let (proc_idx, ttbr0) = {
        let mut procs = PROCESSES.lock();
        let Some(slot_idx) = procs.iter().position(Option::is_none) else {
            drop(procs);
            drop(addr_space);
            return Err("user: process table full");
        };
        addr_space.asid = alloc_asid(&procs);
        let ttbr0 = addr_space.ttbr0();
        procs[slot_idx] = Some(Process {
            tid: None,
            entry,
            user_sp: USER_STACK_TOP,
            addr_space,
            exit_code: None,
            completed: false,
            detached,
        });
        (slot_idx, ttbr0)
    };

    let Some(tid) = crate::scheduler::spawn_with_ttbr0("el0", el0_task_entry, proc_idx, ttbr0)
    else {
        let failed = PROCESSES.lock()[proc_idx].take();
        drop(failed);
        return Err("user: scheduler task slots exhausted");
    };

    Ok((proc_idx, tid))
}

pub fn exec_elf<'a, S: ElfSource<'a>>(source: S) -> Result<i64, &'static str> {
    let bytes = source.into_elf_bytes()?;
    let (proc_idx, tid) = spawn_process(bytes.as_slice(), false)?;
    drop(bytes);

    crate::scheduler::join(tid);

    let proc = PROCESSES.lock()[proc_idx].take();
    let Some(proc) = proc else {
        return Err("user: missing completed process state");
    };
    let exit_code = if proc.completed {
        proc.exit_code.unwrap_or(-1)
    } else {
        -1
    };
    drop(proc);
    Ok(exit_code)
}

#[allow(dead_code)]
pub fn spawn_elf(filename: &str) -> Result<usize, &'static str> {
    let bytes = crate::fs::read_file(filename)?;
    let (_proc_idx, tid) = spawn_process(&bytes, true)?;
    Ok(tid)
}
