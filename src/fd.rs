use alloc::vec::Vec;

use crate::pipe::{self, PipeEnd};

pub const MAX_FDS: usize = 16;
pub(crate) const EBADF: i64 = -9;
const FIRST_DYNAMIC_FD: usize = 3;

pub enum FdKind {
    ConsoleIn,
    ConsoleOut,
    UstarFile { bytes: Vec<u8>, offset: usize },
    PipeRead(PipeEnd),
    PipeWrite(PipeEnd),
}

pub struct FdTable {
    entries: [Option<FdKind>; MAX_FDS],
}

impl FdTable {
    pub fn new_stdio() -> Self {
        let mut entries = [const { None }; MAX_FDS];
        entries[0] = Some(FdKind::ConsoleIn);
        entries[1] = Some(FdKind::ConsoleOut);
        entries[2] = Some(FdKind::ConsoleOut);
        Self { entries }
    }

    pub fn alloc(&mut self, kind: FdKind) -> Option<usize> {
        let index = self.entries[FIRST_DYNAMIC_FD..]
            .iter()
            .position(Option::is_none)?
            + FIRST_DYNAMIC_FD;
        self.entries[index] = Some(kind);
        Some(index)
    }

    pub fn has_capacity(&self, count: usize) -> bool {
        self.entries[FIRST_DYNAMIC_FD..]
            .iter()
            .filter(|entry| entry.is_none())
            .count()
            >= count
    }

    pub fn get(&self, fd: usize) -> Option<&FdKind> {
        self.entries.get(fd)?.as_ref()
    }

    pub fn get_mut(&mut self, fd: usize) -> Option<&mut FdKind> {
        self.entries.get_mut(fd)?.as_mut()
    }

    pub fn close(&mut self, fd: usize) -> Result<(), i64> {
        let entry = self
            .entries
            .get_mut(fd)
            .and_then(Option::take)
            .ok_or(EBADF)?;
        if let FdKind::PipeRead(end) | FdKind::PipeWrite(end) = entry {
            pipe::close(end);
        }
        Ok(())
    }

    pub fn close_all(&mut self) {
        for fd in 0..MAX_FDS {
            let _ = self.close(fd);
        }
    }
}
