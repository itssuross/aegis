use alloc::boxed::Box;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::smp::{self, MAX_CPUS};
use crate::sync::{IrqSpinLock, irq_restore, irq_save_disable};

const MAX_THREADS: usize = MAX_CPUS + 7;

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
    pub cpu: Option<usize>,
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
    pub cpu: Option<usize>,
    pub pinned_cpu: Option<usize>,
    state: ThreadState,
    wake_tick_ms: Option<u64>,
    context: CpuContext,
    #[expect(dead_code, reason = "owns the stack backing the saved context")]
    stack: Option<Box<[u128; 1024]>>,
}

struct Scheduler {
    threads: [Option<Thread>; MAX_THREADS],
    current: [usize; MAX_CPUS],
}

struct SchedulerLock {
    held: AtomicBool,
    state: UnsafeCell<Scheduler>,
}

// SAFETY: Every access to state requires the IRQ-masked scheduler lock.
unsafe impl Sync for SchedulerLock {}

static SCHEDULER: SchedulerLock = SchedulerLock {
    held: AtomicBool::new(false),
    state: UnsafeCell::new(Scheduler {
        threads: [const { None }; MAX_THREADS],
        current: [0, 1, 2, 3],
    }),
};

struct SchedulerGuard {
    daif: u64,
    not_send: core::marker::PhantomData<*mut ()>,
}

impl SchedulerGuard {
    fn lock() -> Self {
        let daif = irq_save_disable();
        while SCHEDULER
            .held
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        Self {
            daif,
            not_send: core::marker::PhantomData,
        }
    }

    fn state(&mut self) -> &mut Scheduler {
        // SAFETY: The unique guard holds the lock for the lifetime of this borrow.
        unsafe { &mut *SCHEDULER.state.get() }
    }
}

impl Drop for SchedulerGuard {
    fn drop(&mut self) {
        post_switch_unlock(self.daif);
    }
}

extern "C" fn post_switch_unlock(daif: u64) {
    SCHEDULER.held.store(false, Ordering::Release);
    irq_restore(daif);
}

#[unsafe(naked)]
extern "C" fn first_switch_entry() -> ! {
    core::arch::naked_asm!(
        "mrs x0, daif",
        "bl {unlock}",
        "b {entry}",
        unlock = sym post_switch_unlock,
        entry = sym thread_entry_trampoline,
    );
}

static TRACE_ENABLED: AtomicBool = AtomicBool::new(false);

pub fn init() {
    let mut guard = SchedulerGuard::lock();
    let scheduler = guard.state();
    assert!(
        scheduler.threads[0].is_none(),
        "scheduler already initialized"
    );
    let names = ["idle-0", "idle-1", "idle-2", "idle-3"];
    for (cpu, name) in names.into_iter().enumerate() {
        scheduler.threads[cpu] = Some(Thread {
            id: cpu,
            name,
            cpu: (cpu == 0).then_some(cpu),
            pinned_cpu: Some(cpu),
            state: if cpu == 0 {
                ThreadState::Running
            } else {
                ThreadState::Ready
            },
            wake_tick_ms: None,
            context: CpuContext::default(),
            stack: None,
        });
    }
}

pub fn init_secondary(cpu_id: usize) {
    let mut guard = SchedulerGuard::lock();
    let scheduler = guard.state();
    scheduler.current[cpu_id] = cpu_id;
    let idle = scheduler.threads[cpu_id].as_mut().unwrap();
    idle.cpu = Some(cpu_id);
    idle.state = ThreadState::Running;
}

pub fn is_current_idle() -> bool {
    SchedulerGuard::lock().state().current[smp::cpu_id()] < MAX_CPUS
}

pub fn set_current_pinned(pinned: bool) {
    let mut guard = SchedulerGuard::lock();
    let scheduler = guard.state();
    let cpu = smp::cpu_id();
    let tid = scheduler.current[cpu];
    scheduler.threads[tid].as_mut().unwrap().pinned_cpu = (pinned || tid < MAX_CPUS).then_some(cpu);
}

pub fn spawn(name: &'static str, entry: extern "C" fn() -> !) -> usize {
    // SAFETY: An array of u128 is valid with every byte set to zero.
    let mut stack = unsafe { Box::<[u128; 1024]>::new_zeroed().assume_init() };
    let sp = stack.as_mut_ptr().wrapping_add(stack.len()) as u64;
    let context = CpuContext {
        x19: entry as usize as u64,
        x30: first_switch_entry as *const () as usize as u64,
        sp,
        ..CpuContext::default()
    };

    let mut guard = SchedulerGuard::lock();
    let scheduler = guard.state();
    assert!(scheduler.threads[0].is_some(), "scheduler not initialized");
    let id = scheduler
        .threads
        .iter()
        .position(Option::is_none)
        .expect("scheduler thread slots exhausted");
    scheduler.threads[id] = Some(Thread {
        id,
        name,
        cpu: None,
        pinned_cpu: None,
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
    let mut guard = SchedulerGuard::lock();
    let scheduler = guard.state();
    for (dst, src) in out.iter_mut().zip(scheduler.threads.iter()) {
        if let Some(thread) = src {
            *dst = Some(ThreadInfo {
                id: thread.id,
                cpu: thread.cpu.or(thread.pinned_cpu),
                name: thread.name,
                state: thread.state.as_str(),
            });
        }
    }
    out
}

pub fn yield_now() {
    schedule(SchedulerGuard::lock(), ThreadState::Ready);
}

fn mark_ready(thread: &mut Thread) {
    thread.state = ThreadState::Ready;
    thread.wake_tick_ms = None;
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
            let mut guard = SchedulerGuard::lock();
            let tid = guard.state().current[smp::cpu_id()];
            // Waiters and wakers acquire the scheduler lock before the queue lock.
            {
                let mut waiters = self.waiters.lock();
                if cond() {
                    return;
                }
                if tid >= MAX_CPUS {
                    let slot = waiters
                        .iter_mut()
                        .find(|slot| slot.is_none())
                        .expect("wait queue full");
                    *slot = Some(tid);
                }
            }
            schedule(
                guard,
                if tid < MAX_CPUS {
                    ThreadState::Ready
                } else {
                    ThreadState::Blocked
                },
            );
        }
    }

    pub fn wake_one(&self) {
        let mut guard = SchedulerGuard::lock();
        let mut waiters = self.waiters.lock();
        if let Some(tid) = waiters[0] {
            Self::remove_waiter(&mut waiters, 0);
            mark_ready(guard.state().threads[tid].as_mut().unwrap());
        }
    }

    #[allow(dead_code, reason = "current callers only need to wake one waiter")]
    pub fn wake_all(&self) {
        let mut guard = SchedulerGuard::lock();
        for tid in self.waiters.lock().iter_mut().filter_map(Option::take) {
            mark_ready(guard.state().threads[tid].as_mut().unwrap());
        }
    }

    fn remove_waiter(waiters: &mut [Option<usize>; MAX_THREADS], index: usize) {
        waiters[index..].rotate_left(1);
        waiters[MAX_THREADS - 1] = None;
    }
}

pub fn sleep_ms(ms: u64) {
    let mut guard = SchedulerGuard::lock();
    let scheduler = guard.state();
    let tid = scheduler.current[smp::cpu_id()];
    if tid >= MAX_CPUS {
        scheduler.threads[tid].as_mut().unwrap().wake_tick_ms =
            Some(crate::timer::uptime_ms().saturating_add(ms));
        schedule(guard, ThreadState::Blocked);
    }
}

pub fn on_timer_tick() {
    let now = crate::timer::uptime_ms();
    let mut guard = SchedulerGuard::lock();
    for thread in guard.state().threads.iter_mut().flatten() {
        if thread.state == ThreadState::Blocked
            && thread.wake_tick_ms.is_some_and(|deadline| deadline <= now)
        {
            mark_ready(thread);
        }
    }
    schedule(guard, ThreadState::Ready);
}

#[unsafe(no_mangle)]
pub extern "C" fn scheduler_thread_exit() -> ! {
    schedule(SchedulerGuard::lock(), ThreadState::Finished);
    unreachable!("a finished thread cannot resume");
}

fn schedule(mut guard: SchedulerGuard, previous_state: ThreadState) {
    let scheduler = guard.state();
    let cpu = smp::cpu_id();
    let previous = scheduler.current[cpu];
    let next = (1..MAX_THREADS)
        .map(|offset| (previous + offset) % MAX_THREADS)
        .find(|&slot| {
            slot >= MAX_CPUS
                && scheduler.threads[slot].as_ref().is_some_and(|thread| {
                    thread.state == ThreadState::Ready
                        && thread.pinned_cpu.is_none_or(|id| id == cpu)
                })
        })
        .unwrap_or(if previous_state == ThreadState::Ready {
            previous
        } else {
            cpu
        });
    if previous == next {
        return;
    }

    scheduler.current[cpu] = next;
    let [prev, next] = scheduler
        .threads
        .get_disjoint_mut([previous, next])
        .unwrap();
    let prev = prev.as_mut().expect("no current thread");
    let next = next.as_mut().expect("no next thread");
    prev.cpu = None;
    next.cpu = Some(cpu);
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
    smp::record_ctx_switch();
    let daif = guard.daif;
    core::mem::forget(guard);
    // SAFETY: The incoming continuation releases the lock only after both stack pointers are switched.
    unsafe { switch_context(prev_ptr, next_ptr) };
    post_switch_unlock(daif);
}
