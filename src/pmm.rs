use core::ptr;

use crate::platform::{PAGE_SHIFT, PAGE_SIZE, RAM_END, RAM_SIZE, phys_to_virt, virt_to_phys};
use crate::sync::IrqSpinLock;

const MAX_FRAMES: usize = 32_768;
const BITMAP_WORDS: usize = MAX_FRAMES / 64;

const _: () = assert!(BITMAP_WORDS == 512);
const _: () = assert!(MAX_FRAMES * PAGE_SIZE == RAM_SIZE);

unsafe extern "C" {
    static __heap_end: u8;
}

#[derive(Clone, Copy, Debug)]
pub struct PmmStats {
    pub managed_start: usize,
    pub managed_end: usize,
    pub total_frames: usize,
    pub used_frames: usize,
    pub free_frames: usize,
}

struct PmmState {
    managed_start: usize,
    managed_end: usize,
    total_frames: usize,
    used_frames: usize,
    bitmap: [u64; BITMAP_WORDS],
}

static PMM: IrqSpinLock<PmmState> = IrqSpinLock::new(PmmState::new());

#[inline]
const fn align_up(addr: usize, align: usize) -> usize {
    (addr + (align - 1)) & !(align - 1)
}

impl PmmState {
    const fn new() -> Self {
        Self {
            managed_start: 0,
            managed_end: 0,
            total_frames: 0,
            used_frames: 0,
            bitmap: [u64::MAX; BITMAP_WORDS],
        }
    }

    #[inline]
    fn set_used(&mut self, frame: usize) {
        let word = frame / 64;
        let mask = 1u64 << (frame % 64);
        if self.bitmap[word] & mask == 0 {
            self.bitmap[word] |= mask;
            self.used_frames += 1;
        }
    }

    #[inline]
    fn is_used(&self, frame: usize) -> bool {
        (self.bitmap[frame / 64] & (1u64 << (frame % 64))) != 0
    }
}

pub fn init() {
    let heap_end = virt_to_phys(&raw const __heap_end as usize);
    let start = align_up(heap_end, PAGE_SIZE);
    let end = RAM_END & !(PAGE_SIZE - 1);

    let mut state = PMM.lock();
    state.bitmap.fill(u64::MAX);
    state.used_frames = 0;

    if end <= start {
        state.managed_start = start;
        state.managed_end = start;
        state.total_frames = 0;
        return;
    }

    let total_frames = ((end - start) >> PAGE_SHIFT).min(MAX_FRAMES);
    state.managed_start = start;
    state.managed_end = start + (total_frames << PAGE_SHIFT);
    state.total_frames = total_frames;

    let full_words = total_frames / 64;
    let rem_bits = total_frames % 64;
    for word in &mut state.bitmap[..full_words] {
        *word = 0;
    }
    if rem_bits > 0 {
        state.bitmap[full_words] = !((1u64 << rem_bits) - 1);
    }
}

pub fn alloc_frame() -> Option<usize> {
    let phys = {
        let mut guard = PMM.lock();
        let state = &mut *guard;
        let mut allocated = None;
        for (word_idx, word) in state.bitmap.iter_mut().enumerate() {
            if *word != u64::MAX {
                let bit = word.trailing_ones() as usize;
                let frame = word_idx * 64 + bit;
                if frame < state.total_frames {
                    *word |= 1u64 << bit;
                    state.used_frames += 1;
                    allocated = Some(state.managed_start + (frame << PAGE_SHIFT));
                }
                break;
            }
        }
        allocated
    }?;

    // SAFETY: The frame is aligned, direct-mapped, and exclusively reserved in the bitmap.
    unsafe {
        ptr::write_bytes(phys_to_virt(phys) as *mut u8, 0, PAGE_SIZE);
    }
    Some(phys)
}

#[allow(dead_code)]
pub fn alloc_contiguous(count: usize) -> Option<usize> {
    if count == 0 {
        return None;
    }
    if count == 1 {
        return alloc_frame();
    }

    let phys = {
        let mut state = PMM.lock();
        if count > state.total_frames.saturating_sub(state.used_frames) {
            return None;
        }

        let mut run_start = 0usize;
        let mut run_len = 0usize;
        let mut frame = 0usize;
        let mut found = None;

        while frame < state.total_frames {
            if frame % 64 == 0 && state.bitmap[frame / 64] == u64::MAX {
                run_len = 0;
                frame += 64;
                continue;
            }
            if state.is_used(frame) {
                run_len = 0;
            } else {
                if run_len == 0 {
                    run_start = frame;
                }
                run_len += 1;
                if run_len == count {
                    found = Some(run_start);
                    break;
                }
            }
            frame += 1;
        }

        let start = found?;
        for idx in start..start + count {
            state.set_used(idx);
        }
        state.managed_start + (start << PAGE_SHIFT)
    };

    // SAFETY: All `count` contiguous frames are direct-mapped and exclusively reserved in the bitmap.
    unsafe {
        ptr::write_bytes(phys_to_virt(phys) as *mut u8, 0, count * PAGE_SIZE);
    }
    Some(phys)
}

pub fn free_frame(phys: usize) {
    if phys & (PAGE_SIZE - 1) != 0 {
        return;
    }

    let mut state = PMM.lock();
    if phys < state.managed_start || phys >= state.managed_end {
        return;
    }
    let frame = (phys - state.managed_start) >> PAGE_SHIFT;
    if frame >= state.total_frames {
        return;
    }
    let word = frame / 64;
    let mask = 1u64 << (frame % 64);
    if state.bitmap[word] & mask != 0 {
        state.bitmap[word] &= !mask;
        state.used_frames -= 1;
    }
}

pub fn stats() -> PmmStats {
    let state = PMM.lock();
    PmmStats {
        managed_start: state.managed_start,
        managed_end: state.managed_end,
        total_frames: state.total_frames,
        used_frames: state.used_frames,
        free_frames: state.total_frames.saturating_sub(state.used_frames),
    }
}
