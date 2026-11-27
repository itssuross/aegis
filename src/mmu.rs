use core::arch::asm;

const ENTRIES_PER_TABLE: usize = 512;
const TABLE_INDEX_MASK: usize = ENTRIES_PER_TABLE - 1;
const VA_SPACE_SIZE: usize = 1 << 39; // 39-bit VA (T0SZ = 25)
const L1_BLOCK_SIZE: u64 = 1 << 30;
const L2_BLOCK_SIZE: usize = 1 << 21;

const KERNEL_MMIO_START: usize = 0x0800_0000;
const KERNEL_MMIO_END: usize = 0x0B00_0000;
const KERNEL_RAM_START: usize = crate::platform::RAM_BASE;
const KERNEL_RAM_END: usize = crate::platform::RAM_END;

const DESC_TYPE_MASK: u64 = 0b11;
const DESC_BLOCK: u64 = 0b01;
const DESC_TABLE: u64 = 0b11;
const DESC_PAGE: u64 = 0b11; // L3 4 KiB page descriptor
const ADDR_MASK: u64 = 0x0000_FFFF_FFFF_F000; // OA[47:12]
const ATTR_INDX_NORMAL: u64 = 0 << 2;
const ATTR_INDX_DEVICE: u64 = 1 << 2;
const AP_EL1_RW: u64 = 0b00 << 6;
const AP_EL0_RW: u64 = 0b01 << 6;
const AP_EL1_RO: u64 = 0b10 << 6;
const AP_EL0_RO: u64 = 0b11 << 6;
const SH_NON: u64 = 0b00 << 8;
const SH_INNER: u64 = 0b11 << 8;
const AF: u64 = 1 << 10;
const NG: u64 = 1 << 11; // non-global, ASID-tagged TLB entry
pub const PXN: u64 = 1 << 53;
pub const UXN: u64 = 1 << 54;

const MAIR_VALUE: u64 = 0x00FF; // Attr0=0xFF (Normal WB-RWA), Attr1=0x00 (Device-nGnRnE)

const TCR_T0SZ: u64 = 25;
const TCR_IRGN0_WBWA: u64 = 0b01 << 8;
const TCR_ORGN0_WBWA: u64 = 0b01 << 10;
const TCR_SH0_INNER: u64 = 0b11 << 12;
const TCR_TG0_4K: u64 = 0b00 << 14;
const TCR_EPD1: u64 = 1 << 23;
const TCR_TG1_4K: u64 = 0b10 << 30; // non-reserved TG1 encoding
const TCR_IPS_40BIT: u64 = 0b010 << 32;
const TCR_AS: u64 = 1 << 36; // 16-bit ASID in TTBR0_EL1[63:48] (A1 = 0)

const TCR_VALUE: u64 = TCR_T0SZ
    | TCR_IRGN0_WBWA
    | TCR_ORGN0_WBWA
    | TCR_SH0_INNER
    | TCR_TG0_4K
    | TCR_EPD1
    | TCR_TG1_4K
    | TCR_IPS_40BIT
    | TCR_AS;

const SCTLR_M: u64 = 1 << 0;
const SCTLR_C: u64 = 1 << 2;
const SCTLR_I: u64 = 1 << 12;

#[repr(C, align(4096))]
struct RawPageTable {
    entries: [u64; ENTRIES_PER_TABLE],
}

static mut L1_TABLE: RawPageTable = RawPageTable {
    entries: [0; ENTRIES_PER_TABLE],
};

#[derive(Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum PageFlags {
    KernelRo,
    KernelRw,
    KernelRx,
    UserRo,
    UserRw,
    UserRx,
    Device,
}

impl PageFlags {
    const fn bits(self) -> u64 {
        match self {
            Self::KernelRo => {
                DESC_PAGE | ATTR_INDX_NORMAL | AP_EL1_RO | SH_INNER | AF | PXN | UXN
            }
            Self::KernelRw => {
                DESC_PAGE | ATTR_INDX_NORMAL | AP_EL1_RW | SH_INNER | AF | PXN | UXN
            }
            Self::KernelRx => DESC_PAGE | ATTR_INDX_NORMAL | AP_EL1_RO | SH_INNER | AF | UXN,
            Self::UserRo => {
                DESC_PAGE | ATTR_INDX_NORMAL | AP_EL0_RO | SH_INNER | AF | NG | PXN | UXN
            }
            Self::UserRw => {
                DESC_PAGE | ATTR_INDX_NORMAL | AP_EL0_RW | SH_INNER | AF | NG | PXN | UXN
            }
            Self::UserRx => DESC_PAGE | ATTR_INDX_NORMAL | AP_EL0_RO | SH_INNER | AF | NG | PXN,
            Self::Device => DESC_PAGE | ATTR_INDX_DEVICE | AP_EL1_RW | SH_NON | AF | PXN | UXN,
        }
    }
}

#[inline]
const fn table_indices(virt: usize) -> (usize, usize, usize) {
    let l1 = (virt >> 30) & TABLE_INDEX_MASK; // VA[38:30]
    let l2 = (virt >> 21) & TABLE_INDEX_MASK; // VA[29:21]
    let l3 = (virt >> 12) & TABLE_INDEX_MASK; // VA[20:12]
    (l1, l2, l3)
}

// SAFETY: `table_phys` must be a valid 4 KiB-aligned table frame in identity-mapped RAM
// and `index < ENTRIES_PER_TABLE`.
#[inline]
unsafe fn read_entry(table_phys: usize, index: usize) -> u64 {
    // SAFETY: Caller guarantees `table_phys` points to a live 512-entry u64 table and `index < 512`.
    unsafe { (table_phys as *const u64).add(index).read() }
}

// SAFETY: `table_phys` must be a valid, exclusively-accessed 4 KiB-aligned table frame
// in identity-mapped RAM and `index < ENTRIES_PER_TABLE`.
#[inline]
unsafe fn write_entry(table_phys: usize, index: usize, value: u64) {
    // SAFETY: Caller guarantees exclusive access to the live 512-entry u64 table and `index < 512`.
    unsafe { (table_phys as *mut u64).add(index).write(value) }
}

pub struct PageTable {
    root_phys: usize,
}

impl PageTable {
    pub fn new() -> Option<Self> {
        let root_phys = crate::pmm::alloc_frame()?;
        Some(Self { root_phys })
    }

    pub fn root_phys(&self) -> usize {
        self.root_phys
    }

    // SAFETY: `table_phys` must be an owned 4 KiB-aligned L1 or L2 table frame in identity-mapped RAM.
    unsafe fn ensure_next_table(table_phys: usize, index: usize) -> Result<usize, &'static str> {
        // SAFETY: Caller owns `table_phys` exclusively and `index < ENTRIES_PER_TABLE`.
        let entry = unsafe { read_entry(table_phys, index) };
        match entry & DESC_TYPE_MASK {
            0 => {
                let next_phys =
                    crate::pmm::alloc_frame().ok_or("mmu: out of physical memory")?;
                // SAFETY: `next_phys` is a freshly zeroed 4 KiB table frame and `table_phys` is owned exclusively.
                unsafe {
                    write_entry(table_phys, index, (next_phys as u64) | DESC_TABLE);
                }
                Ok(next_phys)
            }
            DESC_TABLE => Ok((entry & ADDR_MASK) as usize),
            _ => Err("mmu: conflicting block descriptor"),
        }
    }

    fn map_l2_blocks(
        &mut self,
        start: usize,
        end: usize,
        attrs: u64,
    ) -> Result<(), &'static str> {
        let mut addr = start;
        while addr < end {
            let (l1_idx, l2_idx, _) = table_indices(addr);
            // SAFETY: `&mut self` grants exclusive access to `root_phys` and its L2 tables.
            let l2_phys = unsafe { Self::ensure_next_table(self.root_phys, l1_idx)? };
            // SAFETY: `l2_phys` is an owned L2 table frame and `l2_idx < ENTRIES_PER_TABLE`.
            let existing = unsafe { read_entry(l2_phys, l2_idx) };
            if (existing & DESC_TYPE_MASK) != 0 {
                return Err("mmu: L2 entry already mapped");
            }
            // SAFETY: `l2_phys` is owned exclusively by `&mut self` and `l2_idx < ENTRIES_PER_TABLE`.
            unsafe {
                write_entry(l2_phys, l2_idx, (addr as u64) | DESC_BLOCK | attrs);
            }
            addr += L2_BLOCK_SIZE;
        }
        Ok(())
    }

    pub fn map_kernel_regions(&mut self) -> Result<(), &'static str> {
        self.map_l2_blocks(
            KERNEL_MMIO_START,
            KERNEL_MMIO_END,
            ATTR_INDX_DEVICE | AP_EL1_RW | SH_NON | AF | PXN | UXN,
        )?;
        self.map_l2_blocks(
            KERNEL_RAM_START,
            KERNEL_RAM_END,
            ATTR_INDX_NORMAL | AP_EL1_RW | SH_INNER | AF | UXN,
        )?;
        Ok(())
    }

    pub fn map_4k(
        &mut self,
        virt: usize,
        phys: usize,
        flags: PageFlags,
    ) -> Result<(), &'static str> {
        if (virt & (crate::platform::PAGE_SIZE - 1)) != 0
            || (phys & (crate::platform::PAGE_SIZE - 1)) != 0
        {
            return Err("mmu: unaligned address");
        }
        if virt >= VA_SPACE_SIZE {
            return Err("mmu: virtual address out of range");
        }
        if (phys as u64) & !ADDR_MASK != 0 {
            return Err("mmu: physical address out of range");
        }

        let (l1_idx, l2_idx, l3_idx) = table_indices(virt);
        // SAFETY: `&mut self` grants exclusive access to the page table hierarchy rooted at `root_phys`.
        let l2_phys = unsafe { Self::ensure_next_table(self.root_phys, l1_idx)? };
        // SAFETY: `l2_phys` is an owned L2 table frame belonging to this `PageTable`.
        let l3_phys = unsafe { Self::ensure_next_table(l2_phys, l2_idx)? };

        // SAFETY: `l3_phys` is an owned L3 table frame and `l3_idx < ENTRIES_PER_TABLE`.
        let existing = unsafe { read_entry(l3_phys, l3_idx) };
        if (existing & DESC_TYPE_MASK) != 0 {
            return Err("mmu: page already mapped");
        }

        // SAFETY: `l3_phys` is owned exclusively by `&mut self` and `l3_idx < ENTRIES_PER_TABLE`.
        unsafe {
            write_entry(l3_phys, l3_idx, (phys as u64) | flags.bits());
        }
        Ok(())
    }

    pub fn unmap_4k(&mut self, virt: usize) -> Option<usize> {
        if (virt & (crate::platform::PAGE_SIZE - 1)) != 0 || virt >= VA_SPACE_SIZE {
            return None;
        }

        let (l1_idx, l2_idx, l3_idx) = table_indices(virt);
        // SAFETY: `&mut self` owns `root_phys` and all descendant tables exclusively.
        unsafe {
            let l1_entry = read_entry(self.root_phys, l1_idx);
            if (l1_entry & DESC_TYPE_MASK) != DESC_TABLE {
                return None;
            }
            let l2_phys = (l1_entry & ADDR_MASK) as usize;

            let l2_entry = read_entry(l2_phys, l2_idx);
            if (l2_entry & DESC_TYPE_MASK) != DESC_TABLE {
                return None;
            }
            let l3_phys = (l2_entry & ADDR_MASK) as usize;

            let l3_entry = read_entry(l3_phys, l3_idx);
            if (l3_entry & DESC_TYPE_MASK) != DESC_PAGE {
                return None;
            }

            write_entry(l3_phys, l3_idx, 0);
            Some((l3_entry & ADDR_MASK) as usize)
        }
    }

    pub fn translate(&self, virt: usize) -> Option<(usize, u64)> {
        if virt >= VA_SPACE_SIZE {
            return None;
        }

        let (l1_idx, l2_idx, l3_idx) = table_indices(virt);
        // SAFETY: `&self` guarantees `root_phys` and all valid descendant table frames remain live.
        unsafe {
            let l1_entry = read_entry(self.root_phys, l1_idx);
            if (l1_entry & DESC_TYPE_MASK) != DESC_TABLE {
                return None;
            }
            let l2_phys = (l1_entry & ADDR_MASK) as usize;

            let l2_entry = read_entry(l2_phys, l2_idx);
            if (l2_entry & DESC_TYPE_MASK) != DESC_TABLE {
                return None;
            }
            let l3_phys = (l2_entry & ADDR_MASK) as usize;

            let l3_entry = read_entry(l3_phys, l3_idx);
            if (l3_entry & DESC_TYPE_MASK) != DESC_PAGE {
                return None;
            }

            let phys =
                ((l3_entry & ADDR_MASK) as usize) | (virt & (crate::platform::PAGE_SIZE - 1));
            Some((phys, l3_entry))
        }
    }
}

impl Drop for PageTable {
    fn drop(&mut self) {
        for l1_idx in 0..ENTRIES_PER_TABLE {
            // SAFETY: `self` owns `root_phys` exclusively during `Drop`.
            let l1_entry = unsafe { read_entry(self.root_phys, l1_idx) };
            if (l1_entry & DESC_TYPE_MASK) != DESC_TABLE {
                continue;
            }
            let l2_phys = (l1_entry & ADDR_MASK) as usize;
            for l2_idx in 0..ENTRIES_PER_TABLE {
                // SAFETY: `l2_phys` is an owned L2 table frame allocated by this `PageTable`.
                let l2_entry = unsafe { read_entry(l2_phys, l2_idx) };
                if (l2_entry & DESC_TYPE_MASK) != DESC_TABLE {
                    continue;
                }
                let l3_phys = (l2_entry & ADDR_MASK) as usize;
                crate::pmm::free_frame(l3_phys);
            }
            crate::pmm::free_frame(l2_phys);
        }
        crate::pmm::free_frame(self.root_phys);
    }
}

pub fn kernel_ttbr0() -> u64 {
    &raw const L1_TABLE as usize as u64
}

// SAFETY: Caller guarantees `ttbr0` holds a valid 4 KiB-aligned L1 page table that identity-maps
// active kernel text, stacks, data, and MMIO, with a valid 16-bit ASID in bits [63:48].
pub unsafe fn switch_ttbr0(ttbr0: u64) {
    // SAFETY: Writing `TTBR0_EL1` followed by `isb` activates the target translation table and ASID.
    unsafe {
        asm!(
            "msr ttbr0_el1, {ttbr0}",
            "isb",
            ttbr0 = in(reg) ttbr0,
            options(nostack, preserves_flags),
        );
    }
}

pub fn flush_asid(asid: u16) {
    let operand = (asid as u64) << 48; // ASID[63:48]
    // SAFETY: `tlbi aside1is` invalidates non-global EL1/EL0 TLB entries for `asid` across all cores.
    unsafe {
        asm!(
            "dsb ishst",
            "tlbi aside1is, {asid}",
            "dsb ish",
            "isb",
            asid = in(reg) operand,
            options(nostack, preserves_flags),
        );
    }
}

unsafe fn enable_mmu_on_current_cpu() {
    let ttbr0 = kernel_ttbr0();
    let sctlr_flags = SCTLR_M | SCTLR_C | SCTLR_I;

    // SAFETY: Caller guarantees `L1_TABLE` is initialized and identity-maps all memory the kernel
    // touches (code, data, stack, heap, and MMIO), so execution continues once `SCTLR_EL1.M` is set.
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
    // to `L1_TABLE` exist while it is populated.
    let l1_table = unsafe { &mut *(&raw mut L1_TABLE) };

    l1_table.entries[0] = DESC_BLOCK | ATTR_INDX_DEVICE | AP_EL1_RW | SH_NON | AF | PXN | UXN;
    l1_table.entries[1] =
        L1_BLOCK_SIZE | DESC_BLOCK | ATTR_INDX_NORMAL | AP_EL1_RW | SH_INNER | AF | UXN;

    // SAFETY: `L1_TABLE` was just populated with EL1-only identity mappings for kernel RAM and MMIO.
    unsafe {
        enable_mmu_on_current_cpu();
    }
}

// SAFETY: Core 0 must have completed `mmu::init()` before any secondary core calls this, and the
// caller must execute at EL1 with the MMU disabled.
pub unsafe fn init_secondary() {
    // SAFETY: Caller guarantees `L1_TABLE` is already initialized and immutable.
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
