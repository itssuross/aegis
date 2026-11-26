use alloc::vec::Vec;
use core::ops::ControlFlow;

use crate::blk::{self, SECTOR_SIZE};

pub struct DirEntry {
    name_buf: [u8; 100],
    name_len: usize,
    pub size: usize,
    pub data_sector: u64,
    pub is_dir: bool,
}

impl DirEntry {
    pub fn name(&self) -> &str {
        core::str::from_utf8(&self.name_buf[..self.name_len]).unwrap_or("<invalid UTF-8>")
    }
}

fn normalize(path: &[u8]) -> &[u8] {
    path.strip_prefix(b"./").unwrap_or(path)
}

fn parse_size(field: &[u8]) -> Result<usize, &'static str> {
    let is_digit_field = |&byte: &u8| !matches!(byte, 0 | b' ');
    let start = field.iter().position(is_digit_field).unwrap_or(field.len());
    let field = &field[start..];
    let end = field.iter().rposition(is_digit_field).map_or(0, |i| i + 1);
    let field = &field[..end];
    field.iter().try_fold(0usize, |size, &byte| {
        if !(b'0'..=b'7').contains(&byte) {
            return Err("fs: invalid ustar size");
        }
        size.checked_mul(8)
            .and_then(|size| size.checked_add(usize::from(byte - b'0')))
            .ok_or("fs: ustar size overflow")
    })
}

fn walk_entries(
    mut visit: impl FnMut(DirEntry, u8) -> ControlFlow<DirEntry>,
) -> Result<Option<DirEntry>, &'static str> {
    let (_, _, capacity) = blk::info().ok_or("fs: block device unavailable")?;
    let mut sector = 0;
    let mut header = [0; SECTOR_SIZE];

    while sector < capacity {
        blk::read_sector(sector, &mut header)?;
        if header.iter().all(|&byte| byte == 0) {
            break;
        }
        if &header[257..262] != b"ustar" {
            return Err("fs: invalid ustar magic");
        }

        let size = parse_size(&header[124..136])?;
        let data_sector = sector + 1;
        let data_sectors = size.div_ceil(SECTOR_SIZE) as u64;
        if data_sectors > capacity - data_sector {
            return Err("fs: ustar entry extends past the block device");
        }
        sector = data_sector + data_sectors;

        let name_field = &header[..100];
        let name_end = name_field.iter().position(|&byte| byte == 0).unwrap_or(100);
        let name = normalize(&name_field[..name_end]);
        if name.is_empty() {
            continue;
        }

        let typeflag = header[156];
        let mut name_buf = [0; 100];
        name_buf[..name.len()].copy_from_slice(name);
        let entry = DirEntry {
            name_buf,
            name_len: name.len(),
            size,
            data_sector,
            is_dir: typeflag == b'5',
        };
        if let ControlFlow::Break(entry) = visit(entry, typeflag) {
            return Ok(Some(entry));
        }
    }

    Ok(None)
}

pub fn list_entries<F: FnMut(&DirEntry)>(mut f: F) -> Result<usize, &'static str> {
    let mut count = 0;
    walk_entries(|entry, _| {
        f(&entry);
        count += 1;
        ControlFlow::Continue(())
    })?;
    Ok(count)
}

pub fn read_file(path: &str) -> Result<Vec<u8>, &'static str> {
    let path = normalize(path.as_bytes());
    let entry = walk_entries(|entry, typeflag| {
        if matches!(typeflag, b'0' | 0) && path == &entry.name_buf[..entry.name_len] {
            ControlFlow::Break(entry)
        } else {
            ControlFlow::Continue(())
        }
    })?
    .ok_or("fs: file not found")?;

    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(entry.size)
        .map_err(|_| "fs: file too large for available memory")?;
    let mut block = [0; SECTOR_SIZE];
    for index in 0..entry.size.div_ceil(SECTOR_SIZE) {
        blk::read_sector(entry.data_sector + index as u64, &mut block)?;
        let remaining = entry.size - bytes.len();
        bytes.extend_from_slice(&block[..remaining.min(SECTOR_SIZE)]);
    }
    Ok(bytes)
}
