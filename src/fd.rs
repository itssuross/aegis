use crate::pipe::{self, PipeEnd};

pub const MAX_FDS: usize = 16;
pub const O_RDONLY: u32 = 0;
pub const O_WRONLY: u32 = 1;
pub const O_RDWR: u32 = 2;
pub const O_CREAT: u32 = 0x40;
pub const O_TRUNC: u32 = 0x200;
pub const O_APPEND: u32 = 0x400;
pub(crate) const EBADF: i64 = -9;
pub(crate) const FILE_NAME_SIZE: usize = 48;
const FIRST_DYNAMIC_FD: usize = 3;

pub enum FdKind {
    ConsoleIn,
    ConsoleOut,
    AegisFile {
        name: [u8; FILE_NAME_SIZE],
        name_len: usize,
        readable: bool,
        writable: bool,
        append: bool,
        offset: usize,
    },
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
