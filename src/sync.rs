use core::arch::asm;
use core::cell::UnsafeCell;
use core::marker::PhantomData;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, Ordering};

pub fn irq_save_disable() -> u64 {
    let daif: u64;
    // SAFETY: EL1 can read DAIF and mask IRQs.
    unsafe {
        asm!(
            "mrs {}, daif",
            "msr daifset, #2",
            "isb",
            out(reg) daif,
            options(nostack, preserves_flags),
        );
    }
    daif
}

pub fn irq_restore(daif: u64) {
    // SAFETY: EL1 can write the interrupt mask bits in DAIF.
    unsafe {
        asm!(
            "msr daif, {}",
            "isb",
            in(reg) daif,
            options(nostack, preserves_flags),
        );
    }
}

pub struct IrqSpinLock<T> {
    locked: AtomicBool,
    data: UnsafeCell<T>,
}

// SAFETY: The lock grants exclusive access and only transfers values that are Send.
unsafe impl<T: Send> Sync for IrqSpinLock<T> {}

impl<T> IrqSpinLock<T> {
    pub const fn new(val: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            data: UnsafeCell::new(val),
        }
    }

    pub fn lock(&self) -> IrqSpinGuard<'_, T> {
        let daif = irq_save_disable();
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        IrqSpinGuard {
            lock: self,
            daif,
            not_send: PhantomData,
        }
    }
}

pub struct IrqSpinGuard<'a, T> {
    lock: &'a IrqSpinLock<T>,
    daif: u64,
    not_send: PhantomData<*mut ()>,
}

impl<T> Deref for IrqSpinGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: The guard holds the lock, and the borrow prevents mutable access.
        unsafe { &*self.lock.data.get() }
    }
}

impl<T> DerefMut for IrqSpinGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: The guard holds the lock and is exclusively borrowed.
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<T> Drop for IrqSpinGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.locked.store(false, Ordering::Release);
        irq_restore(self.daif);
    }
}
