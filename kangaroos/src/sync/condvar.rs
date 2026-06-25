use core::cell::UnsafeCell;

use crate::kernel::scheduler;
use crate::sync::mutex::{Mutex, MutexGuard};

struct CondvarInner {
    /// Head of the intrusive wait list, `0xFF` = empty.
    wait_head: u8,
}

/// A condition variable for use with [`Mutex`].
///
/// Tasks call [`wait`] to atomically release the supplied mutex guard and
/// block until another task calls [`notify_one`] or [`notify_all`]. On
/// return from [`wait`] the mutex is re-acquired and a fresh guard is
/// returned. Callers should always re-check the guarded condition in a loop
/// because spurious wakeups are possible in theory (and the wait/notify
/// contract does not guarantee ordering):
///
/// ```ignore
/// static CV: Condvar = Condvar::new();
/// static MX: Mutex<u32> = Mutex::new(0);
///
/// // producer
/// {
///     let mut g = MX.lock();
///     *g = 42;
/// }
/// CV.notify_one();
///
/// // consumer
/// let mut g = MX.lock();
/// while *g == 0 {
///     g = CV.wait(g);
/// }
/// ```
pub struct Condvar(UnsafeCell<CondvarInner>);

// SAFETY: single-core Cortex-M; all mutations are guarded by `interrupt::free`.
unsafe impl Sync for Condvar {}
unsafe impl Send for Condvar {}

impl Condvar {
    /// Create a new condition variable.
    pub const fn new() -> Self {
        Condvar(UnsafeCell::new(CondvarInner { wait_head: 0xFF }))
    }

    /// Atomically release `guard`'s mutex and block until notified.
    ///
    /// On return, the mutex has been re-acquired and the new guard is
    /// returned. Must not be called from an interrupt handler.
    pub fn wait<'m, T>(&self, guard: MutexGuard<'m, T>) -> MutexGuard<'m, T> {
        // Capture the mutex reference before the guard is consumed.
        let mutex_ref: &'m Mutex<T> = guard.mutex_ref();

        // Prevent `guard`'s Drop from unlocking the mutex — we do that
        // manually inside the critical section below so that the release and
        // the self-block are atomic.
        let _guard = core::mem::ManuallyDrop::new(guard);

        cortex_m::interrupt::free(|_| unsafe {
            let inner = &mut *self.0.get();
            #[cfg(feature = "defmt")]
            defmt::debug!("condvar: '{}' waiting", crate::ktask(crate::CURRENT_TASK).name);
            scheduler::wait_list_push(&mut inner.wait_head, crate::CURRENT_TASK);
            scheduler::block_current();
            // Release the mutex inside the same critical section.
            // unlock_internal opens a nested interrupt::free; on single-core
            // ARM, CPSID is idempotent so nesting is safe.  It may itself
            // call SCB::set_pendsv() if a mutex waiter has higher priority.
            mutex_ref.unlock_internal();
        });

        // Always trigger a context switch: this task just blocked itself.
        // If unlock_internal already set PENDSVSET the write is idempotent.
        cortex_m::peripheral::SCB::set_pendsv();

        // When this task is resumed by notify_one / notify_all, re-acquire
        // the mutex before returning the guard to the caller.
        mutex_ref.lock()
    }

    /// Wake the highest-priority task waiting on this condition variable, if any.
    pub fn notify_one(&self) {
        let need_preempt = cortex_m::interrupt::free(|_| unsafe {
            let inner = &mut *self.0.get();
            if inner.wait_head == 0xFF {
                return false;
            }
            let idx = scheduler::wait_list_pop_highest(&mut inner.wait_head);
            let preempt = scheduler::unblock(idx);
            #[cfg(feature = "defmt")]
            defmt::debug!("condvar: notify_one, woke '{}'", crate::ktask(idx).name);
            preempt
        });

        if need_preempt {
            cortex_m::peripheral::SCB::set_pendsv();
        }
    }

    /// Wake all tasks waiting on this condition variable.
    ///
    /// Tasks are woken in priority order. After waking they each compete to
    /// re-acquire the associated mutex via [`Mutex::lock`].
    pub fn notify_all(&self) {
        let need_preempt = cortex_m::interrupt::free(|_| unsafe {
            let inner = &mut *self.0.get();
            let mut preempt = false;
            while inner.wait_head != 0xFF {
                let idx = scheduler::wait_list_pop_highest(&mut inner.wait_head);
                if scheduler::unblock(idx) {
                    preempt = true;
                }
                #[cfg(feature = "defmt")]
                defmt::debug!("condvar: notify_all, woke '{}'", crate::ktask(idx).name);
            }
            preempt
        });

        if need_preempt {
            cortex_m::peripheral::SCB::set_pendsv();
        }
    }
}
