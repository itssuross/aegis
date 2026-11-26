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

const FR_RXFE: u32 = 1 << 4;
const FR_TXFF: u32 = 1 << 5;
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

static RX_WAITQ: crate::scheduler::WaitQueue = crate::scheduler::WaitQueue::new();

impl RxRingBuffer {
    fn push(&self, byte: u8) -> bool {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        if head.wrapping_sub(tail) == RX_BUF_SIZE {
            return false;
        }
        // SAFETY: Slot `head & RX_BUF_MASK` is exclusively owned by the producer until `head` advances.
        unsafe {
            self.buf
                .get()
                .cast::<u8>()
                .add(head & RX_BUF_MASK)
                .write(byte);
        }
        self.head.store(head.wrapping_add(1), Ordering::Release);
        true
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
    write_reg(CR, 0);
    write_reg(ICR, 0x7FF);

    write_reg(IBRD, 13); // 115200 baud @ 24 MHz UARTCLK
    write_reg(FBRD, 1);

    write_reg(LCR_H, (1 << 4) | (3 << 5)); // FEN | WLEN=8

    write_reg(IMSC, IMSC_RXIM | IMSC_RTIM);
    write_reg(ICR, 0x7FF);

    write_reg(CR, (1 << 0) | (1 << 8) | (1 << 9)); // UARTEN | TXE | RXE
}

pub fn handle_irq() {
    let mut received = false;
    while (read_reg(FR) & FR_RXFE) == 0 {
        let byte = (read_reg(DR) & 0xFF) as u8;
        received |= RX_RING.push(byte);
    }
    write_reg(ICR, IMSC_RXIM | IMSC_RTIM);
    if received {
        RX_WAITQ.wake_one();
    }
}

pub fn try_read_byte() -> Option<u8> {
    RX_RING.pop()
}

pub fn has_pending_input() -> bool {
    RX_RING.head.load(Ordering::Acquire) != RX_RING.tail.load(Ordering::Acquire)
}

pub fn wait_for_input() {
    RX_WAITQ.wait_until(has_pending_input);
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

static PRINT_LOCK: crate::sync::IrqSpinLock<()> = crate::sync::IrqSpinLock::new(());

#[doc(hidden)]
pub fn _print(args: fmt::Arguments<'_>) {
    let _guard = PRINT_LOCK.lock();
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
