use core::mem::size_of;
use core::ptr::read_volatile;

use crate::sync::IrqSpinLock;
use crate::virtio::{self, VIRTQ_DESC_F_WRITE, VirtioTransport};

const VIRTIO_BLK_DEVICE_ID: u32 = 2;

const REG_CFG_CAPACITY_LO: usize = 0x100; // virtio_blk_config.capacity[31:0]
const REG_CFG_CAPACITY_HI: usize = 0x104; // virtio_blk_config.capacity[63:32]

const VIRTIO_BLK_T_IN: u32 = 0;
const VIRTIO_BLK_T_OUT: u32 = 1;

const VIRTIO_BLK_S_OK: u8 = 0;
const VIRTIO_BLK_S_IOERR: u8 = 1;
const VIRTIO_BLK_S_UNSUPP: u8 = 2;

pub const SECTOR_SIZE: usize = 512;

#[repr(C)]
#[allow(dead_code)]
struct BlkReqHeader {
    req_type: u32,
    reserved: u32,
    sector: u64,
}

struct BlkDevice {
    slot: usize,
    mmio_base: usize,
    capacity_sectors: u64,
    transport: VirtioTransport,
}

static BLK_DEVICE: IrqSpinLock<Option<BlkDevice>> = IrqSpinLock::new(None);

pub fn init() -> bool {
    let Some((slot, mmio_base)) = virtio::find_device(VIRTIO_BLK_DEVICE_ID) else {
        return false;
    };

    let Ok(transport) = VirtioTransport::init(mmio_base) else {
        return false;
    };

    let cap_lo = transport.read_reg(REG_CFG_CAPACITY_LO) as u64;
    let cap_hi = transport.read_reg(REG_CFG_CAPACITY_HI) as u64;
    let capacity_sectors = cap_lo | (cap_hi << 32);

    let mut guard = BLK_DEVICE.lock();
    *guard = Some(BlkDevice {
        slot,
        mmio_base,
        capacity_sectors,
        transport,
    });
    true
}

#[allow(dead_code)]
pub fn is_available() -> bool {
    BLK_DEVICE.lock().is_some()
}

pub fn info() -> Option<(usize, usize, u64)> {
    let guard = BLK_DEVICE.lock();
    let dev = guard.as_ref()?;
    Some((dev.slot, dev.mmio_base, dev.capacity_sectors))
}

#[allow(dead_code)]
fn do_request(
    req_type: u32,
    sector: u64,
    data_addr: u64,
    data_flags: u16,
) -> Result<(), &'static str> {
    let mut guard = BLK_DEVICE.lock();
    let dev = guard.as_mut().ok_or("blk: device unavailable")?;
    if sector >= dev.capacity_sectors {
        return Err("blk: sector out of range");
    }

    let req = BlkReqHeader {
        req_type,
        reserved: 0,
        sector,
    };
    let mut status: u8 = 0xFF;

    let descs = [
        (
            &raw const req as usize as u64,
            size_of::<BlkReqHeader>() as u32,
            0,
        ),
        (data_addr, SECTOR_SIZE as u32, data_flags),
        (&raw mut status as usize as u64, 1, VIRTQ_DESC_F_WRITE),
    ];

    // SAFETY: `req`, the 512-byte caller buffer at `data_addr`, and `status` reside in
    // identity-mapped RAM and remain valid for the duration of the synchronous request.
    unsafe {
        dev.transport.submit_chain(descs);
    }

    // SAFETY: `status` is written by the VirtIO device before `submit_chain` returns.
    let result = unsafe { read_volatile(&raw const status) };
    match result {
        VIRTIO_BLK_S_OK => Ok(()),
        VIRTIO_BLK_S_IOERR => Err("blk: I/O error"),
        VIRTIO_BLK_S_UNSUPP => Err("blk: unsupported operation"),
        _ => Err("blk: unknown status"),
    }
}

#[allow(dead_code)]
pub fn read_sector(sector: u64, buf: &mut [u8; 512]) -> Result<(), &'static str> {
    do_request(
        VIRTIO_BLK_T_IN,
        sector,
        buf.as_mut_ptr() as usize as u64,
        VIRTQ_DESC_F_WRITE,
    )
}

#[allow(dead_code)]
pub fn write_sector(sector: u64, buf: &[u8; 512]) -> Result<(), &'static str> {
    do_request(VIRTIO_BLK_T_OUT, sector, buf.as_ptr() as usize as u64, 0)
}
