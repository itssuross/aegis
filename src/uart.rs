use core::fmt::{self, Write};
use core::ptr::{read_volatile, write_volatile};

const UART0_BASE: usize = 0x0900_0000;

const DR: usize = 0x00;
const FR: usize = 0x18;
const IBRD: usize = 0x24;
const FBRD: usize = 0x28;
const LCR_H: usize = 0x2C;
const CR: usize = 0x30;
const ICR: usize = 0x44;

const FR_TXFF: u32 = 1 << 5;

#[inline]
fn read_reg(offset: usize) -> u32 {
    // SAFETY: `UART0_BASE + offset` addresses a valid 32-bit PL011 MMIO register on QEMU virt.
    unsafe { read_volatile((UART0_BASE + offset) as *const u32) }
}

#[inline]
fn write_reg(offset: usize, value: u32) {
    // SAFETY: `UART0_BASE + offset` addresses a valid 32-bit PL011 MMIO register on QEMU virt.
    unsafe { write_volatile((UART0_BASE + offset) as *mut u32, value) }
}

pub fn init() {
    // Disable UART while configuring control and line parameters.
    write_reg(CR, 0);

    // Clear all pending interrupts.
    write_reg(ICR, 0x7FF);

    // Configure 115200 baud for QEMU virt's 24 MHz UARTCLK:
    // divisor = 24_000_000 / (16 * 115_200) = 13.0208... -> IBRD = 13, FBRD = 1.
    write_reg(IBRD, 13);
    write_reg(FBRD, 1);

    // Enable FIFOs (bit 4) and configure 8-bit word length (bits 6:5 = 0b11).
    write_reg(LCR_H, (1 << 4) | (3 << 5));

    // Enable UART (bit 0), TX (bit 8), and RX (bit 9).
    write_reg(CR, (1 << 0) | (1 << 8) | (1 << 9));
}

fn put_raw_byte(byte: u8) {
    while (read_reg(FR) & FR_TXFF) != 0 {
        core::hint::spin_loop();
    }
    write_reg(DR, u32::from(byte));
}

pub fn write_byte(byte: u8) {
    if byte == b'\n' {
        put_raw_byte(b'\r');
    }
    put_raw_byte(byte);
}

pub struct UartWriter;

impl Write for UartWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for byte in s.bytes() {
            write_byte(byte);
        }
        Ok(())
    }
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments<'_>) {
    let mut writer = UartWriter;
    let _ = writer.write_fmt(args);
}

#[macro_export]
macro_rules! kprint {
    ($($arg:tt)*) => {
        $crate::uart::_print(core::format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! kprintln {
    () => {
        $crate::kprint!("\n")
    };
    ($($arg:tt)*) => {
        $crate::kprint!("{}\n", core::format_args!($($arg)*))
    };
}
