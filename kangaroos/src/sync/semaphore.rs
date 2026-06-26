use core::cell::UnsafeCell;

use crate::kernel::scheduler;

struct SemaphoreInner {
    count: u8,
    max: u8,
    wait_head: u8, // 0xFF = empty
}

/// Counting semaphore.
///
/// Declare as a `static` — interior mutability is provided via `UnsafeCell`.
///
/// ```ignore
/// static SEM: Semaphore = Semaphore::new(0, 1);
///
/// // producer task
/// SEM.give();
///
/// // consumer task
/// SEM.take(); // blocks until a token is available
/// ```
///
/// `give()` is safe to call from interrupt handlers as well as from tasks.
pub struct Semaphore {
    inner: UnsafeCell<SemaphoreInner>,
    /// Optional human-readable name. `None` when constructed with [`Semaphore::new`];
    /// set by [`Semaphore::new_named`] or the [`semaphore!`] macro.
    pub name: Option<&'static str>,
}

// SAFETY: single-core Cortex-M; all mutations are guarded by `interrupt::free`.
unsafe impl Sync for Semaphore {}

impl Semaphore {
    /// Create an unnamed semaphore with `initial` tokens and a ceiling of `max`.
    ///
    /// `max` must be ≥ `initial`. A binary semaphore is `new(0, 1)`.
    /// Prefer the [`semaphore!`] macro for named statics.
    pub const fn new(initial: u8, max: u8) -> Self {
        Semaphore {
            inner: UnsafeCell::new(SemaphoreInner {
                count: initial,
                max,
                wait_head: 0xFF,
            }),
            name: None,
        }
    }

    /// Create a named semaphore. Called by the [`semaphore!`] macro; prefer
    /// that macro over calling this directly.
    pub const fn new_named(initial: u8, max: u8, name: &'static str) -> Self {
        Semaphore {
            inner: UnsafeCell::new(SemaphoreInner {
                count: initial,
                max,
                wait_head: 0xFF,
            }),
            name: Some(name),
        }
    }

    /// Consume one token, blocking the calling task until one is available.
    ///
    /// Must not be called from interrupt handlers.
    pub fn take(&self) {
        let mut must_block = false;
        #[cfg(feature = "defmt")]
        let id = super::PrimName(self.name, self as *const _ as u32);

        crate::port::interrupt_free(|| unsafe {
            let inner = &mut *self.inner.get();
            if inner.count > 0 {
                inner.count -= 1;
                #[cfg(feature = "defmt")]
                defmt::debug!("semaphore {}: taken by '{}', count={=u8}",
                    id, crate::ktask(crate::CURRENT_TASK).name, inner.count);
            } else {
                #[cfg(feature = "defmt")]
                defmt::debug!("semaphore {}: empty, '{}' blocking",
                    id, crate::ktask(crate::CURRENT_TASK).name);
                scheduler::wait_list_push(&mut inner.wait_head, crate::CURRENT_TASK);
                scheduler::block_current();
                must_block = true;
            }
        });

        if must_block {
            // Task resumes here after give() transfers the token.
            crate::port::trigger_pendsv();
        }
    }

    /// Try to consume one token without blocking.
    ///
    /// Returns `true` if a token was acquired, `false` if the semaphore was
    /// at zero. Safe to call from interrupt handlers.
    pub fn try_take(&self) -> bool {
        crate::port::interrupt_free(|| unsafe {
            let inner = &mut *self.inner.get();
            if inner.count > 0 {
                inner.count -= 1;
                true
            } else {
                false
            }
        })
    }

    /// Produce one token, unblocking the highest-priority waiting task if any.
    ///
    /// If no task is waiting and the count is below `max`, the count is
    /// incremented. If the count is already at `max`, the token is dropped
    /// (the semaphore is full).
    ///
    /// Safe to call from interrupt handlers as well as from tasks.
    pub fn give(&self) {
        let mut need_preempt = false;
        #[cfg(feature = "defmt")]
        let id = super::PrimName(self.name, self as *const _ as u32);

        crate::port::interrupt_free(|| unsafe {
            let inner = &mut *self.inner.get();
            if inner.wait_head != 0xFF {
                // Hand the token directly to the highest-priority waiter.
                let idx = scheduler::wait_list_pop_highest(&mut inner.wait_head);
                need_preempt = scheduler::unblock(idx);
                #[cfg(feature = "defmt")]
                defmt::debug!("semaphore {}: given, woke '{}'", id, crate::ktask(idx).name);
            } else if inner.count < inner.max {
                inner.count += 1;
                #[cfg(feature = "defmt")]
                defmt::debug!("semaphore {}: given, count={=u8}", id, inner.count);
            } else {
                // count == max and no waiters — token dropped.
                #[cfg(feature = "defmt")]
                defmt::warn!("semaphore {}: give dropped, count at max={=u8}", id, inner.max);
            }
        });

        if need_preempt {
            crate::port::trigger_pendsv();
        }
    }

    /// Return the current token count.
    pub fn count(&self) -> u8 {
        crate::port::interrupt_free(|| unsafe { (*self.inner.get()).count })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::Semaphore;

    #[test]
    fn semaphore_initial_count() {
        let sem = Semaphore::new(3, 5);
        assert_eq!(sem.count(), 3);
    }

    #[test]
    fn semaphore_try_take_decrements() {
        let sem = Semaphore::new(3, 5);
        assert!(sem.try_take());
        assert_eq!(sem.count(), 2);
        assert!(sem.try_take());
        assert!(sem.try_take());
        assert_eq!(sem.count(), 0);
        // Empty — must return false without blocking.
        assert!(!sem.try_take());
        assert_eq!(sem.count(), 0);
    }

    #[test]
    fn semaphore_give_increments() {
        let sem = Semaphore::new(0, 3);
        sem.give(); assert_eq!(sem.count(), 1);
        sem.give(); assert_eq!(sem.count(), 2);
        sem.give(); assert_eq!(sem.count(), 3);
        // At ceiling — token must be silently dropped.
        sem.give(); assert_eq!(sem.count(), 3);
    }

    #[test]
    fn semaphore_binary() {
        let sem = Semaphore::new(0, 1);
        assert!(!sem.try_take());
        sem.give();
        assert_eq!(sem.count(), 1);
        assert!(sem.try_take());
        assert_eq!(sem.count(), 0);
        assert!(!sem.try_take());
    }

    #[test]
    fn semaphore_give_then_take_roundtrip() {
        let sem = Semaphore::new(2, 4);
        assert!(sem.try_take());   // 2 → 1
        assert!(sem.try_take());   // 1 → 0
        sem.give();                // 0 → 1
        assert_eq!(sem.count(), 1);
        assert!(sem.try_take());   // 1 → 0
        assert!(!sem.try_take());  // still 0
    }
}
