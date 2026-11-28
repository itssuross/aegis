#![allow(dead_code)]

pub const PAGE_SIZE: usize = 4096;
pub const PAGE_SHIFT: usize = 12;

pub const RAM_BASE: usize = 0x4000_0000;
pub const RAM_SIZE: usize = 128 * 1024 * 1024;
pub const RAM_END: usize = RAM_BASE + RAM_SIZE;

pub const PHYS_OFFSET: usize = 0xFFFF_FF80_0000_0000;
pub const KERNEL_VMA_BASE: usize = PHYS_OFFSET + RAM_BASE;

pub const fn phys_to_virt(phys: usize) -> usize {
    phys + PHYS_OFFSET
}

pub const fn virt_to_phys(virt: usize) -> usize {
    virt - PHYS_OFFSET
}

pub const GICD_BASE: usize = 0x0800_0000;
pub const GICC_BASE: usize = 0x0801_0000;
pub const UART0_BASE: usize = 0x0900_0000;

pub const VIRTIO_MMIO_BASE: usize = 0x0a00_0000;
pub const VIRTIO_MMIO_STRIDE: usize = 0x200;
pub const VIRTIO_MMIO_SLOTS: usize = 32;
