use core::alloc::{GlobalAlloc, Layout};
use core::arch::asm;
use core::cell::UnsafeCell;
use core::mem::size_of;
use core::ops::{Deref, DerefMut};
use core::ptr;
use core::sync::atomic::{AtomicBool, Ordering};

const BLOCK_ALIGN: usize = 16;
const MIN_BLOCK_SIZE: usize = size_of::<FreeBlock>();

const _: () = assert!(MIN_BLOCK_SIZE <= BLOCK_ALIGN);

#[repr(C)]
struct FreeBlock {
    size: usize,
    next: *mut FreeBlock,
}

struct IrqSpinLock<T> {
    locked: AtomicBool,
    data: UnsafeCell<T>,
}

// SAFETY: Access to `data` is serialized by `locked`, with IRQs masked for the critical section.
unsafe impl<T: Send> Sync for IrqSpinLock<T> {}

impl<T> IrqSpinLock<T> {
    const fn new(data: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            data: UnsafeCell::new(data),
        }
    }

    fn lock(&self) -> IrqSpinLockGuard<'_, T> {
        let daif: u64;
        // SAFETY: Saving DAIF and masking IRQs (DAIF.I) at EL1 has no memory-safety implications.
        unsafe {
            asm!(
                "mrs {}, daif",
                "msr daifset, #2",
                out(reg) daif,
                options(nostack, preserves_flags),
            );
        }

        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            while self.locked.load(Ordering::Relaxed) {
                core::hint::spin_loop();
            }
        }

        IrqSpinLockGuard { lock: self, daif }
    }
}

struct IrqSpinLockGuard<'a, T> {
    lock: &'a IrqSpinLock<T>,
    daif: u64,
}

impl<T> Deref for IrqSpinLockGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: The guard proves exclusive ownership of the lock.
        unsafe { &*self.lock.data.get() }
    }
}

impl<T> DerefMut for IrqSpinLockGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: The guard proves exclusive ownership of the lock.
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<T> Drop for IrqSpinLockGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.locked.store(false, Ordering::Release);
        // SAFETY: Restores the DAIF state saved in `lock()`, re-enabling IRQs only if they were
        // enabled before the lock was taken.
        unsafe {
            asm!("msr daif, {}", in(reg) self.daif, options(nostack, preserves_flags));
        }
    }
}

struct Heap {
    head: FreeBlock,
    allocated: usize,
}

// SAFETY: The raw pointers reference heap memory owned exclusively by this `Heap`, and all
// access is serialized through `IrqSpinLock`.
unsafe impl Send for Heap {}

#[inline]
fn align_up(addr: usize, align: usize) -> Option<usize> {
    debug_assert!(align.is_power_of_two());
    Some(addr.checked_add(align - 1)? & !(align - 1))
}

#[inline]
fn block_layout(layout: Layout) -> (usize, usize) {
    let align = layout.align().max(BLOCK_ALIGN);
    let size = (layout.size().max(MIN_BLOCK_SIZE) + BLOCK_ALIGN - 1) & !(BLOCK_ALIGN - 1);
    (size, align)
}

impl Heap {
    const fn empty() -> Self {
        Self {
            head: FreeBlock {
                size: 0,
                next: ptr::null_mut(),
            },
            allocated: 0,
        }
    }

    // SAFETY: `start..start + size` must be unused, writable memory owned exclusively by the heap.
    unsafe fn init(&mut self, start: usize, size: usize) {
        self.head.next = ptr::null_mut();
        self.allocated = 0;

        let Some(aligned_start) = align_up(start, BLOCK_ALIGN) else {
            return;
        };
        let end = start.saturating_add(size) & !(BLOCK_ALIGN - 1);
        if end <= aligned_start || end - aligned_start < MIN_BLOCK_SIZE {
            return;
        }

        let block = aligned_start as *mut FreeBlock;
        // SAFETY: The caller guarantees the region is valid, writable, and exclusively owned.
        unsafe {
            block.write(FreeBlock {
                size: end - aligned_start,
                next: ptr::null_mut(),
            });
        }
        self.head.next = block;
    }

    fn alloc(&mut self, layout: Layout) -> *mut u8 {
        let (size, align) = block_layout(layout);
        let mut prev: *mut FreeBlock = &raw mut self.head;

        // SAFETY: Every node reachable from `head` is a valid `FreeBlock` inside the heap region.
        unsafe {
            let mut cur = (*prev).next;
            while !cur.is_null() {
                let block_start = cur as usize;
                let block_end = block_start + (*cur).size;

                if let Some(alloc_start) = align_up(block_start, align)
                    && let Some(alloc_end) = alloc_start.checked_add(size)
                    && alloc_end <= block_end
                {
                    let front = alloc_start - block_start;
                    let back = block_end - alloc_end;
                    let next = (*cur).next;

                    let after = if back > 0 {
                        let tail = alloc_end as *mut FreeBlock;
                        tail.write(FreeBlock { size: back, next });
                        tail
                    } else {
                        next
                    };

                    if front > 0 {
                        (*cur).size = front;
                        (*cur).next = after;
                    } else {
                        (*prev).next = after;
                    }

                    self.allocated += size;
                    return alloc_start as *mut u8;
                }

                prev = cur;
                cur = (*cur).next;
            }
        }

        ptr::null_mut()
    }

    // SAFETY: `ptr` must have been returned by `alloc` on this heap with the same `layout`.
    unsafe fn dealloc(&mut self, ptr: *mut u8, layout: Layout) {
        let (size, _) = block_layout(layout);
        let addr = ptr as usize;
        let head: *mut FreeBlock = &raw mut self.head;
        let mut prev = head;

        // SAFETY: Every node reachable from `head` is a valid `FreeBlock`, and the caller
        // guarantees `addr..addr + size` is a live allocation being returned to the heap.
        unsafe {
            while !(*prev).next.is_null() && ((*prev).next as usize) < addr {
                prev = (*prev).next;
            }

            let next = (*prev).next;
            let mut merged_size = size;
            let mut merged_next = next;

            if !next.is_null() && addr + size == next as usize {
                merged_size += (*next).size;
                merged_next = (*next).next;
            }

            if prev != head && prev as usize + (*prev).size == addr {
                (*prev).size += merged_size;
                (*prev).next = merged_next;
            } else {
                let block = addr as *mut FreeBlock;
                block.write(FreeBlock {
                    size: merged_size,
                    next: merged_next,
                });
                (*prev).next = block;
            }
        }

        self.allocated -= size;
    }

    fn free_bytes(&self) -> usize {
        let mut total = 0;
        let mut cur = self.head.next;
        // SAFETY: Every node reachable from `head` is a valid `FreeBlock` inside the heap region.
        unsafe {
            while !cur.is_null() {
                total += (*cur).size;
                cur = (*cur).next;
            }
        }
        total
    }
}

pub struct KernelAllocator {
    heap: IrqSpinLock<Heap>,
}

unsafe impl GlobalAlloc for KernelAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.heap.lock().alloc(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `GlobalAlloc` callers guarantee `ptr` was allocated here with `layout`.
        unsafe { self.heap.lock().dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: KernelAllocator = KernelAllocator {
    heap: IrqSpinLock::new(Heap::empty()),
};

// SAFETY: `heap_start..heap_start + heap_size` must be unused, writable memory reserved
// exclusively for the heap, and this must be called before any allocation and not while
// allocations are outstanding.
pub unsafe fn init(heap_start: usize, heap_size: usize) {
    // SAFETY: Forwarded from the caller's contract.
    unsafe { ALLOCATOR.heap.lock().init(heap_start, heap_size) }
}

pub fn stats() -> (usize, usize) {
    let heap = ALLOCATOR.heap.lock();
    (heap.allocated, heap.free_bytes())
}
