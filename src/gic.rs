use core::ptr::{read_volatile, write_volatile};

const GICD_BASE: usize = 0x0800_0000;
const GICC_BASE: usize = 0x0801_0000;

const GICD_CTLR: usize = 0x000;
const GICD_ISENABLER0: usize = 0x100;
const GICD_ISENABLER1: usize = 0x104;
const GICD_IPRIORITYR: usize = 0x400;
const GICD_ITARGETSR: usize = 0x800;

const GICC_CTLR: usize = 0x000;
const GICC_PMR: usize = 0x004;
const GICC_IAR: usize = 0x00C;
const GICC_EOIR: usize = 0x010;

pub const TIMER_IRQ_ID: u32 = 30; // PPI 14
pub const UART0_IRQ_ID: u32 = 33; // SPI 1

#[inline]
fn write_gicd(offset: usize, value: u32) {
    // SAFETY: `GICD_BASE + offset` addresses a valid 32-bit GICv2 Distributor MMIO register on QEMU virt.
    unsafe { write_volatile((GICD_BASE + offset) as *mut u32, value) }
}

#[inline]
fn write_gicd_byte(offset: usize, value: u8) {
    // SAFETY: `GICD_BASE + offset` addresses a byte-accessible GICv2 Distributor MMIO register on QEMU virt.
    unsafe { write_volatile((GICD_BASE + offset) as *mut u8, value) }
}

#[inline]
fn read_gicc(offset: usize) -> u32 {
    // SAFETY: `GICC_BASE + offset` addresses a valid 32-bit GICv2 CPU interface MMIO register on QEMU virt.
    unsafe { read_volatile((GICC_BASE + offset) as *const u32) }
}

#[inline]
fn write_gicc(offset: usize, value: u32) {
    // SAFETY: `GICC_BASE + offset` addresses a valid 32-bit GICv2 CPU interface MMIO register on QEMU virt.
    unsafe { write_volatile((GICC_BASE + offset) as *mut u32, value) }
}

pub fn init() {
    write_gicd(GICD_CTLR, 0);

    write_gicd(GICD_ISENABLER0, 1 << TIMER_IRQ_ID);
    write_gicd_byte(GICD_IPRIORITYR + (TIMER_IRQ_ID as usize), 0x00);

    write_gicd(GICD_ISENABLER1, 1 << (UART0_IRQ_ID - 32));
    write_gicd_byte(GICD_IPRIORITYR + (UART0_IRQ_ID as usize), 0x00);
    write_gicd_byte(GICD_ITARGETSR + (UART0_IRQ_ID as usize), 0x01); // CPU 0

    write_gicd(GICD_CTLR, 1);

    write_gicc(GICC_PMR, 0xFF);
    write_gicc(GICC_CTLR, 1);
}

#[inline]
pub fn acknowledge() -> u32 {
    read_gicc(GICC_IAR)
}

#[inline]
pub fn end_of_interrupt(iar: u32) {
    write_gicc(GICC_EOIR, iar);
}
