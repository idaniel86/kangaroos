/// Current execution state of a task.
#[derive(Copy, Clone, PartialEq, Eq)]
pub(crate) enum TaskState {
    /// Slot not yet initialised (default for the `TASKS` array).
    Uninit,
    /// In the run queue, eligible to be scheduled next.
    Ready,
    /// Currently on-CPU — exactly one task has this state at any time.
    Running,
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
#[derive(Copy, Clone)]
#[repr(C)]
pub(crate) struct Tcb {
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
    /// Reloaded into `slice_remaining` after each expiry.
    pub(crate) time_slice: u8,
    /// Time-slice ticks remaining before round-robin rotation within a priority tier.
    pub(crate) slice_remaining: u8,
    /// Lowest address of the task's stack slice, used for canary verification.
    pub(crate) stack_base: usize,
    /// Optional human-readable name for debugging.
    pub(crate) name: &'static str,
    /// Intrusive singly-linked wait-list next pointer.
    /// `0xFF` means "end of list". Valid only while `state == Blocked`.
    pub(crate) wait_next: u8,
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
            slice_remaining: 0,
            stack_base: 0,
            name: "",
            wait_next: 0xFF,
            wait_ptr: 0,
        }
    }
}
