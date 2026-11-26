use core::arch::{asm, naked_asm};
use core::sync::atomic::{AtomicBool, Ordering};

use crate::mmu::{USER_MEM_BASE, USER_MEM_SIZE};

const ELF_HDR_SIZE: usize = 64;
const ELF_PHDR_SIZE: usize = 56;
const ELF_MAGIC: [u8; 4] = *b"\x7FELF";
const ELFCLASS64: u8 = 2;
const ELFDATA2LSB: u8 = 1;
const ET_EXEC: u16 = 2;
const EM_AARCH64: u16 = 0xB7;
const PT_LOAD: u32 = 1;

#[repr(C)]
#[allow(dead_code)]
struct El1ReturnCtx {
    callee_regs: [u64; 12],
    sp: u64,
    daif: u64,
    exit_code: i32,
}

const _: () = assert!(core::mem::offset_of!(El1ReturnCtx, sp) == 96);
const _: () = assert!(core::mem::offset_of!(El1ReturnCtx, daif) == 104);
const _: () = assert!(core::mem::offset_of!(El1ReturnCtx, exit_code) == 112);

static mut EL1_RETURN_CTX: El1ReturnCtx = El1ReturnCtx {
    callee_regs: [0; 12],
    sp: 0,
    daif: 0,
    exit_code: 0,
};

static EL0_ACTIVE: AtomicBool = AtomicBool::new(false);

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

// SAFETY: Caller ensures `entry` and `user_sp` are valid EL0 addresses and `EL1_RETURN_CTX` is exclusively owned.
#[unsafe(naked)]
unsafe extern "C" fn enter_el0(entry: usize, user_sp: usize) -> i32 {
    naked_asm!(
        "adrp x9, {ctx}",
        "add  x9, x9, :lo12:{ctx}",
        "stp  x19, x20, [x9, #0]",
        "stp  x21, x22, [x9, #16]",
        "stp  x23, x24, [x9, #32]",
        "stp  x25, x26, [x9, #48]",
        "stp  x27, x28, [x9, #64]",
        "stp  x29, x30, [x9, #80]",
        "mov  x10, sp",
        "mrs  x11, daif",
        "stp  x10, x11, [x9, #96]",
        "msr  sp_el0, x1",
        "msr  elr_el1, x0",
        "msr  spsr_el1, xzr",
        "mov  x0, xzr",
        "mov  x1, xzr",
        "mov  x2, xzr",
        "mov  x3, xzr",
        "mov  x4, xzr",
        "mov  x5, xzr",
        "mov  x6, xzr",
        "mov  x7, xzr",
        "mov  x8, xzr",
        "mov  x9, xzr",
        "mov  x10, xzr",
        "mov  x11, xzr",
        "mov  x12, xzr",
        "mov  x13, xzr",
        "mov  x14, xzr",
        "mov  x15, xzr",
        "mov  x16, xzr",
        "mov  x17, xzr",
        "mov  x18, xzr",
        "mov  x19, xzr",
        "mov  x20, xzr",
        "mov  x21, xzr",
        "mov  x22, xzr",
        "mov  x23, xzr",
        "mov  x24, xzr",
        "mov  x25, xzr",
        "mov  x26, xzr",
        "mov  x27, xzr",
        "mov  x28, xzr",
        "mov  x29, xzr",
        "mov  x30, xzr",
        "eret",
        ctx = sym EL1_RETURN_CTX,
    );
}

// SAFETY: Caller ensures `EL1_RETURN_CTX` contains a live continuation saved by `enter_el0`.
#[unsafe(naked)]
unsafe extern "C" fn restore_el1(code: i32) -> ! {
    naked_asm!(
        "msr  daifset, #2",
        "isb",
        "adrp x9, {ctx}",
        "add  x9, x9, :lo12:{ctx}",
        "str  w0, [x9, #112]",
        "ldp  x19, x20, [x9, #0]",
        "ldp  x21, x22, [x9, #16]",
        "ldp  x23, x24, [x9, #32]",
        "ldp  x25, x26, [x9, #48]",
        "ldp  x27, x28, [x9, #64]",
        "ldp  x29, x30, [x9, #80]",
        "ldp  x10, x11, [x9, #96]",
        "mov  sp, x10",
        "msr  sp_el0, xzr",
        "ldr  w0, [x9, #112]",
        "msr  daif, x11",
        "isb",
        "ret",
        ctx = sym EL1_RETURN_CTX,
    );
}

pub fn exit_to_el1(code: i32) -> ! {
    assert!(
        EL0_ACTIVE.swap(false, Ordering::AcqRel),
        "exit_to_el1 called with no active EL0 task"
    );
    // SAFETY: `EL0_ACTIVE` was true, proving `enter_el0` saved a live EL1 continuation
    // (`x19`–`x30`, `sp`, and `daif`) into `EL1_RETURN_CTX` on the current kernel stack.
    unsafe { restore_el1(code) }
}

pub fn exec_elf(image: &[u8]) -> Result<i32, &'static str> {
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

    let user_end = USER_MEM_BASE + USER_MEM_SIZE;
    let entry = read_u64(image, 24) as usize;
    if !(USER_MEM_BASE..user_end).contains(&entry) {
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
    for index in 0..phnum {
        let ph = &image[phoff + index * phentsize..][..ELF_PHDR_SIZE];
        if read_u32(ph, 0) != PT_LOAD {
            continue;
        }
        let p_offset = read_u64(ph, 8) as usize;
        let p_vaddr = read_u64(ph, 16) as usize;
        let p_filesz = read_u64(ph, 32) as usize;
        let p_memsz = read_u64(ph, 40) as usize;

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
        if p_vaddr < USER_MEM_BASE || seg_end > user_end || (p_memsz == 0 && p_vaddr >= user_end) {
            return Err("elf: segment outside user memory");
        }
        load_segments += 1;
    }
    if load_segments == 0 {
        return Err("elf: no PT_LOAD segments");
    }

    if EL0_ACTIVE.swap(true, Ordering::AcqRel) {
        return Err("elf: user task already active");
    }

    // SAFETY: `USER_MEM_BASE..user_end` is identity-mapped RW for EL1 and owned exclusively
    // while `EL0_ACTIVE` is set; every `PT_LOAD` slice and destination range was bounds-checked.
    unsafe {
        core::ptr::write_bytes(USER_MEM_BASE as *mut u8, 0, USER_MEM_SIZE);
        for index in 0..phnum {
            let ph = &image[phoff + index * phentsize..][..ELF_PHDR_SIZE];
            if read_u32(ph, 0) != PT_LOAD {
                continue;
            }
            let p_offset = read_u64(ph, 8) as usize;
            let p_vaddr = read_u64(ph, 16) as usize;
            let p_filesz = read_u64(ph, 32) as usize;
            if p_filesz > 0 {
                core::ptr::copy_nonoverlapping(
                    image.as_ptr().add(p_offset),
                    p_vaddr as *mut u8,
                    p_filesz,
                );
                let mut line = p_vaddr & !63;
                while line < p_vaddr + p_filesz {
                    asm!("dc cvau, {}", in(reg) line, options(nostack, preserves_flags));
                    line += 64;
                }
            }
        }
        asm!(
            "dsb ish",
            "ic iallu",
            "dsb ish",
            "isb",
            options(nostack, preserves_flags),
        );
    }

    // SAFETY: `entry` and `user_end` lie within the isolated EL0 2 MiB region, I-cache is
    // coherent with loaded code, and `EL0_ACTIVE` grants exclusive access to `EL1_RETURN_CTX`.
    let status = unsafe { enter_el0(entry, user_end) };
    Ok(status)
}
