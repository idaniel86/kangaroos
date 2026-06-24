use crate::kernel::tcb::TaskState;

/// Monotonic 64-bit tick counter, incremented once per SysTick interrupt.
///
/// # Safety
/// Modified only inside `tick()`, called exclusively from the SysTick handler.
pub(crate) static mut TICK: u64 = 0;

/// Return the index of the highest-priority ready task.
///
/// Among tasks at equal priority the search wraps around `CURRENT_TASK + 1`,
/// implementing round-robin ordering within a priority tier.
///
/// # Panics
/// Panics if no task is ready. Under normal operation the idle task (always
/// `Ready`) prevents this.
pub(crate) fn find_next() -> usize {
    // SAFETY: called from PendSV (Handler mode, lowest priority) where no
    // concurrent mutation of TASKS/TASK_COUNT/CURRENT_TASK is possible on a
    // single-core device.
    unsafe {
        let count = crate::TASK_COUNT;
        let current = crate::CURRENT_TASK;

        // Pass 1: find the minimum (highest) priority among all Ready/Running tasks.
        let mut best_prio = u8::MAX;
        for i in 0..count {
            let t = crate::ktask(i);
            if matches!(t.state, TaskState::Ready | TaskState::Running)
                && t.priority < best_prio
            {
                best_prio = t.priority;
            }
        }

        // Pass 2: starting just after the current task, pick the first task at
        // `best_prio` (round-robin within the priority tier).
        let start = (current + 1) % count;
        for offset in 0..count {
            let i = (start + offset) % count;
            let t = crate::ktask(i);
            if t.priority == best_prio
                && matches!(t.state, TaskState::Ready | TaskState::Running)
            {
                return i;
            }
        }

        // Unreachable under normal operation (idle task is always Ready).
        panic!("scheduler: no ready task");
    }
}

/// Advance the global tick counter and run time-based scheduling logic.
///
/// Called from the SysTick exception handler. Returns `true` when PendSV
/// should be triggered to perform a context switch.
pub(crate) fn tick() -> bool {
    // SAFETY: called exclusively from the SysTick exception handler (single-core).
    unsafe {
        TICK = TICK.wrapping_add(1);
        let now = TICK;

        // Wake any tasks whose sleep deadline has passed.
        for i in 0..crate::TASK_COUNT {
            if let TaskState::Sleeping(deadline) = crate::ktask(i).state {
                if now >= deadline {
                    crate::ktask(i).state = TaskState::Ready;
                }
            }
        }

        let current = crate::CURRENT_TASK;

        // Guard: scheduler not yet started (svc_first_task_sp has not run yet).
        if !matches!(crate::ktask(current).state, TaskState::Running) {
            return false;
        }

        let cur_prio = crate::ktask(current).priority;

        // Preempt immediately if a higher-priority task has become ready.
        for i in 0..crate::TASK_COUNT {
            if i != current
                && matches!(crate::ktask(i).state, TaskState::Ready)
                && crate::ktask(i).priority < cur_prio
            {
                return true;
            }
        }

        // Decrement the running task's time slice.
        let slice = crate::ktask(current).slice_remaining;
        if slice > 0 {
            crate::ktask(current).slice_remaining = slice - 1;
        }

        // On slice expiry, rotate if an equal-priority peer is ready.
        if crate::ktask(current).slice_remaining == 0 {
            crate::ktask(current).slice_remaining = crate::ktask(current).time_slice;
            for i in 0..crate::TASK_COUNT {
                if i != current
                    && matches!(crate::ktask(i).state, TaskState::Ready)
                    && crate::ktask(i).priority == cur_prio
                {
                    return true;
                }
            }
        }

        false
    }
}
