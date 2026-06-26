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
pub struct Once {
    inner: UnsafeCell<OnceInner>,
    /// Optional human-readable name. `None` when constructed with [`Once::new`];
    /// set by [`Once::new_named`] or the [`once!`] macro.
    pub name: Option<&'static str>,
}

// SAFETY: single-core Cortex-M; all mutations guarded by `interrupt::free`.
unsafe impl Sync for Once {}

impl Default for Once {
    fn default() -> Self {
        Self::new()
    }
}

impl Once {
    /// Create an unnamed `Once`. Prefer the [`once!`] macro for named statics.
    pub const fn new() -> Self {
        Once {
            inner: UnsafeCell::new(OnceInner {
                state: OnceState::Unstarted,
                wait_head: 0xFF,
            }),
            name: None,
        }
    }

    /// Create a named `Once`. Called by the [`once!`] macro; prefer that
    /// macro over calling this directly.
    pub const fn new_named(name: &'static str) -> Self {
        Once {
            inner: UnsafeCell::new(OnceInner {
                state: OnceState::Unstarted,
                wait_head: 0xFF,
            }),
            name: Some(name),
        }
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
        #[cfg(feature = "defmt")]
        let id = super::PrimName(self.name, self as *const _ as u32);

        crate::port::interrupt_free(|| unsafe {
            let inner = &mut *self.inner.get();
            match inner.state {
                OnceState::Done => {}
                OnceState::Unstarted => {
                    inner.state = OnceState::Running;
                    am_initializer = true;
                }
                OnceState::Running => {
                    #[cfg(feature = "defmt")]
                    defmt::debug!(
                        "once {}: '{}' waiting for initialisation",
                        id,
                        crate::ktask(crate::CURRENT_TASK).name
                    );
                    scheduler::wait_list_push(&mut inner.wait_head, crate::CURRENT_TASK);
                    scheduler::block_current();
                    must_block = true;
                }
            }
        });

        if must_block {
            // Blocked waiter: resumes when the initialiser drains the wait list.
            crate::port::trigger_pendsv();
            return;
        }

        if !am_initializer {
            // Fast path: already done by the time we checked.
            return;
        }

        // Run the user-supplied initialisation closure outside any critical section.
        #[cfg(feature = "defmt")]
        defmt::debug!("once {}: '{}' initialising", id, unsafe {
            crate::ktask(crate::CURRENT_TASK).name
        });
        f();

        // Mark done and unblock all waiters.
        let mut need_preempt = false;
        crate::port::interrupt_free(|| unsafe {
            let inner = &mut *self.inner.get();
            inner.state = OnceState::Done;
            // Drain the entire wait list.
            while inner.wait_head != 0xFF {
                let idx = scheduler::wait_list_pop_highest(&mut inner.wait_head);
                if scheduler::unblock(idx) {
                    need_preempt = true;
                }
                #[cfg(feature = "defmt")]
                defmt::debug!(
                    "once {}: initialised, woke '{}'",
                    id,
                    crate::ktask(idx).name
                );
            }
        });

        if need_preempt {
            crate::port::trigger_pendsv();
        }
    }

    /// Returns `true` if the initialisation closure has already completed.
    pub fn is_completed(&self) -> bool {
        crate::port::interrupt_free(|| unsafe { (*self.inner.get()).state == OnceState::Done })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::Once;
    use core::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn once_runs_closure_exactly_once() {
        static COUNT: AtomicUsize = AtomicUsize::new(0);
        COUNT.store(0, Ordering::Relaxed);

        let once = Once::new();
        once.call_once(|| {
            COUNT.fetch_add(1, Ordering::Relaxed);
        });
        once.call_once(|| {
            COUNT.fetch_add(1, Ordering::Relaxed);
        });
        once.call_once(|| {
            COUNT.fetch_add(1, Ordering::Relaxed);
        });

        assert_eq!(COUNT.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn once_second_call_is_noop() {
        let once = Once::new();
        let mut ran = false;
        once.call_once(|| {
            ran = true;
        });
        assert!(ran);

        ran = false;
        once.call_once(|| {
            ran = true;
        }); // state is Done — must not run
        assert!(!ran);
    }

    #[test]
    fn once_is_completed_reflects_state() {
        let once = Once::new();
        assert!(!once.is_completed());
        once.call_once(|| {});
        assert!(once.is_completed());
    }

    #[test]
    fn once_closure_side_effect_is_visible() {
        static FLAG: AtomicUsize = AtomicUsize::new(0);
        FLAG.store(0, Ordering::Relaxed);

        let once = Once::new();
        once.call_once(|| FLAG.store(42, Ordering::Relaxed));
        assert_eq!(FLAG.load(Ordering::Relaxed), 42);

        // Subsequent calls must not alter FLAG.
        once.call_once(|| FLAG.store(99, Ordering::Relaxed));
        assert_eq!(FLAG.load(Ordering::Relaxed), 42);
    }
}
