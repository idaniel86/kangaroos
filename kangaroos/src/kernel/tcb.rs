/// Current execution state of a task.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum TaskState {
    /// Slot not yet initialised (default for the `TASKS` array).
    Uninit,
    /// In the run queue, eligible to be scheduled next.
    /// Carries the remaining time-slice ticks so the quantum is preserved
    /// when the task is priority-preempted before its slice expires.
    Ready { slice_remaining: u8 },
    /// Currently on-CPU — exactly one task has this state at any time.
    Running { slice_remaining: u8 },
    /// Blocked on a synchronisation primitive (used by Phase 3 sync objects).
    Blocked,
    /// Sleeping until the global tick counter reaches the stored deadline.
    Sleeping(u64),
    /// Task has called [`task::exit()`] and is permanently removed from all
    /// queues. The TCB slot is retained but the scheduler, canary checker,
    /// and all sync primitives skip tasks in this state.
    Dead,
}

/// Task control block — one entry in the static `TASKS` array.
#[repr(C)]
pub struct Tcb {
    /// Saved stack pointer, updated by PendSV on every context switch.
    pub(crate) sp: usize,
    /// Execution state.
    pub(crate) state: TaskState,
    /// Effective (possibly PI-boosted) priority: 0 = highest, `u8::MAX` = lowest.
    /// The scheduler always uses this field; `base_priority` stores the original.
    pub(crate) priority: u8,
    /// Original spawn priority, never changed after initialisation.
    /// `Mutex` restores `priority` to this value when the lock is released.
    pub(crate) base_priority: u8,
    /// Configured time-slice quantum for this task in SysTick ticks.
    /// Reloaded into the `slice_remaining` field of `Ready`/`Running` after each expiry.
    pub(crate) time_slice: u8,
    /// Lowest address of the task's stack slice, used for canary verification.
    pub(crate) stack_base: usize,
    /// Optional human-readable name for debugging.
    pub(crate) name: &'static str,
    /// Intrusive singly-linked wait-list next pointer.
    /// `null` means end of list. Valid only while `state == Blocked`.
    pub(crate) wait_next: *mut Tcb,
    /// Intrusive link for the global "all tasks" singly-linked list.
    /// Set once at spawn time by `spawn_into()`; never modified afterwards.
    /// Used by canary checks and `find_next()` to iterate every live task.
    pub(crate) all_next: *mut Tcb,
    /// Raw pointer (as `usize`) to a value parked on this task's frozen stack.
    ///
    /// Used by `Channel` blocking paths:
    /// - Blocked **sender**: points to a `MaybeUninit<T>` holding the pending value.
    /// - Blocked **receiver**: points to a `MaybeUninit<T>` slot to be filled by the sender.
    ///
    /// Valid only while `state == Blocked` and the task is in a channel wait list.
    /// Initialised to `0`; meaningless in any other state.
    pub(crate) wait_ptr: usize,
}

impl Tcb {
    /// Return an uninitialised TCB suitable as a const array filler.
    pub(crate) const fn zeroed() -> Self {
        Tcb {
            sp: 0,
            state: TaskState::Uninit,
            priority: 0,
            base_priority: 0,
            time_slice: 0,
            stack_base: 0,
            name: "",
            wait_next: core::ptr::null_mut(),
            all_next: core::ptr::null_mut(),
            wait_ptr: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::{TaskState, Tcb};
    use core::mem;

    #[test]
    fn tcb_zeroed_state_is_uninit() {
        let t = Tcb::zeroed();
        assert!(matches!(t.state, TaskState::Uninit));
    }

    #[test]
    fn tcb_zeroed_sentinel_fields() {
        let t = Tcb::zeroed();
        assert_eq!(t.sp, 0);
        assert_eq!(t.priority, 0);
        assert_eq!(t.base_priority, 0);
        assert_eq!(t.time_slice, 0);
        assert_eq!(t.stack_base, 0);
        assert_eq!(t.name, "");
        assert!(t.wait_next.is_null());
        assert!(t.all_next.is_null());
        assert_eq!(t.wait_ptr, 0);
    }

    #[test]
    fn task_state_equality() {
        assert_eq!(TaskState::Ready { slice_remaining: 5 }, TaskState::Ready { slice_remaining: 5 });
        assert_ne!(TaskState::Ready { slice_remaining: 5 }, TaskState::Ready { slice_remaining: 0 });
        assert_eq!(TaskState::Running { slice_remaining: 3 }, TaskState::Running { slice_remaining: 3 });
        assert_ne!(TaskState::Ready { slice_remaining: 0 }, TaskState::Blocked);
        assert_eq!(TaskState::Sleeping(100), TaskState::Sleeping(100));
        assert_ne!(TaskState::Sleeping(100), TaskState::Sleeping(200));
        assert_ne!(TaskState::Dead, TaskState::Uninit);
    }

    #[test]
    fn tcb_size_regression() {
        // On 32-bit ARM the TCB must fit within 64 bytes (~56 bytes expected).
        // On a 64-bit host each pointer doubles in size (especially the fat
        // pointer `name: &'static str`), so scale the limit accordingly (~88 bytes).
        let max = if mem::size_of::<usize>() == 4 { 64 } else { 96 };
        assert!(
            mem::size_of::<Tcb>() <= max,
            "Tcb is {} bytes — check for unintended field additions (limit {max})",
            mem::size_of::<Tcb>(),
        );
    }
}
