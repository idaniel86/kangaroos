use core::cell::UnsafeCell;
use core::marker::PhantomData;
use core::ops::{Deref, DerefMut};

use crate::kernel::scheduler;
use crate::kernel::tcb::Tcb;

struct MutexInner<T> {
    data: T,
    /// Pointer to the current owner's TCB, `null` = unlocked.
    owner: *mut Tcb,
    /// Head of the intrusive wait list, `null` = empty.
    wait_head: *mut Tcb,
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
            inner: UnsafeCell::new(MutexInner {
                data,
                owner: core::ptr::null_mut(),
                wait_head: core::ptr::null_mut(),
            }),
            name: None,
        }
    }

    /// Create a named `Mutex`. Called by the [`mutex!`] macro; prefer
    /// that macro over calling this directly.
    pub const fn new_named(data: T, name: &'static str) -> Self {
        Mutex {
            inner: UnsafeCell::new(MutexInner {
                data,
                owner: core::ptr::null_mut(),
                wait_head: core::ptr::null_mut(),
            }),
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
            if inner.owner.is_null() {
                // Unlocked: claim it immediately.
                inner.owner = crate::CURRENT;
                #[cfg(feature = "defmt")]
                defmt::debug!("mutex {}: acquired by '{}'", id, (*crate::CURRENT).name);
            } else {
                // Locked: apply priority inheritance then sleep.
                let owner = inner.owner;
                let cur_prio = (*crate::CURRENT).priority;
                let owner_prio = (*owner).priority;
                if cur_prio < owner_prio {
                    // Boost owner to the waiter's (higher) priority.
                    #[cfg(feature = "defmt")]
                    defmt::debug!(
                        "mutex {}: PI boost '{}' prio {=u8} -> {=u8}",
                        id,
                        (*owner).name,
                        owner_prio,
                        cur_prio
                    );
                    (*owner).priority = cur_prio;
                }
                #[cfg(feature = "defmt")]
                defmt::debug!(
                    "mutex {}: contended, '{}' blocking, owner='{}'",
                    id,
                    (*crate::CURRENT).name,
                    (*owner).name
                );
                scheduler::block_and_push(&mut inner.wait_head, crate::CURRENT);
                must_block = true;
            }
        });

        if must_block {
            // Switch away; unlock_internal will grant ownership before we resume.
            crate::port::trigger_pendsv();
        }

        MutexGuard {
            mutex: self,
            _not_send: PhantomData,
        }
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
            if inner.owner.is_null() {
                inner.owner = crate::CURRENT;
                true
            } else {
                false
            }
        });
        if acquired {
            Some(MutexGuard {
                mutex: self,
                _not_send: PhantomData,
            })
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
            let old_owner = inner.owner;

            // Restore the holder's effective priority to its static base.
            (*old_owner).priority = (*old_owner).base_priority;

            if !inner.wait_head.is_null() {
                // Transfer ownership directly to the highest-priority waiter.
                let next_owner = scheduler::wait_list_pop_highest(&mut inner.wait_head);
                inner.owner = next_owner;
                need_preempt = scheduler::unblock(next_owner);
                #[cfg(feature = "defmt")]
                defmt::debug!(
                    "mutex {}: released by '{}', granted to '{}'",
                    id,
                    (*old_owner).name,
                    (*next_owner).name
                );
            } else {
                inner.owner = core::ptr::null_mut();
                #[cfg(feature = "defmt")]
                defmt::debug!("mutex {}: released by '{}'", id, (*old_owner).name);
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
