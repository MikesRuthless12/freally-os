// SPDX-License-Identifier: Apache-2.0
//! A spin lock that also masks interrupts while held, so an interrupt handler
//! on the same CPU can never deadlock against the code it interrupted.

use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, Ordering};

pub struct SpinLock<T> {
    locked: AtomicBool,
    value: UnsafeCell<T>,
}

// SAFETY: access to `value` is serialised by `locked`.
unsafe impl<T: Send> Sync for SpinLock<T> {}
// SAFETY: as above.
unsafe impl<T: Send> Send for SpinLock<T> {}

pub struct Guard<'a, T> {
    lock: &'a SpinLock<T>,
    irq_were_on: bool,
}

fn flags() -> u64 {
    let f: u64;
    // SAFETY: reads RFLAGS.
    unsafe { core::arch::asm!("pushfq; pop {}", out(reg) f, options(nomem, preserves_flags)) };
    f
}

impl<T> SpinLock<T> {
    pub const fn new(value: T) -> Self {
        SpinLock { locked: AtomicBool::new(false), value: UnsafeCell::new(value) }
    }

    pub fn lock(&self) -> Guard<'_, T> {
        let irq_were_on = flags() & (1 << 9) != 0;
        crate::arch::cpu::interrupts_off();
        while self.locked.compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
            core::hint::spin_loop();
        }
        Guard { lock: self, irq_were_on }
    }
}

impl<T> Deref for Guard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: the guard holds the lock.
        unsafe { &*self.lock.value.get() }
    }
}

impl<T> DerefMut for Guard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: the guard holds the lock exclusively.
        unsafe { &mut *self.lock.value.get() }
    }
}

impl<T> Drop for Guard<'_, T> {
    fn drop(&mut self) {
        self.lock.locked.store(false, Ordering::Release);
        if self.irq_were_on {
            crate::arch::cpu::interrupts_on();
        }
    }
}
