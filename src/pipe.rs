use crate::scheduler::WaitQueue;
use crate::sync::IrqSpinLock;

pub const PIPE_BUF_SIZE: usize = 1024;
const MAX_PIPES: usize = 16;
const EBADF: i64 = -9;
const EPIPE: i64 = -32;

static PIPES: [PipeHandle; MAX_PIPES] = [const { PipeHandle::new() }; MAX_PIPES];

pub struct PipeHandle {
    state: IrqSpinLock<PipeState>,
    read_wait: WaitQueue,
    write_wait: WaitQueue,
}

struct PipeState {
    buf: [u8; PIPE_BUF_SIZE],
    head: usize,
    len: usize,
    readers: usize,
    writers: usize,
}

pub struct PipeEnd {
    pipe: &'static PipeHandle,
    readable: bool,
}

impl PipeHandle {
    const fn new() -> Self {
        Self {
            state: IrqSpinLock::new(PipeState {
                buf: [0; PIPE_BUF_SIZE],
                head: 0,
                len: 0,
                readers: 0,
                writers: 0,
            }),
            read_wait: WaitQueue::new(),
            write_wait: WaitQueue::new(),
        }
    }

    pub fn create() -> Option<(PipeEnd, PipeEnd)> {
        for pipe in &PIPES {
            let mut state = pipe.state.lock();
            if state.readers == 0 && state.writers == 0 {
                state.head = 0;
                state.len = 0;
                state.readers = 1;
                state.writers = 1;
                return Some((
                    PipeEnd {
                        pipe,
                        readable: true,
                    },
                    PipeEnd {
                        pipe,
                        readable: false,
                    },
                ));
            }
        }
        None
    }
}

impl Clone for PipeEnd {
    fn clone(&self) -> Self {
        let mut state = self.pipe.state.lock();
        if self.readable {
            state.readers += 1;
        } else {
            state.writers += 1;
        }
        Self {
            pipe: self.pipe,
            readable: self.readable,
        }
    }
}

impl Drop for PipeEnd {
    fn drop(&mut self) {
        let mut state = self.pipe.state.lock();
        let (refs, peer_wait) = if self.readable {
            (&mut state.readers, &self.pipe.write_wait)
        } else {
            (&mut state.writers, &self.pipe.read_wait)
        };
        *refs -= 1;
        let last = *refs == 0;
        drop(state);
        if last {
            peer_wait.wake_all();
        }
    }
}

pub fn read(end: &PipeEnd, dst: &mut [u8]) -> Result<usize, i64> {
    if !end.readable {
        return Err(EBADF);
    }
    if dst.is_empty() {
        return Ok(0);
    }

    let pipe = end.pipe;
    loop {
        let mut state = pipe.state.lock();
        if state.len != 0 {
            let count = dst.len().min(state.len);
            let first = count.min(PIPE_BUF_SIZE - state.head);
            dst[..first].copy_from_slice(&state.buf[state.head..state.head + first]);
            dst[first..count].copy_from_slice(&state.buf[..count - first]);
            state.head = (state.head + count) % PIPE_BUF_SIZE;
            state.len -= count;
            drop(state);
            pipe.write_wait.wake_all();
            return Ok(count);
        }
        if state.writers == 0 {
            return Ok(0);
        }
        drop(state);
        // WaitQueue takes the scheduler lock before the state lock in the predicate.
        pipe.read_wait.wait_until(|| {
            let state = pipe.state.lock();
            state.len != 0 || state.writers == 0
        });
    }
}

pub fn write(end: &PipeEnd, src: &[u8]) -> Result<usize, i64> {
    if end.readable {
        return Err(EBADF);
    }

    let pipe = end.pipe;
    loop {
        let mut state = pipe.state.lock();
        if state.readers == 0 {
            return Err(EPIPE);
        }
        if src.is_empty() {
            return Ok(0);
        }
        if state.len != PIPE_BUF_SIZE {
            let count = src.len().min(PIPE_BUF_SIZE - state.len);
            let tail = (state.head + state.len) % PIPE_BUF_SIZE;
            let first = count.min(PIPE_BUF_SIZE - tail);
            state.buf[tail..tail + first].copy_from_slice(&src[..first]);
            state.buf[..count - first].copy_from_slice(&src[first..count]);
            state.len += count;
            drop(state);
            pipe.read_wait.wake_all();
            return Ok(count);
        }
        drop(state);
        pipe.write_wait.wait_until(|| {
            let state = pipe.state.lock();
            state.len != PIPE_BUF_SIZE || state.readers == 0
        });
    }
}

pub fn close(end: PipeEnd) {
    drop(end);
}
