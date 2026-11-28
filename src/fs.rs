use alloc::vec::Vec;

use crate::blk::{self, SECTOR_SIZE};
use crate::sync::IrqSpinLock;

const MAGIC: &[u8; 8] = b"AEGISFS1";
const TOTAL_SECTORS: usize = 512;
const MAX_ENTRIES: usize = 32;
const ENTRY_SIZE: usize = 64;
const NAME_SIZE: usize = 48;
const ENTRIES_PER_SECTOR: usize = SECTOR_SIZE / ENTRY_SIZE;
const DIRECTORY_START: usize = 1;
const DATA_START: usize = DIRECTORY_START + MAX_ENTRIES / ENTRIES_PER_SECTOR;
const ALLOCATION_SECTORS: usize = 8;

#[derive(Clone, Copy)]
pub struct FileMeta {
    pub entry_idx: usize,
    pub start_sector: usize,
    pub size: usize,
    pub capacity_sectors: usize,
    revision: u64,
}

pub struct FsStats {
    pub total_sectors: usize,
    pub used_sectors: usize,
    pub free_sectors: usize,
    pub active_files: usize,
    pub max_files: usize,
}

#[derive(Clone, Copy)]
struct DirEntry {
    name: [u8; NAME_SIZE],
    start_sector: u32,
    size: u32,
    capacity_sectors: u32,
}

impl DirEntry {
    fn decode(bytes: &[u8]) -> Result<Option<Self>, &'static str> {
        match read_u32(bytes, 48) {
            0 => return Ok(None),
            1 => {}
            _ => return Err("fs: invalid directory flags"),
        }

        let name = bytes[..NAME_SIZE].try_into().unwrap();
        let entry = Self {
            name,
            start_sector: read_u32(bytes, 52),
            size: read_u32(bytes, 56),
            capacity_sectors: read_u32(bytes, 60),
        };
        let name_end = entry.name_len();
        let name =
            core::str::from_utf8(&entry.name[..name_end]).map_err(|_| "fs: invalid filename")?;
        validate_name(name)?;
        if entry.name[name_end..].iter().any(|&byte| byte != 0) {
            return Err("fs: invalid filename padding");
        }

        let start = entry.start_sector as usize;
        let capacity = entry.capacity_sectors as usize;
        if !(DATA_START..TOTAL_SECTORS).contains(&start)
            || !(start - DATA_START).is_multiple_of(ALLOCATION_SECTORS)
            || capacity == 0
            || !capacity.is_multiple_of(ALLOCATION_SECTORS)
            || capacity > TOTAL_SECTORS - start
            || entry.size as usize > capacity * SECTOR_SIZE
        {
            return Err("fs: invalid file extent");
        }
        Ok(Some(entry))
    }

    fn encode(&self, bytes: &mut [u8]) {
        bytes[..NAME_SIZE].copy_from_slice(&self.name);
        bytes[48..52].copy_from_slice(&1u32.to_le_bytes());
        bytes[52..56].copy_from_slice(&self.start_sector.to_le_bytes());
        bytes[56..60].copy_from_slice(&self.size.to_le_bytes());
        bytes[60..64].copy_from_slice(&self.capacity_sectors.to_le_bytes());
    }

    fn name_len(&self) -> usize {
        self.name.iter().position(|&b| b == 0).unwrap_or(NAME_SIZE)
    }

    fn name(&self) -> &str {
        core::str::from_utf8(&self.name[..self.name_len()]).unwrap()
    }

    fn overlaps(&self, start: usize, capacity: usize) -> bool {
        let own_start = self.start_sector as usize;
        start < own_start + self.capacity_sectors as usize && own_start < start + capacity
    }
}

struct Filesystem {
    entries: [Option<DirEntry>; MAX_ENTRIES],
    revisions: [u64; MAX_ENTRIES],
}

static FS: IrqSpinLock<Option<Filesystem>> = IrqSpinLock::new(None);

impl Filesystem {
    fn index(&self, name: &str) -> Option<usize> {
        self.entries
            .iter()
            .position(|entry| entry.is_some_and(|entry| entry.name() == name))
    }

    fn meta(&self, entry_idx: usize) -> Option<FileMeta> {
        let entry = self.entries.get(entry_idx)?.as_ref()?;
        Some(FileMeta {
            entry_idx,
            start_sector: entry.start_sector as usize,
            size: entry.size as usize,
            capacity_sectors: entry.capacity_sectors as usize,
            revision: self.revisions[entry_idx],
        })
    }

    fn allocate(&self, capacity: usize) -> Result<usize, &'static str> {
        let last_start = TOTAL_SECTORS - capacity;
        (DATA_START..=last_start)
            .step_by(ALLOCATION_SECTORS)
            .find(|&start| {
                !self
                    .entries
                    .iter()
                    .flatten()
                    .any(|entry| entry.overlaps(start, capacity))
            })
            .ok_or("fs: no contiguous space")
    }

    fn store_entry(
        &mut self,
        entry_idx: usize,
        entry: Option<DirEntry>,
    ) -> Result<(), &'static str> {
        let mut block = [0; SECTOR_SIZE];
        let sector_idx = entry_idx / ENTRIES_PER_SECTOR;
        let first = sector_idx * ENTRIES_PER_SECTOR;
        for (index, bytes) in block.chunks_exact_mut(ENTRY_SIZE).enumerate() {
            let index = first + index;
            let value = if index == entry_idx {
                entry
            } else {
                self.entries[index]
            };
            if let Some(value) = value {
                value.encode(bytes);
            }
        }

        blk::write_sector(DIRECTORY_START + sector_idx, &block)?;
        self.entries[entry_idx] = entry;
        self.revisions[entry_idx] = self.revisions[entry_idx].wrapping_add(1);
        Ok(())
    }
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn validate_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty() || name.len() >= NAME_SIZE || name.contains(['/', '\0']) {
        return Err("fs: invalid filename");
    }
    Ok(())
}

pub fn init() -> Result<(), &'static str> {
    let mut mounted = FS.lock();
    if mounted.is_some() {
        return Ok(());
    }

    let (_, _, capacity) = blk::info().ok_or("fs: block device unavailable")?;
    let mut block = [0; SECTOR_SIZE];
    blk::read_sector(0, &mut block)?;
    if &block[..8] != MAGIC {
        return Err("fs: invalid AegisFS magic");
    }
    if read_u32(&block, 8) as usize != TOTAL_SECTORS
        || read_u32(&block, 12) as usize != MAX_ENTRIES
        || read_u32(&block, 16) as usize != DATA_START
        || capacity < TOTAL_SECTORS as u64
    {
        return Err("fs: invalid AegisFS geometry");
    }

    let mut fs = Filesystem {
        entries: [None; MAX_ENTRIES],
        revisions: [0; MAX_ENTRIES],
    };
    for sector in DIRECTORY_START..DATA_START {
        blk::read_sector(sector as u64, &mut block)?;
        for (index, bytes) in block.chunks_exact(ENTRY_SIZE).enumerate() {
            if let Some(entry) = DirEntry::decode(bytes)? {
                if fs.entries.iter().flatten().any(|other| {
                    other.name() == entry.name()
                        || other
                            .overlaps(entry.start_sector as usize, entry.capacity_sectors as usize)
                }) {
                    return Err("fs: conflicting directory entries");
                }
                let entry_idx = (sector - DIRECTORY_START) * ENTRIES_PER_SECTOR + index;
                fs.entries[entry_idx] = Some(entry);
            }
        }
    }
    *mounted = Some(fs);
    Ok(())
}

pub fn find(name: &str) -> Option<FileMeta> {
    let mounted = FS.lock();
    let fs = mounted.as_ref()?;
    fs.meta(fs.index(name)?)
}

fn read_extent(meta: &FileMeta, mut offset: usize, dst: &mut [u8]) -> Result<usize, &'static str> {
    let count = dst.len().min(meta.size.saturating_sub(offset));
    let mut dst = &mut dst[..count];
    let mut block = [0; SECTOR_SIZE];

    while !dst.is_empty() {
        let sector = meta.start_sector + offset / SECTOR_SIZE;
        let within = offset % SECTOR_SIZE;
        let chunk = dst.len().min(SECTOR_SIZE - within);
        blk::read_sector(sector as u64, &mut block)?;
        dst[..chunk].copy_from_slice(&block[within..within + chunk]);
        offset += chunk;
        dst = &mut dst[chunk..];
    }
    Ok(count)
}

pub fn read_at(meta: &FileMeta, offset: usize, dst: &mut [u8]) -> Result<usize, &'static str> {
    let mounted = FS.lock();
    let fs = mounted.as_ref().ok_or("fs: AegisFS not mounted")?;
    let current = fs
        .meta(meta.entry_idx)
        .filter(|entry| entry.revision == meta.revision)
        .ok_or("fs: file changed")?;
    read_extent(&current, offset, dst)
}

pub fn read_file(name: &str) -> Result<Vec<u8>, &'static str> {
    let mounted = FS.lock();
    let fs = mounted.as_ref().ok_or("fs: AegisFS not mounted")?;
    let name = name.strip_prefix("./").unwrap_or(name);
    let index = fs.index(name).ok_or("fs: file not found")?;
    let meta = fs.meta(index).unwrap();
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(meta.size)
        .map_err(|_| "fs: file too large for available memory")?;
    bytes.resize(meta.size, 0);
    read_extent(&meta, 0, &mut bytes)?;
    Ok(bytes)
}

fn write_extent(start: usize, mut offset: usize, mut data: &[u8]) -> Result<(), &'static str> {
    let mut block = [0; SECTOR_SIZE];
    while !data.is_empty() {
        let sector = start + offset / SECTOR_SIZE;
        let within = offset % SECTOR_SIZE;
        let chunk = data.len().min(SECTOR_SIZE - within);
        if within != 0 {
            blk::read_sector(sector as u64, &mut block)?;
        }
        block[within..within + chunk].copy_from_slice(&data[..chunk]);
        block[within + chunk..].fill(0);
        blk::write_sector(sector, &block)?;
        offset += chunk;
        data = &data[chunk..];
    }
    Ok(())
}

fn update_file(name: &str, data: &[u8], append: bool) -> Result<(), &'static str> {
    validate_name(name)?;
    let mut mounted = FS.lock();
    let fs = mounted.as_mut().ok_or("fs: AegisFS not mounted")?;
    let entry_idx = fs
        .index(name)
        .or_else(|| fs.entries.iter().position(Option::is_none))
        .ok_or("fs: directory full")?;
    let previous = fs.meta(entry_idx);
    if append && data.is_empty() && previous.is_some() {
        return Ok(());
    }

    let offset = if append {
        previous.map_or(0, |meta| meta.size)
    } else {
        0
    };
    let size = offset.checked_add(data.len()).ok_or("fs: file too large")?;
    if size > (TOTAL_SECTORS - DATA_START) * SECTOR_SIZE {
        return Err("fs: file too large");
    }
    let needed = size.div_ceil(ALLOCATION_SECTORS * SECTOR_SIZE).max(1) * ALLOCATION_SECTORS;
    let (start, capacity) = match previous {
        Some(meta) if meta.capacity_sectors >= needed => (meta.start_sector, meta.capacity_sectors),
        _ => {
            let start = fs.allocate(needed)?;
            if let Some(meta) = previous.filter(|_| offset != 0) {
                let mut block = [0; SECTOR_SIZE];
                for index in 0..offset.div_ceil(SECTOR_SIZE) {
                    blk::read_sector((meta.start_sector + index) as u64, &mut block)?;
                    blk::write_sector(start + index, &block)?;
                }
            }
            (start, needed)
        }
    };

    write_extent(start, offset, data)?;
    let mut entry = DirEntry {
        name: [0; NAME_SIZE],
        start_sector: start as u32,
        size: size as u32,
        capacity_sectors: capacity as u32,
    };
    entry.name[..name.len()].copy_from_slice(name.as_bytes());
    fs.store_entry(entry_idx, Some(entry))
}

pub fn write_file(name: &str, data: &[u8]) -> Result<(), &'static str> {
    update_file(name, data, false)
}

pub fn append_file(name: &str, data: &[u8]) -> Result<(), &'static str> {
    update_file(name, data, true)
}

pub fn remove_file(name: &str) -> Result<(), &'static str> {
    validate_name(name)?;
    let mut mounted = FS.lock();
    let fs = mounted.as_mut().ok_or("fs: AegisFS not mounted")?;
    let index = fs.index(name).ok_or("fs: file not found")?;
    fs.store_entry(index, None)
}

pub fn list<F: FnMut(&str, usize)>(mut f: F) {
    let entries = {
        let mounted = FS.lock();
        let Some(fs) = mounted.as_ref() else {
            return;
        };
        fs.entries
    };
    for entry in entries.iter().flatten() {
        f(entry.name(), entry.size as usize);
    }
}

pub fn stats() -> FsStats {
    let mounted = FS.lock();
    let Some(fs) = mounted.as_ref() else {
        return FsStats {
            total_sectors: 0,
            used_sectors: 0,
            free_sectors: 0,
            active_files: 0,
            max_files: 0,
        };
    };

    let mut used_sectors = DATA_START;
    let mut active_files = 0;
    for entry in fs.entries.iter().flatten() {
        used_sectors += entry.capacity_sectors as usize;
        active_files += 1;
    }
    FsStats {
        total_sectors: TOTAL_SECTORS,
        used_sectors,
        free_sectors: TOTAL_SECTORS - used_sectors,
        active_files,
        max_files: MAX_ENTRIES,
    }
}
