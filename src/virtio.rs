use alloc::boxed::Box;
use core::arch::asm;
use core::ptr::{read_volatile, write_volatile};

pub const QUEUE_SIZE: usize = 16;

pub const VIRTIO_MMIO_BASE: usize = 0x0a00_0000;
pub const VIRTIO_MMIO_STRIDE: usize = 0x200;
pub const VIRTIO_MMIO_SLOTS: usize = 32;

const VIRTIO_MAGIC: u32 = 0x7472_6976; // "virt"
const VIRTIO_VERSION_2: u32 = 2;

const REG_MAGIC_VALUE: usize = 0x000;
const REG_VERSION: usize = 0x004;
const REG_DEVICE_ID: usize = 0x008;
const REG_DEVICE_FEATURES: usize = 0x010;
const REG_DEVICE_FEATURES_SEL: usize = 0x014;
const REG_DRIVER_FEATURES: usize = 0x020;
const REG_DRIVER_FEATURES_SEL: usize = 0x024;
const REG_QUEUE_SEL: usize = 0x030;
const REG_QUEUE_NUM_MAX: usize = 0x034;
const REG_QUEUE_NUM: usize = 0x038;
const REG_QUEUE_READY: usize = 0x044;
const REG_QUEUE_NOTIFY: usize = 0x050;
const REG_INTERRUPT_STATUS: usize = 0x060;
const REG_INTERRUPT_ACK: usize = 0x064;
const REG_STATUS: usize = 0x070;
const REG_QUEUE_DESC_LOW: usize = 0x080;
const REG_QUEUE_DESC_HIGH: usize = 0x084;
const REG_QUEUE_DRIVER_LOW: usize = 0x090;
const REG_QUEUE_DRIVER_HIGH: usize = 0x094;
const REG_QUEUE_DEVICE_LOW: usize = 0x0a0;
const REG_QUEUE_DEVICE_HIGH: usize = 0x0a4;

const STATUS_RESET: u32 = 0;
const STATUS_ACKNOWLEDGE: u32 = 1 << 0;
const STATUS_DRIVER: u32 = 1 << 1;
const STATUS_DRIVER_OK: u32 = 1 << 2;
const STATUS_FEATURES_OK: u32 = 1 << 3;
const STATUS_FAILED: u32 = 1 << 7;

const FEATURE_HI_VERSION_1: u32 = 1 << 0; // VIRTIO_F_VERSION_1 (bit 32)

pub const VIRTQ_DESC_F_NEXT: u16 = 1 << 0;
pub const VIRTQ_DESC_F_WRITE: u16 = 1 << 1;

#[repr(C)]
#[derive(Copy, Clone)]
pub struct VirtqDesc {
    pub addr: u64,
    pub len: u32,
    pub flags: u16,
    pub next: u16,
}

#[repr(C)]
pub struct VirtqAvail {
    pub flags: u16,
    pub idx: u16,
    pub ring: [u16; QUEUE_SIZE],
    pub used_event: u16,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct VirtqUsedElem {
    pub id: u32,
    pub len: u32,
}

#[repr(C)]
pub struct VirtqUsed {
    pub flags: u16,
    pub idx: u16,
    pub ring: [VirtqUsedElem; QUEUE_SIZE],
    pub avail_event: u16,
}

#[repr(C, align(4096))]
pub struct SplitVirtqueue {
    pub desc: [VirtqDesc; QUEUE_SIZE],
    pub avail: VirtqAvail,
    pub used: VirtqUsed,
}

impl SplitVirtqueue {
    pub const fn zeroed() -> Self {
        Self {
            desc: [VirtqDesc {
                addr: 0,
                len: 0,
                flags: 0,
                next: 0,
            }; QUEUE_SIZE],
            avail: VirtqAvail {
                flags: 0,
                idx: 0,
                ring: [0; QUEUE_SIZE],
                used_event: 0,
            },
            used: VirtqUsed {
                flags: 0,
                idx: 0,
                ring: [VirtqUsedElem { id: 0, len: 0 }; QUEUE_SIZE],
                avail_event: 0,
            },
        }
    }
}

#[inline]
fn dsb_sy() {
    // SAFETY: `dsb sy` is a valid EL1 full-system data synchronization barrier.
    unsafe {
        asm!("dsb sy", options(nostack, preserves_flags));
    }
}

#[inline]
fn mmio_read32(base: usize, offset: usize) -> u32 {
    let ptr = (base + offset) as *const u32;
    // SAFETY: `base + offset` lies within the identity-mapped QEMU virt VirtIO MMIO region.
    unsafe { read_volatile(ptr) }
}

#[inline]
fn mmio_write32(base: usize, offset: usize, value: u32) {
    let ptr = (base + offset) as *mut u32;
    // SAFETY: `base + offset` lies within the identity-mapped QEMU virt VirtIO MMIO region.
    unsafe { write_volatile(ptr, value) }
}

pub fn find_device(device_id: u32) -> Option<(usize, usize)> {
    for slot in 0..VIRTIO_MMIO_SLOTS {
        let base = VIRTIO_MMIO_BASE + slot * VIRTIO_MMIO_STRIDE;
        if mmio_read32(base, REG_MAGIC_VALUE) != VIRTIO_MAGIC {
            continue;
        }
        if mmio_read32(base, REG_VERSION) != VIRTIO_VERSION_2 {
            continue;
        }
        if mmio_read32(base, REG_DEVICE_ID) == device_id {
            return Some((slot, base));
        }
    }
    None
}

pub struct VirtioTransport {
    base: usize,
    queue: Box<SplitVirtqueue>,
    avail_idx: u16,
    last_used_idx: u16,
}

impl VirtioTransport {
    pub fn init(base: usize) -> Result<Self, &'static str> {
        if mmio_read32(base, REG_MAGIC_VALUE) != VIRTIO_MAGIC {
            return Err("virtio: invalid magic");
        }
        if mmio_read32(base, REG_VERSION) != VIRTIO_VERSION_2 {
            return Err("virtio: unsupported MMIO version");
        }

        mmio_write32(base, REG_STATUS, STATUS_RESET);
        dsb_sy();

        let mut status = STATUS_ACKNOWLEDGE;
        mmio_write32(base, REG_STATUS, status);
        status |= STATUS_DRIVER;
        mmio_write32(base, REG_STATUS, status);

        mmio_write32(base, REG_DEVICE_FEATURES_SEL, 0);
        let _dev_feat_lo = mmio_read32(base, REG_DEVICE_FEATURES);
        mmio_write32(base, REG_DEVICE_FEATURES_SEL, 1);
        let dev_feat_hi = mmio_read32(base, REG_DEVICE_FEATURES);

        mmio_write32(base, REG_DRIVER_FEATURES_SEL, 0);
        mmio_write32(base, REG_DRIVER_FEATURES, 0);
        mmio_write32(base, REG_DRIVER_FEATURES_SEL, 1);
        mmio_write32(base, REG_DRIVER_FEATURES, dev_feat_hi & FEATURE_HI_VERSION_1);

        status |= STATUS_FEATURES_OK;
        mmio_write32(base, REG_STATUS, status);
        if mmio_read32(base, REG_STATUS) & STATUS_FEATURES_OK == 0 {
            mmio_write32(base, REG_STATUS, STATUS_FAILED);
            return Err("virtio: features rejected");
        }

        mmio_write32(base, REG_QUEUE_SEL, 0);
        if mmio_read32(base, REG_QUEUE_READY) != 0 {
            mmio_write32(base, REG_STATUS, STATUS_FAILED);
            return Err("virtio: queue 0 already ready");
        }

        let max_num = mmio_read32(base, REG_QUEUE_NUM_MAX);
        if max_num == 0 || (max_num as usize) < QUEUE_SIZE {
            mmio_write32(base, REG_STATUS, STATUS_FAILED);
            return Err("virtio: queue 0 too small");
        }

        let queue = Box::new(SplitVirtqueue::zeroed());
        let desc_addr = (&raw const queue.desc) as usize as u64;
        let avail_addr = (&raw const queue.avail) as usize as u64;
        let used_addr = (&raw const queue.used) as usize as u64;

        mmio_write32(base, REG_QUEUE_NUM, QUEUE_SIZE as u32);
        mmio_write32(base, REG_QUEUE_DESC_LOW, desc_addr as u32);
        mmio_write32(base, REG_QUEUE_DESC_HIGH, (desc_addr >> 32) as u32);
        mmio_write32(base, REG_QUEUE_DRIVER_LOW, avail_addr as u32);
        mmio_write32(base, REG_QUEUE_DRIVER_HIGH, (avail_addr >> 32) as u32);
        mmio_write32(base, REG_QUEUE_DEVICE_LOW, used_addr as u32);
        mmio_write32(base, REG_QUEUE_DEVICE_HIGH, (used_addr >> 32) as u32);
        dsb_sy();

        mmio_write32(base, REG_QUEUE_READY, 1);
        dsb_sy();

        status |= STATUS_DRIVER_OK;
        mmio_write32(base, REG_STATUS, status);
        dsb_sy();

        Ok(Self {
            base,
            queue,
            avail_idx: 0,
            last_used_idx: 0,
        })
    }

    #[inline]
    pub fn read_reg(&self, offset: usize) -> u32 {
        mmio_read32(self.base, offset)
    }

    // SAFETY: Each descriptor `(addr, len, flags)` must point to valid, identity-mapped RAM
    // that remains live and appropriately readable/writable for DMA until this call returns.
    pub unsafe fn submit_chain(&mut self, descs: [(u64, u32, u16); 3]) {
        for (i, (addr, len, flags)) in descs.into_iter().enumerate() {
            let desc = VirtqDesc {
                addr,
                len,
                flags: if i < 2 { flags | VIRTQ_DESC_F_NEXT } else { flags },
                next: if i < 2 { (i + 1) as u16 } else { 0 },
            };
            // SAFETY: `i < 3 < QUEUE_SIZE` and `self.queue` is exclusively owned.
            unsafe {
                write_volatile(&raw mut self.queue.desc[i], desc);
            }
        }

        let ring_slot = (self.avail_idx as usize) % QUEUE_SIZE;
        // SAFETY: `ring_slot < QUEUE_SIZE` and `self.queue` is exclusively owned.
        unsafe {
            write_volatile(&raw mut self.queue.avail.ring[ring_slot], 0);
        }
        dsb_sy();

        self.avail_idx = self.avail_idx.wrapping_add(1);
        // SAFETY: `self.queue` is exclusively owned and descriptors are published above.
        unsafe {
            write_volatile(&raw mut self.queue.avail.idx, self.avail_idx);
        }
        dsb_sy();

        mmio_write32(self.base, REG_QUEUE_NOTIFY, 0);

        let expected_used = self.last_used_idx.wrapping_add(1);
        loop {
            // SAFETY: `self.queue.used.idx` is updated by the VirtIO device in identity-mapped RAM.
            let used_idx = unsafe { read_volatile(&raw const self.queue.used.idx) };
            if used_idx == expected_used {
                break;
            }
            core::hint::spin_loop();
        }
        dsb_sy();

        self.last_used_idx = expected_used;

        let isr = mmio_read32(self.base, REG_INTERRUPT_STATUS);
        if isr != 0 {
            mmio_write32(self.base, REG_INTERRUPT_ACK, isr);
        }
    }
}
