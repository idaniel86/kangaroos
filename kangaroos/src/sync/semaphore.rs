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

        cortex_m::interrupt::free(|_| unsafe {
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
            cortex_m::peripheral::SCB::set_pendsv();
        }
    }

    /// Try to consume one token without blocking.
    ///
    /// Returns `true` if a token was acquired, `false` if the semaphore was
    /// at zero. Safe to call from interrupt handlers.
    pub fn try_take(&self) -> bool {
        cortex_m::interrupt::free(|_| unsafe {
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

        cortex_m::interrupt::free(|_| unsafe {
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
            cortex_m::peripheral::SCB::set_pendsv();
        }
    }

    /// Return the current token count.
    pub fn count(&self) -> u8 {
        cortex_m::interrupt::free(|_| unsafe { (*self.inner.get()).count })
    }
}
