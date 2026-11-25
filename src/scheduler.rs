use alloc::boxed::Box;
use core::arch::asm;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, Ordering};

const MAX_THREADS: usize = 8;

#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct CpuContext {
    x19: u64,
    x20: u64,
    x21: u64,
    x22: u64,
    x23: u64,
    x24: u64,
    x25: u64,
    x26: u64,
    x27: u64,
    x28: u64,
    x29: u64,
    x30: u64,
    sp: u64,
}

const _: () = assert!(core::mem::size_of::<CpuContext>() == 104);
const _: () = assert!(core::mem::offset_of!(CpuContext, sp) == 96);
const _: () = assert!(core::mem::align_of::<[u128; 1024]>() == 16);

unsafe extern "C" {
    #[link_name = "switch_context_with_fp"]
    fn switch_context(prev: *mut CpuContext, next: *const CpuContext);
    fn thread_entry_trampoline();
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ThreadState {
    Ready,
    Running,
    Finished,
}

struct Thread {
    id: usize,
    name: &'static str,
    state: ThreadState,
    context: CpuContext,
    #[expect(dead_code, reason = "owns the stack backing the saved context")]
    stack: Option<Box<[u128; 1024]>>,
}

struct Scheduler {
    threads: [Option<Thread>; MAX_THREADS],
    current: usize,
}

struct SchedulerLock {
    locked: AtomicBool,
    data: UnsafeCell<Scheduler>,
}

// SAFETY: The lock serializes access to the scheduler, and boot parks all secondary cores.
unsafe impl Sync for SchedulerLock {}

static SCHEDULER: SchedulerLock = SchedulerLock {
    locked: AtomicBool::new(false),
    data: UnsafeCell::new(Scheduler {
        threads: [const { None }; MAX_THREADS],
        current: 0,
    }),
};

struct IrqMask(u64);

impl IrqMask {
    fn acquire() -> Self {
        let daif: u64;
        // SAFETY: EL1 can save DAIF and mask IRQs before acquiring the scheduler lock.
        unsafe {
            asm!(
                "mrs {}, daif",
                "msr daifset, #2",
                "isb",
                out(reg) daif,
                options(nostack, preserves_flags),
            );
        }
        Self(daif)
    }
}

impl Drop for IrqMask {
    fn drop(&mut self) {
        // SAFETY: Restore the DAIF saved on this thread's own stack.
        unsafe {
            asm!(
                "msr daif, {}",
                "isb",
                in(reg) self.0,
                options(nostack, preserves_flags),
            );
        }
    }
}

impl SchedulerLock {
    fn lock<'a>(&'a self, _irq_mask: &'a IrqMask) -> SchedulerGuard<'a> {
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        SchedulerGuard(self)
    }
}

struct SchedulerGuard<'a>(&'a SchedulerLock);

impl SchedulerGuard<'_> {
    fn data(&mut self) -> &mut Scheduler {
        // SAFETY: The guard holds the lock and IRQs remain masked.
        unsafe { &mut *self.0.data.get() }
    }
}

impl Drop for SchedulerGuard<'_> {
    fn drop(&mut self) {
        self.0.locked.store(false, Ordering::Release);
    }
}

pub fn init() {
    let irq_mask = IrqMask::acquire();
    let mut guard = SCHEDULER.lock(&irq_mask);
    let scheduler = guard.data();
    assert!(
        scheduler.threads[0].is_none(),
        "scheduler already initialized"
    );
    scheduler.threads[0] = Some(Thread {
        id: 0,
        name: "idle",
        state: ThreadState::Running,
        context: CpuContext::default(),
        stack: None,
    });
}

pub fn spawn(name: &'static str, entry: extern "C" fn() -> !) -> usize {
    // Allocate and zero in place so callers on 16 KiB stacks can spawn threads.
    // SAFETY: An array of u128 is valid with every byte set to zero.
    let mut stack = unsafe { Box::<[u128; 1024]>::new_zeroed().assume_init() };
    let sp = stack.as_mut_ptr().wrapping_add(stack.len()) as u64;
    let context = CpuContext {
        x19: entry as usize as u64,
        x30: thread_entry_trampoline as *const () as usize as u64,
        sp,
        ..CpuContext::default()
    };

    let irq_mask = IrqMask::acquire();
    let mut guard = SCHEDULER.lock(&irq_mask);
    let scheduler = guard.data();
    assert!(scheduler.threads[0].is_some(), "scheduler not initialized");
    let id = scheduler
        .threads
        .iter()
        .position(Option::is_none)
        .expect("scheduler thread slots exhausted");
    scheduler.threads[id] = Some(Thread {
        id,
        name,
        state: ThreadState::Ready,
        context,
        stack: Some(stack),
    });
    id
}

pub fn yield_now() {
    schedule(ThreadState::Ready);
}

pub fn on_timer_tick() {
    yield_now();
}

#[unsafe(no_mangle)]
pub extern "C" fn scheduler_thread_exit() -> ! {
    schedule(ThreadState::Finished);
    unreachable!("a finished thread cannot resume");
}

fn schedule(previous_state: ThreadState) {
    let irq_mask = IrqMask::acquire();
    let mut guard = SCHEDULER.lock(&irq_mask);
    let scheduler = guard.data();
    let previous = scheduler.current;
    let Some(next) = (1..MAX_THREADS)
        .map(|offset| (previous + offset) % MAX_THREADS)
        .find(|&slot| {
            scheduler.threads[slot]
                .as_ref()
                .is_some_and(|thread| thread.state == ThreadState::Ready)
        })
    else {
        assert!(previous_state == ThreadState::Ready, "no runnable thread");
        return;
    };

    scheduler.current = next;
    let [prev, next] = scheduler
        .threads
        .get_disjoint_mut([previous, next])
        .unwrap();
    let prev = prev.as_mut().expect("no current thread");
    let next = next.as_mut().expect("no next thread");
    prev.state = previous_state;
    next.state = ThreadState::Running;
    crate::kprintln!(
        "[sched] switching tid {} ({}) -> tid {} ({})",
        prev.id,
        prev.name,
        next.id,
        next.name
    );
    let prev_ptr = &raw mut prev.context;
    let next_ptr = &raw const next.context;
    drop(guard);

    // SAFETY: Both contexts live at fixed addresses in the static slot array. IRQs
    // stay masked until the incoming thread resumes its own continuation or trampoline.
    unsafe { switch_context(prev_ptr, next_ptr) };
}
