use core::arch::asm;

const ENTRIES_PER_TABLE: usize = 512;
const L1_BLOCK_SIZE: u64 = 1 << 30;

const DESC_BLOCK: u64 = 0b01;
const ATTR_INDX_NORMAL: u64 = 0 << 2;
const ATTR_INDX_DEVICE: u64 = 1 << 2;
const SH_INNER: u64 = 0b11 << 8;
const AF: u64 = 1 << 10;
const PXN: u64 = 1 << 53;
const UXN: u64 = 1 << 54;

const MAIR_VALUE: u64 = 0x00FF; // Attr0=0xFF (Normal WB-RWA), Attr1=0x00 (Device-nGnRnE)

const TCR_T0SZ: u64 = 25;
const TCR_IRGN0_WBWA: u64 = 0b01 << 8;
const TCR_ORGN0_WBWA: u64 = 0b01 << 10;
const TCR_SH0_INNER: u64 = 0b11 << 12;
const TCR_TG0_4K: u64 = 0b00 << 14;
const TCR_EPD1: u64 = 1 << 23;
const TCR_TG1_4K: u64 = 0b10 << 30; // non-reserved TG1 encoding
const TCR_IPS_40BIT: u64 = 0b010 << 32;

const TCR_VALUE: u64 = TCR_T0SZ
    | TCR_IRGN0_WBWA
    | TCR_ORGN0_WBWA
    | TCR_SH0_INNER
    | TCR_TG0_4K
    | TCR_EPD1
    | TCR_TG1_4K
    | TCR_IPS_40BIT;

const SCTLR_M: u64 = 1 << 0;
const SCTLR_C: u64 = 1 << 2;
const SCTLR_I: u64 = 1 << 12;

#[repr(C, align(4096))]
pub struct PageTable {
    entries: [u64; ENTRIES_PER_TABLE],
}

static mut L1_TABLE: PageTable = PageTable {
    entries: [0; ENTRIES_PER_TABLE],
};

pub fn init() {
    // SAFETY: Called once on the single boot core with the MMU disabled; no other references
    // to `L1_TABLE` exist while it is populated.
    let table = unsafe { &mut *(&raw mut L1_TABLE) };

    table.entries[0] = DESC_BLOCK | ATTR_INDX_DEVICE | AF | PXN | UXN;
    table.entries[1] = L1_BLOCK_SIZE | DESC_BLOCK | ATTR_INDX_NORMAL | SH_INNER | AF | UXN;

    let ttbr0 = &raw const L1_TABLE as u64;
    let sctlr_flags = SCTLR_M | SCTLR_C | SCTLR_I;

    // SAFETY: `L1_TABLE` identity-maps all memory the kernel touches (code, data, stack, heap,
    // and MMIO), so execution continues seamlessly once SCTLR_EL1.M is set.
    unsafe {
        asm!(
            "msr mair_el1, {mair}",
            "msr tcr_el1, {tcr}",
            "msr ttbr0_el1, {ttbr0}",
            "dsb ish",
            "tlbi vmalle1",
            "dsb ish",
            "isb",
            "mrs {tmp}, sctlr_el1",
            "orr {tmp}, {tmp}, {flags}",
            "msr sctlr_el1, {tmp}",
            "isb",
            mair = in(reg) MAIR_VALUE,
            tcr = in(reg) TCR_VALUE,
            ttbr0 = in(reg) ttbr0,
            flags = in(reg) sctlr_flags,
            tmp = out(reg) _,
            options(nostack, preserves_flags),
        );
    }
}

pub fn is_enabled() -> bool {
    let sctlr: u64;
    // SAFETY: Reading `SCTLR_EL1` is valid at EL1 on AArch64 with no side effects.
    unsafe {
        asm!("mrs {}, sctlr_el1", out(reg) sctlr, options(nomem, nostack, preserves_flags));
    }
    sctlr & SCTLR_M != 0
}
