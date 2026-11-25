use core::cell::UnsafeCell;
use core::fmt::{self, Write};
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicUsize, Ordering};

const UART0_BASE: usize = 0x0900_0000;

const DR: usize = 0x00;
const FR: usize = 0x18;
const IBRD: usize = 0x24;
const FBRD: usize = 0x28;
const LCR_H: usize = 0x2C;
const CR: usize = 0x30;
const IMSC: usize = 0x38;
const ICR: usize = 0x44;

// FR flag bits: RXFE (receive FIFO empty), TXFF (transmit FIFO full).
const FR_RXFE: u32 = 1 << 4;
const FR_TXFF: u32 = 1 << 5;
// IMSC / ICR bits: RXIM (receive interrupt), RTIM (receive timeout interrupt).
const IMSC_RXIM: u32 = 1 << 4;
const IMSC_RTIM: u32 = 1 << 6;

const RX_BUF_SIZE: usize = 256;
const RX_BUF_MASK: usize = RX_BUF_SIZE - 1;

struct RxRingBuffer {
    buf: UnsafeCell<[u8; RX_BUF_SIZE]>,
    head: AtomicUsize,
    tail: AtomicUsize,
}

// SAFETY: Single producer (`handle_irq`, IRQs masked in EL1 IRQ context) writes only to
// `buf[head & mask]` before Release-storing `head`; single consumer (`try_read_byte`) reads
// only `buf[tail & mask]` after Acquire-loading `head` before Release-storing `tail`.
unsafe impl Sync for RxRingBuffer {}

static RX_RING: RxRingBuffer = RxRingBuffer {
    buf: UnsafeCell::new([0; RX_BUF_SIZE]),
    head: AtomicUsize::new(0),
    tail: AtomicUsize::new(0),
};

impl RxRingBuffer {
    fn push(&self, byte: u8) {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        if head.wrapping_sub(tail) == RX_BUF_SIZE {
            return;
        }
        // SAFETY: Slot `head & RX_BUF_MASK` is exclusively owned by the producer until `head` advances.
        unsafe {
            self.buf.get().cast::<u8>().add(head & RX_BUF_MASK).write(byte);
        }
        self.head.store(head.wrapping_add(1), Ordering::Release);
    }

    fn pop(&self) -> Option<u8> {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        if tail == head {
            return None;
        }
        // SAFETY: Slot `tail & RX_BUF_MASK` was written before `head`'s Release store and is
        // exclusively owned by the consumer until `tail` advances.
        let byte = unsafe { self.buf.get().cast::<u8>().add(tail & RX_BUF_MASK).read() };
        self.tail.store(tail.wrapping_add(1), Ordering::Release);
        Some(byte)
    }
}

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

    // Unmask RX (bit 4) and RX timeout (bit 6) interrupts and clear pending flags.
    write_reg(IMSC, IMSC_RXIM | IMSC_RTIM);
    write_reg(ICR, 0x7FF);

    // Enable UART (bit 0), TX (bit 8), and RX (bit 9).
    write_reg(CR, (1 << 0) | (1 << 8) | (1 << 9));
}

pub fn handle_irq() {
    while (read_reg(FR) & FR_RXFE) == 0 {
        let byte = (read_reg(DR) & 0xFF) as u8;
        RX_RING.push(byte);
    }
    write_reg(ICR, IMSC_RXIM | IMSC_RTIM);
}

pub fn try_read_byte() -> Option<u8> {
    RX_RING.pop()
}

#[allow(dead_code, reason = "the idle loops wire up this API in a separate commit")]
pub fn has_pending_input() -> bool {
    RX_RING.head.load(Ordering::Acquire) != RX_RING.tail.load(Ordering::Acquire)
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
