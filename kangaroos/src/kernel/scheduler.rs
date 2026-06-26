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

        // Single pass in round-robin order starting just after the current
        // task.  Because we visit tasks in the order current+1, current+2, …
        // (wrapping), the *first* task seen at any priority level is the
        // correct round-robin winner for that level.  A strictly better
        // (lower) priority replaces the candidate; equal or worse does not.
        let start = (current + 1) % count;
        let mut best_prio = u8::MAX;
        let mut best_idx = usize::MAX;

        for offset in 0..count {
            let i = (start + offset) % count;
            let t = crate::ktask(i);
            // Accept if no candidate yet (covers priority == u8::MAX, i.e. idle),
            // or if this task has strictly higher priority (lower value).
            if matches!(t.state, TaskState::Ready | TaskState::Running)
                && (best_idx == usize::MAX || t.priority < best_prio)
            {
                best_prio = t.priority;
                best_idx = i;
            }
        }

        if best_idx != usize::MAX {
            return best_idx;
        }

        // Unreachable under normal operation (idle task is always Ready).
        #[cfg(not(feature = "defmt"))]
        panic!("scheduler: no ready task");
        #[cfg(feature = "defmt")]
        defmt::panic!("scheduler: no ready task");
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

        // Hoist both globals: one memory load each, shared across all loops.
        let count = crate::TASK_COUNT;
        let current = crate::CURRENT_TASK;

        // Guard: scheduler not yet started (svc_first_task_sp has not run yet).
        if !matches!(crate::ktask(current).state, TaskState::Running) {
            return false;
        }

        // cur_prio hoisted before the fused loop so it is available inside it.
        let cur_prio = crate::ktask(current).priority;
        let mut should_preempt = false;

        // Fused single pass: wake sleeping tasks whose deadline has passed, and
        // simultaneously check whether any ready task has higher priority than
        // the current one. `continue` on i == current avoids creating an
        // aliased `&mut Tcb` while `cur` is borrowed later.
        for i in 0..count {
            if i == current {
                continue;
            }
            let t = crate::ktask(i); // one pointer load per iteration
            if let TaskState::Sleeping(deadline) = t.state {
                if now >= deadline {
                    t.state = TaskState::Ready;
                }
            }
            // Covers both freshly-woken sleepers and tasks already ready.
            if matches!(t.state, TaskState::Ready) && t.priority < cur_prio {
                should_preempt = true;
                // Do not break — remaining sleepers must still be woken.
            }
        }

        if should_preempt {
            return true;
        }

        // Decrement the running task's time slice (single ktask call).
        let cur = crate::ktask(current);
        if cur.slice_remaining > 0 {
            cur.slice_remaining -= 1;
        }

        // On slice expiry, rotate if an equal-priority peer is ready.
        if cur.slice_remaining == 0 {
            cur.slice_remaining = cur.time_slice;
            for i in 0..count {
                if i == current {
                    continue;
                }
                let t = crate::ktask(i); // one pointer load per iteration
                if matches!(t.state, TaskState::Ready) && t.priority == cur_prio {
                    return true;
                }
            }
        }

        false
    }
}

// ---------------------------------------------------------------------------
// Sync-primitive helpers
//
// All four functions must be called with interrupts disabled (inside
// `cortex_m::interrupt::free`). They manipulate TCB state and the intrusive
// wait list but do **not** trigger PendSV — the caller is responsible for
// calling `cortex_m::peripheral::SCB::set_pendsv()` after the critical
// section when a context switch is needed.
// ---------------------------------------------------------------------------

/// Mark the currently running task as `Blocked`.
///
/// # Safety
/// Must be called inside `interrupt::free`. The caller must trigger PendSV
/// after leaving the critical section so the scheduler selects a new task.
pub(crate) unsafe fn block_current() {
    unsafe {
        crate::ktask(crate::CURRENT_TASK).state = TaskState::Blocked;
    }
}

/// Mark task `idx` as `Ready` and return whether PendSV should fire.
///
/// Returns `true` when the newly-ready task has a higher priority (lower
/// number) than the currently running task, indicating that preemption is
/// warranted. The caller must call `SCB::set_pendsv()` in that case.
///
/// # Safety
/// Must be called inside `interrupt::free`.
pub(crate) unsafe fn unblock(idx: usize) -> bool {
    unsafe {
        crate::ktask(idx).state = TaskState::Ready;
        crate::ktask(idx).priority < crate::ktask(crate::CURRENT_TASK).priority
    }
}

/// Prepend `task_idx` to the intrusive wait list rooted at `*head`.
///
/// O(1). The list is LIFO at insertion; priority ordering is enforced on
/// removal by `wait_list_pop_highest`.
///
/// # Safety
/// Must be called inside `interrupt::free`.
pub(crate) unsafe fn wait_list_push(head: &mut u8, task_idx: usize) {
    debug_assert!(task_idx <= 254, "task_idx {task_idx} exceeds u8 sentinel limit (254)");
    unsafe {
        crate::ktask(task_idx).wait_next = *head;
    }
    *head = task_idx as u8;
}

/// Remove and return the highest-priority (lowest `priority` value) task
/// from the wait list rooted at `*head`.
///
/// O(N waiters). Returns `usize::MAX` if the list is empty — callers should
/// check `*head != 0xFF` before calling.
///
/// # Safety
/// Must be called inside `interrupt::free`.
pub(crate) unsafe fn wait_list_pop_highest(head: &mut u8) -> usize {
    if *head == 0xFF {
        return usize::MAX;
    }

    unsafe {
        // Walk the list to find the entry with the smallest priority value.
        let mut best_idx = *head as usize;
        let mut best_prio = crate::ktask(best_idx).priority;
        let mut cur = crate::ktask(best_idx).wait_next;
        while cur != 0xFF {
            let cur_idx = cur as usize;
            let p = crate::ktask(cur_idx).priority;
            if p < best_prio {
                best_prio = p;
                best_idx = cur_idx;
            }
            cur = crate::ktask(cur_idx).wait_next;
        }

        // Unlink `best_idx` from the list.
        if *head as usize == best_idx {
            *head = crate::ktask(best_idx).wait_next;
        } else {
            let mut prev = *head as usize;
            loop {
                let next = crate::ktask(prev).wait_next as usize;
                if next == best_idx {
                    crate::ktask(prev).wait_next = crate::ktask(best_idx).wait_next;
                    break;
                }
                prev = next;
            }
        }

        crate::ktask(best_idx).wait_next = 0xFF;
        best_idx
    }
}

// ---------------------------------------------------------------------------
// Assembly-callable context-switch helpers — shared by v6m, v7m, v7em_fpu.
//
// ARMv8-M provides its own definitions in `arch/v8m.rs` that additionally
// update the PSPLIM register, so these are compiled out on that target.
// ---------------------------------------------------------------------------

/// Select and activate the first task at kernel launch.
///
/// Called from the SVCall stub in each arch module via `bl svc_first_task_sp`.
/// Finds the highest-priority `Ready` task, marks it `Running`, stores its
/// index in `CURRENT_TASK`, and returns its SP so the assembly performs the
/// first `EXC_RETURN` into task context.
#[unsafe(no_mangle)]
#[cfg(not(armv8m))]
unsafe extern "C" fn svc_first_task_sp() -> usize {
    // SAFETY: called from SVCall (Handler mode) before the scheduler starts.
    // Single-core Cortex-M: no concurrent mutation of TASKS/TASK_COUNT/
    // CURRENT_TASK is possible while we are in Handler mode.
    unsafe {
        let count = crate::TASK_COUNT;
        let mut best_prio = u8::MAX;
        let mut best_idx = 0usize;

        for i in 0..count {
            let t = crate::ktask(i);
            if matches!(t.state, TaskState::Ready) && t.priority < best_prio {
                best_prio = t.priority;
                best_idx = i;
            }
        }

        crate::CURRENT_TASK = best_idx;
        crate::ktask(best_idx).state = TaskState::Running;
        crate::ktask(best_idx).sp
    }
}

/// Save the current task's SP, select the next task, and return its SP.
///
/// Called from the PendSV stub in each arch module via
/// `bl pendsv_save_and_switch` (AAPCS: r0 in = old SP, r0 out = new SP).
/// Transitions the current task `Running → Ready` (or leaves `Blocked` /
/// `Sleeping` unchanged), then delegates to `find_next()`.
#[unsafe(no_mangle)]
#[cfg(not(armv8m))]
unsafe extern "C" fn pendsv_save_and_switch(current_sp: usize) -> usize {
    // SAFETY: called from PendSV (Handler mode, lowest interrupt priority).
    // Single-core Cortex-M: exclusive access to TASKS/TASK_COUNT/CURRENT_TASK
    // is guaranteed — no other Handler-mode code runs concurrently, and
    // Thread-mode code only touches these globals inside interrupt::free.
    unsafe {
        let old = crate::CURRENT_TASK;
        crate::ktask(old).sp = current_sp;

        if crate::ktask(old).state == TaskState::Running {
            crate::ktask(old).state = TaskState::Ready;
        }

        let next = find_next();
        crate::CURRENT_TASK = next;
        crate::ktask(next).state = TaskState::Running;

        crate::ktask(next).sp
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(unused_assignments)]
mod tests {
    use super::{find_next, tick, TICK};
    use crate::kernel::tcb::{Tcb, TaskState};
    use std::sync::Mutex;

    // All scheduler tests share global mutable state (TASKS_PTR, TASK_COUNT,
    // CURRENT_TASK, TICK).  Serialize them with a single lock so parallel
    // test threads do not corrupt each other's task arrays.
    static SCHED_LOCK: Mutex<()> = Mutex::new(());

    // Set up a stack-allocated task array and register it with the kernel
    // globals.  Returns a guard that resets the globals on drop.
    macro_rules! with_tasks {
        ($tasks:ident, $count:expr, $current:expr) => {
            let mut $tasks = [Tcb::zeroed(); $count];
            let _guard = {
                unsafe {
                    crate::TASKS_PTR = $tasks.as_mut_ptr();
                    crate::TASK_COUNT = $count;
                    crate::CURRENT_TASK = $current;
                }
                // Return a guard that clears the pointer when dropped.
                struct Guard;
                impl Drop for Guard {
                    fn drop(&mut self) {
                        unsafe {
                            crate::TASKS_PTR = core::ptr::null_mut();
                            crate::TASK_COUNT = 0;
                        }
                    }
                }
                Guard
            };
        };
    }

    #[test]
    fn find_next_picks_highest_priority_ready() {
        let _lock = SCHED_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        with_tasks!(tasks, 3, 0);
        tasks[0].state = TaskState::Running;  tasks[0].priority = 5;
        tasks[1].state = TaskState::Ready;    tasks[1].priority = 3; // wins
        tasks[2].state = TaskState::Blocked;  tasks[2].priority = 1; // blocked
        assert_eq!(find_next(), 1);
    }

    #[test]
    fn find_next_round_robin_equal_priority() {
        let _lock = SCHED_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        with_tasks!(tasks, 3, 0);
        tasks[0].state = TaskState::Running; tasks[0].priority = 5;
        tasks[1].state = TaskState::Ready;   tasks[1].priority = 5;
        tasks[2].state = TaskState::Ready;   tasks[2].priority = 5;
        // Search starts at current+1 = 1, so task 1 is the round-robin winner.
        assert_eq!(find_next(), 1);
    }

    #[test]
    fn find_next_skips_non_ready() {
        let _lock = SCHED_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        with_tasks!(tasks, 3, 0);
        tasks[0].state = TaskState::Running; tasks[0].priority = 5;
        tasks[1].state = TaskState::Blocked; tasks[1].priority = 0; // highest prio but blocked
        tasks[2].state = TaskState::Ready;   tasks[2].priority = 5;
        // Only task 2 is ready → it must be selected despite lower prio than task 1.
        assert_eq!(find_next(), 2);
    }

    #[test]
    fn tick_wakes_sleeping_task_and_requests_preempt() {
        let _lock = SCHED_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        with_tasks!(tasks, 2, 0);
        tasks[0].state = TaskState::Running;     tasks[0].priority = 5;
        tasks[0].time_slice = 10;                tasks[0].slice_remaining = 10;
        tasks[1].state = TaskState::Sleeping(5); tasks[1].priority = 3; // higher prio
        unsafe { TICK = 4; } // will become 5 after tick() increments
        let preempt = tick(); // TICK → 5 → wakes task 1 → preempt needed
        assert!(preempt, "expected preemption when a higher-prio task is woken");
        assert!(matches!(tasks[1].state, TaskState::Ready));
    }

    #[test]
    fn tick_does_not_preempt_when_lower_prio_sleeper_wakes() {
        let _lock = SCHED_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        with_tasks!(tasks, 2, 0);
        tasks[0].state = TaskState::Running;     tasks[0].priority = 2; // higher prio
        tasks[0].time_slice = 10;                tasks[0].slice_remaining = 10;
        tasks[1].state = TaskState::Sleeping(5); tasks[1].priority = 5; // lower prio
        unsafe { TICK = 4; }
        let preempt = tick();
        assert!(!preempt, "lower-prio wakeup must not trigger preemption");
        assert!(matches!(tasks[1].state, TaskState::Ready));
    }

    #[test]
    fn tick_slice_expiry_rotates_equal_prio() {
        let _lock = SCHED_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        with_tasks!(tasks, 2, 0);
        tasks[0].state = TaskState::Running; tasks[0].priority = 5;
        tasks[0].time_slice = 1;             tasks[0].slice_remaining = 1;
        tasks[1].state = TaskState::Ready;   tasks[1].priority = 5; // same prio peer
        unsafe { TICK = 0; }
        let preempt = tick(); // slice expires → rotate
        assert!(preempt);
    }

    #[test]
    fn tick_increments_global_counter() {
        let _lock = SCHED_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        with_tasks!(tasks, 1, 0);
        tasks[0].state = TaskState::Running; tasks[0].priority = 5;
        tasks[0].time_slice = 100;           tasks[0].slice_remaining = 100;
        unsafe { TICK = 999; }
        tick();
        assert_eq!(unsafe { TICK }, 1000);
    }
}
