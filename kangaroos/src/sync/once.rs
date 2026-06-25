use core::cell::UnsafeCell;

use crate::kernel::scheduler;

#[derive(Copy, Clone, PartialEq, Eq)]
enum OnceState {
    Unstarted,
    Running,
    Done,
}

struct OnceInner {
    state: OnceState,
    wait_head: u8, // 0xFF = empty
}

/// A synchronisation primitive that runs an initialisation closure exactly
/// once, even if multiple tasks race to call [`Once::call_once`] concurrently.
///
/// Tasks that call `call_once` while another task is running the initialiser
/// block until initialisation completes, then return.
///
/// ```ignore
/// static INIT: Once = Once::new();
///
/// INIT.call_once(|| {
///     // expensive one-time setup …
/// });
/// ```
pub struct Once(UnsafeCell<OnceInner>);

// SAFETY: single-core Cortex-M; all mutations guarded by `interrupt::free`.
unsafe impl Sync for Once {}

impl Once {
    /// Create an uninitialised `Once`.
    pub const fn new() -> Self {
        Once(UnsafeCell::new(OnceInner {
            state: OnceState::Unstarted,
            wait_head: 0xFF,
        }))
    }

    /// Run `f` if this is the first call; otherwise wait for the first caller
    /// to finish and then return.
    ///
    /// Guaranteed to call `f` at most once across all concurrent callers.
    /// Must not be called from interrupt handlers.
    pub fn call_once(&self, f: impl FnOnce()) {
        // Determine our role under the critical section.
        let mut am_initializer = false;
        let mut must_block = false;

        cortex_m::interrupt::free(|_| unsafe {
            let inner = &mut *self.0.get();
            match inner.state {
                OnceState::Done => {}
                OnceState::Unstarted => {
                    inner.state = OnceState::Running;
                    am_initializer = true;
                }
                OnceState::Running => {
                    #[cfg(feature = "defmt")]
                    defmt::debug!("once: '{}' waiting for initialisation",
                        crate::ktask(crate::CURRENT_TASK).name);
                    scheduler::wait_list_push(&mut inner.wait_head, crate::CURRENT_TASK);
                    scheduler::block_current();
                    must_block = true;
                }
            }
        });

        if must_block {
            // Blocked waiter: resumes when the initialiser drains the wait list.
            cortex_m::peripheral::SCB::set_pendsv();
            return;
        }

        if !am_initializer {
            // Fast path: already done by the time we checked.
            return;
        }

        // Run the user-supplied initialisation closure outside any critical section.
        #[cfg(feature = "defmt")]
        defmt::debug!("once: '{}' initialising", unsafe { crate::ktask(crate::CURRENT_TASK).name });
        f();

        // Mark done and unblock all waiters.
        let mut need_preempt = false;
        cortex_m::interrupt::free(|_| unsafe {
            let inner = &mut *self.0.get();
            inner.state = OnceState::Done;
            // Drain the entire wait list.
            while inner.wait_head != 0xFF {
                let idx = scheduler::wait_list_pop_highest(&mut inner.wait_head);
                if scheduler::unblock(idx) {
                    need_preempt = true;
                }
                #[cfg(feature = "defmt")]
                defmt::debug!("once: initialised, woke '{}'", crate::ktask(idx).name);
            }
        });

        if need_preempt {
            cortex_m::peripheral::SCB::set_pendsv();
        }
    }

    /// Returns `true` if the initialisation closure has already completed.
    pub fn is_completed(&self) -> bool {
        cortex_m::interrupt::free(|_| unsafe {
            (*self.0.get()).state == OnceState::Done
        })
    }
}
