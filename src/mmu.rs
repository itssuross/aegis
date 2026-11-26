use core::arch::asm;

pub const USER_MEM_BASE: usize = 0x4200_0000;
pub const USER_MEM_SIZE: usize = 0x0020_0000;

const ENTRIES_PER_TABLE: usize = 512;
const L1_BLOCK_SIZE: u64 = 1 << 30;
const L2_BLOCK_SIZE: u64 = 1 << 21;
const USER_L2_INDEX: usize = (USER_MEM_BASE - L1_BLOCK_SIZE as usize) / USER_MEM_SIZE;

const _: () = assert!(USER_L2_INDEX == 16);
const _: () = assert!(USER_MEM_SIZE as u64 == L2_BLOCK_SIZE);

const DESC_BLOCK: u64 = 0b01;
const DESC_TABLE: u64 = 0b11;
const ATTR_INDX_NORMAL: u64 = 0 << 2;
const ATTR_INDX_DEVICE: u64 = 1 << 2;
const AP_EL1_RW: u64 = 0b00 << 6;
const AP_EL0_RW: u64 = 0b01 << 6;
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

static mut L2_RAM_TABLE: PageTable = PageTable {
    entries: [0; ENTRIES_PER_TABLE],
};

unsafe fn enable_mmu_on_current_cpu() {
    let ttbr0 = &raw const L1_TABLE as u64;
    let sctlr_flags = SCTLR_M | SCTLR_C | SCTLR_I;

    // SAFETY: Caller guarantees `L1_TABLE` and `L2_RAM_TABLE` are initialized and identity-map all
    // memory the kernel touches (code, data, stack, heap, and MMIO), so execution continues
    // seamlessly once SCTLR_EL1.M is set.
    unsafe {
        asm!(
            "msr mair_el1, {mair}",
            "msr tcr_el1, {tcr}",
            "msr ttbr0_el1, {ttbr0}",
            "dsb ish",
            "isb",
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

pub fn init() {
    // SAFETY: Called once on the single boot core with the MMU disabled; no other references
    // to `L1_TABLE` or `L2_RAM_TABLE` exist while they are populated.
    let (l1_table, l2_table) =
        unsafe { (&mut *(&raw mut L1_TABLE), &mut *(&raw mut L2_RAM_TABLE)) };

    for (index, entry) in l2_table.entries.iter_mut().enumerate() {
        let block_pa = L1_BLOCK_SIZE + (index as u64) * L2_BLOCK_SIZE;
        *entry = if index == USER_L2_INDEX {
            block_pa | DESC_BLOCK | ATTR_INDX_NORMAL | AP_EL0_RW | SH_INNER | AF | PXN
        } else {
            block_pa | DESC_BLOCK | ATTR_INDX_NORMAL | AP_EL1_RW | SH_INNER | AF | UXN
        };
    }

    l1_table.entries[0] = DESC_BLOCK | ATTR_INDX_DEVICE | AF | PXN | UXN;
    l1_table.entries[1] = (&raw const L2_RAM_TABLE as u64) | DESC_TABLE;

    // SAFETY: `L1_TABLE` and `L2_RAM_TABLE` were just populated with identity mappings for all
    // kernel memory and MMIO.
    unsafe {
        enable_mmu_on_current_cpu();
    }
}

// SAFETY: Core 0 must have completed `mmu::init()` before any secondary core calls this, and the
// caller must execute at EL1 with the MMU disabled.
pub unsafe fn init_secondary() {
    // SAFETY: Caller guarantees `L1_TABLE` and `L2_RAM_TABLE` are already initialized and immutable.
    unsafe {
        enable_mmu_on_current_cpu();
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
