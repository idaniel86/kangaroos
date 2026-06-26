use core::cell::UnsafeCell;
use core::marker::PhantomData;
use core::ops::{Deref, DerefMut};

use crate::kernel::scheduler;

struct MutexInner<T> {
    data: T,
    /// Task index of the current owner, `0xFF` = unlocked.
    owner: u8,
    /// Head of the intrusive wait list, `0xFF` = empty.
    wait_head: u8,
}

/// A mutual-exclusion lock with **priority inheritance** (PI).
///
/// When a higher-priority task blocks on a `Mutex` held by a lower-priority
/// task, the owner's effective priority is temporarily raised to the
/// waiter's priority, preventing priority inversion. The original priority
/// is restored when the lock is released.
///
/// Declare as a `static`:
///
/// ```ignore
/// static COUNTER: Mutex<u32> = Mutex::new(0);
///
/// let mut guard = COUNTER.lock();
/// *guard += 1;
/// // guard is released (and priority restored) here via Drop
/// ```
pub struct Mutex<T> {
    inner: UnsafeCell<MutexInner<T>>,
    /// Optional human-readable name. `None` when constructed with [`Mutex::new`];
    /// set by [`Mutex::new_named`] or the [`mutex!`] macro.
    pub name: Option<&'static str>,
}

// SAFETY: single-core Cortex-M; all mutations are guarded by `interrupt::free`.
unsafe impl<T: Send> Sync for Mutex<T> {}
unsafe impl<T: Send> Send for Mutex<T> {}

impl<T> Mutex<T> {
    /// Create a new, unlocked `Mutex` wrapping `data`. Prefer the [`mutex!`]
    /// macro for named statics.
    pub const fn new(data: T) -> Self {
        Mutex {
            inner: UnsafeCell::new(MutexInner { data, owner: 0xFF, wait_head: 0xFF }),
            name: None,
        }
    }

    /// Create a named `Mutex`. Called by the [`mutex!`] macro; prefer
    /// that macro over calling this directly.
    pub const fn new_named(data: T, name: &'static str) -> Self {
        Mutex {
            inner: UnsafeCell::new(MutexInner { data, owner: 0xFF, wait_head: 0xFF }),
            name: Some(name),
        }
    }

    /// Acquire the lock, blocking until it is available.
    ///
    /// Returns a [`MutexGuard`] that releases the lock when dropped.
    /// Must not be called from interrupt handlers.
    pub fn lock(&self) -> MutexGuard<'_, T> {
        let mut must_block = false;
        #[cfg(feature = "defmt")]
        let id = super::PrimName(self.name, self as *const _ as u32);

        crate::port::interrupt_free(|| unsafe {
            let inner = &mut *self.inner.get();
            if inner.owner == 0xFF {
                // Unlocked: claim it immediately.
                debug_assert!(crate::CURRENT_TASK <= 254, "CURRENT_TASK exceeds u8 sentinel limit");
                inner.owner = crate::CURRENT_TASK as u8;
                #[cfg(feature = "defmt")]
                defmt::debug!("mutex {}: acquired by '{}'", id, crate::ktask(crate::CURRENT_TASK).name);
            } else {
                // Locked: apply priority inheritance then sleep.
                let cur_idx = crate::CURRENT_TASK;
                let owner_idx = inner.owner as usize;
                let cur_prio = crate::ktask(cur_idx).priority;
                let owner_prio = crate::ktask(owner_idx).priority;
                if cur_prio < owner_prio {
                    // Boost owner to the waiter's (higher) priority.
                    #[cfg(feature = "defmt")]
                    defmt::debug!("mutex {}: PI boost '{}' prio {=u8} -> {=u8}",
                        id, crate::ktask(owner_idx).name, owner_prio, cur_prio);
                    crate::ktask(owner_idx).priority = cur_prio;
                }
                #[cfg(feature = "defmt")]
                defmt::debug!("mutex {}: contended, '{}' blocking, owner='{}'",
                    id, crate::ktask(cur_idx).name, crate::ktask(owner_idx).name);
                scheduler::wait_list_push(&mut inner.wait_head, cur_idx);
                scheduler::block_current();
                must_block = true;
            }
        });

        if must_block {
            // Switch away; unlock_internal will grant ownership before we resume.
            crate::port::trigger_pendsv();
        }

        MutexGuard { mutex: self, _not_send: PhantomData }
    }

    /// Attempt to acquire the lock without blocking.
    ///
    /// Returns `Some(guard)` if the lock was free, `None` otherwise.
    ///
    /// # Priority inheritance
    /// **`try_lock` does not apply priority inheritance (PI).** If a
    /// lower-priority task acquires the lock via `try_lock` while a
    /// higher-priority task later blocks in [`lock`], no PI boost is applied
    /// to the holder because the boost is set at the moment a waiter blocks —
    /// but `try_lock` never blocks.  This can cause unbounded priority
    /// inversion if the holder is preempted while holding the lock.
    ///
    /// Prefer [`lock`] in any code path where priority inversion matters.
    /// Use `try_lock` only for genuinely non-blocking, best-effort paths
    /// (e.g. ISR-adjacent code or spin-free fast paths where the caller
    /// immediately retries via the blocking [`lock`] on failure).
    ///
    /// [`lock`]: Mutex::lock
    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        let acquired = crate::port::interrupt_free(|| unsafe {
            let inner = &mut *self.inner.get();
            if inner.owner == 0xFF {
                debug_assert!(crate::CURRENT_TASK <= 254, "CURRENT_TASK exceeds u8 sentinel limit");
                inner.owner = crate::CURRENT_TASK as u8;
                true
            } else {
                false
            }
        });
        if acquired {
            Some(MutexGuard { mutex: self, _not_send: PhantomData })
        } else {
            None
        }
    }

    /// Release the lock. Called from `MutexGuard::drop` and from `Condvar::wait`.
    ///
    /// # Safety
    /// Caller must be the current lock owner. May be called from within an
    /// existing `interrupt::free` critical section — nesting is safe on
    /// single-core ARM because `CPSID` is idempotent.
    pub(crate) unsafe fn unlock_internal(&self) {
        let mut need_preempt = false;
        #[cfg(feature = "defmt")]
        let id = super::PrimName(self.name, self as *const _ as u32);

        crate::port::interrupt_free(|| unsafe {
            let inner = &mut *self.inner.get();
            let old_owner = inner.owner as usize;

            // Restore the holder's effective priority to its static base.
            crate::ktask(old_owner).priority = crate::ktask(old_owner).base_priority;

            if inner.wait_head != 0xFF {
                // Transfer ownership directly to the highest-priority waiter.
                let next_owner = scheduler::wait_list_pop_highest(&mut inner.wait_head);
                debug_assert!(next_owner <= 254, "next_owner {next_owner} exceeds u8 sentinel limit");
                inner.owner = next_owner as u8;
                need_preempt = scheduler::unblock(next_owner);
                #[cfg(feature = "defmt")]
                defmt::debug!("mutex {}: released by '{}', granted to '{}'",
                    id, crate::ktask(old_owner).name, crate::ktask(next_owner).name);
            } else {
                inner.owner = 0xFF;
                #[cfg(feature = "defmt")]
                defmt::debug!("mutex {}: released by '{}'", id, crate::ktask(old_owner).name);
            }
        });

        if need_preempt {
            crate::port::trigger_pendsv();
        }
    }
}

// ---------------------------------------------------------------------------
// MutexGuard
// ---------------------------------------------------------------------------

/// RAII guard returned by [`Mutex::lock`] and [`Mutex::try_lock`].
///
/// The underlying lock is released when this guard is dropped.
///
/// `!Send`: a guard must not be moved to another task because it carries the
/// lock ownership of the *creating* task's context (PI is per-task).
pub struct MutexGuard<'a, T> {
    pub(crate) mutex: &'a Mutex<T>,
    // *mut T is !Send + !Sync, which propagates to MutexGuard on stable Rust.
    _not_send: PhantomData<*mut T>,
}

impl<'a, T> MutexGuard<'a, T> {
    /// Return a reference to the underlying [`Mutex`]. Used by `Condvar::wait`
    /// to re-acquire the lock after being woken.
    pub(crate) fn mutex_ref(&self) -> &'a Mutex<T> {
        self.mutex
    }
}

impl<T> Deref for MutexGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: we hold the lock.
        unsafe { &(*self.mutex.inner.get()).data }
    }
}

impl<T> DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: we hold the lock.
        unsafe { &mut (*self.mutex.inner.get()).data }
    }
}

impl<T> Drop for MutexGuard<'_, T> {
    fn drop(&mut self) {
        // SAFETY: we are the owner — unlock_internal checks and transfers ownership.
        unsafe { self.mutex.unlock_internal() }
    }
}
