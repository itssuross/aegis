use alloc::boxed::Box;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::sync::{IrqSpinLock, irq_restore, irq_save_disable};

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

#[derive(Clone, Copy)]
pub struct ThreadInfo {
    pub id: usize,
    pub name: &'static str,
    pub state: &'static str,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ThreadState {
    Ready,
    Running,
    Blocked,
    Finished,
}

impl ThreadState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "READY",
            Self::Running => "RUNNING",
            Self::Blocked => "Blocked",
            Self::Finished => "FINISHED",
        }
    }
}

struct Thread {
    id: usize,
    name: &'static str,
    state: ThreadState,
    wake_tick_ms: Option<u64>,
    context: CpuContext,
    #[expect(dead_code, reason = "owns the stack backing the saved context")]
    stack: Option<Box<[u128; 1024]>>,
}

struct Scheduler {
    threads: [Option<Thread>; MAX_THREADS],
    current: usize,
}

static SCHEDULER: IrqSpinLock<Scheduler> = IrqSpinLock::new(Scheduler {
    threads: [const { None }; MAX_THREADS],
    current: 0,
});

static TRACE_ENABLED: AtomicBool = AtomicBool::new(false);

pub fn init() {
    let mut scheduler = SCHEDULER.lock();
    assert!(
        scheduler.threads[0].is_none(),
        "scheduler already initialized"
    );
    scheduler.threads[0] = Some(Thread {
        id: 0,
        name: "idle",
        state: ThreadState::Running,
        wake_tick_ms: None,
        context: CpuContext::default(),
        stack: None,
    });
}

pub fn spawn(name: &'static str, entry: extern "C" fn() -> !) -> usize {
    // SAFETY: An array of u128 is valid with every byte set to zero.
    let mut stack = unsafe { Box::<[u128; 1024]>::new_zeroed().assume_init() };
    let sp = stack.as_mut_ptr().wrapping_add(stack.len()) as u64;
    let context = CpuContext {
        x19: entry as usize as u64,
        x30: thread_entry_trampoline as *const () as usize as u64,
        sp,
        ..CpuContext::default()
    };

    let mut scheduler = SCHEDULER.lock();
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
        wake_tick_ms: None,
        context,
        stack: Some(stack),
    });
    id
}

pub fn set_trace(enabled: bool) {
    TRACE_ENABLED.store(enabled, Ordering::Relaxed);
}

pub fn is_trace_enabled() -> bool {
    TRACE_ENABLED.load(Ordering::Relaxed)
}

pub fn snapshot() -> [Option<ThreadInfo>; MAX_THREADS] {
    let mut out = [None; MAX_THREADS];
    let scheduler = SCHEDULER.lock();
    for (dst, src) in out.iter_mut().zip(scheduler.threads.iter()) {
        if let Some(thread) = src {
            *dst = Some(ThreadInfo {
                id: thread.id,
                name: thread.name,
                state: thread.state.as_str(),
            });
        }
    }
    out
}

fn current_tid() -> usize {
    SCHEDULER.lock().current
}

pub fn yield_now() {
    schedule(ThreadState::Ready);
}

pub fn block_current_masked(saved_daif: u64) {
    schedule_masked(ThreadState::Blocked);
    irq_restore(saved_daif);
}

fn mark_ready(thread: &mut Thread) {
    thread.state = ThreadState::Ready;
    thread.wake_tick_ms = None;
}

fn try_wake_thread(tid: usize) -> bool {
    let mut scheduler = SCHEDULER.lock();
    let Some(thread) = scheduler.threads.get_mut(tid).and_then(Option::as_mut) else {
        return false;
    };
    if thread.state != ThreadState::Blocked {
        return false;
    }
    mark_ready(thread);
    true
}

pub fn wake_thread(tid: usize) {
    try_wake_thread(tid);
}

pub struct WaitQueue {
    waiters: IrqSpinLock<[Option<usize>; MAX_THREADS]>,
}

impl WaitQueue {
    pub const fn new() -> Self {
        Self {
            waiters: IrqSpinLock::new([None; MAX_THREADS]),
        }
    }

    pub fn wait_until<F: FnMut() -> bool>(&self, mut cond: F) {
        loop {
            let saved_daif = irq_save_disable();
            if cond() {
                irq_restore(saved_daif);
                return;
            }

            let tid = current_tid();
            if tid == 0 {
                irq_restore(saved_daif);
                yield_now();
                continue;
            }

            {
                let mut waiters = self.waiters.lock();
                let slot = waiters
                    .iter_mut()
                    .find(|slot| slot.is_none())
                    .expect("wait queue full");
                *slot = Some(tid);
            }
            block_current_masked(saved_daif);

            let mut waiters = self.waiters.lock();
            if let Some(index) = waiters.iter().position(|&slot| slot == Some(tid)) {
                Self::remove_waiter(&mut waiters, index);
            }
        }
    }

    pub fn wake_one(&self) {
        let mut waiters = self.waiters.lock();
        while let Some(tid) = waiters[0] {
            Self::remove_waiter(&mut waiters, 0);
            if try_wake_thread(tid) {
                break;
            }
        }
    }

    #[allow(dead_code, reason = "current callers only need to wake one waiter")]
    pub fn wake_all(&self) {
        let mut waiters = self.waiters.lock();
        for tid in waiters.iter_mut().filter_map(Option::take) {
            wake_thread(tid);
        }
    }

    fn remove_waiter(waiters: &mut [Option<usize>; MAX_THREADS], index: usize) {
        waiters[index..].rotate_left(1);
        waiters[MAX_THREADS - 1] = None;
    }
}

pub fn sleep_ms(ms: u64) {
    let saved_daif = irq_save_disable();
    let deadline = crate::timer::uptime_ms().saturating_add(ms);
    {
        let mut scheduler = SCHEDULER.lock();
        let tid = scheduler.current;
        if tid != 0 {
            let thread = scheduler.threads[tid].as_mut().expect("no current thread");
            thread.wake_tick_ms = Some(deadline);
        }
    }
    block_current_masked(saved_daif);
}

pub fn on_timer_tick() {
    let now = crate::timer::uptime_ms();
    {
        let mut scheduler = SCHEDULER.lock();
        for thread in scheduler.threads.iter_mut().flatten() {
            if thread.state == ThreadState::Blocked
                && thread.wake_tick_ms.is_some_and(|deadline| deadline <= now)
            {
                mark_ready(thread);
            }
        }
    }
    yield_now();
}

#[unsafe(no_mangle)]
pub extern "C" fn scheduler_thread_exit() -> ! {
    schedule(ThreadState::Finished);
    unreachable!("a finished thread cannot resume");
}

fn schedule(previous_state: ThreadState) {
    let saved_daif = irq_save_disable();
    schedule_masked(previous_state);
    irq_restore(saved_daif);
}

fn schedule_masked(previous_state: ThreadState) {
    let mut scheduler = SCHEDULER.lock();
    let previous = scheduler.current;
    if previous == 0 && previous_state == ThreadState::Blocked {
        return;
    }
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
    if TRACE_ENABLED.load(Ordering::Relaxed) {
        crate::kprintln!(
            "[sched] switching tid {} ({}) -> tid {} ({})",
            prev.id,
            prev.name,
            next.id,
            next.name
        );
    }
    let prev_ptr = &raw mut prev.context;
    let next_ptr = &raw const next.context;
    drop(scheduler);

    // SAFETY: Both contexts live at fixed addresses in the static slot array. IRQs
    // stay masked until the incoming thread resumes its own continuation or trampoline.
    unsafe { switch_context(prev_ptr, next_ptr) };
}
