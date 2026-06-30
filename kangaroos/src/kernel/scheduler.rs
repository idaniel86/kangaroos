use crate::kernel::tcb::{TaskState, Tcb};

/// Monotonic 64-bit tick counter, incremented once per SysTick interrupt.
///
/// # Safety
/// Modified only inside `tick()`, called exclusively from the SysTick handler.
pub(crate) static mut TICK: u64 = 0;

/// Return a pointer to the highest-priority ready task.
///
/// Among tasks at equal priority the search starts from the node after
/// `CURRENT`, implementing round-robin ordering within a priority tier.
///
/// # Panics
/// Panics if no task is ready. Under normal operation the idle task (always
/// `Ready`) prevents this.
pub(crate) fn find_next() -> *mut Tcb {
    // SAFETY: called from PendSV (Handler mode, lowest priority) where no
    // concurrent mutation of ALL_TASKS/CURRENT is possible on a
    // single-core device.
    unsafe {
        let current = crate::CURRENT;
        let list_head = crate::ALL_TASKS;

        // Round-robin start: node after CURRENT, or list head if CURRENT is
        // the last node or null (before first launch).
        let start = if current.is_null() {
            list_head
        } else {
            let n = (*current).all_next;
            if n.is_null() { list_head } else { n }
        };

        let mut best_prio = u8::MAX;
        let mut best: *mut Tcb = core::ptr::null_mut();

        // Pass 1: from start to end of list.
        // First task seen at a given priority wins the round-robin tiebreak.
        let mut t = start;
        while !t.is_null() {
            if matches!((*t).state, TaskState::Ready { .. } | TaskState::Running { .. })
                && (best.is_null() || (*t).priority < best_prio)
            {
                best_prio = (*t).priority;
                best = t;
            }
            t = (*t).all_next;
        }

        // Pass 2: from list head up to (but not including) start.
        // Only replaces on strictly higher priority, preserving the
        // round-robin winner from pass 1 at equal priority.
        if !current.is_null() {
            let mut t = list_head;
            while !t.is_null() && t != start {
                if matches!((*t).state, TaskState::Ready { .. } | TaskState::Running { .. })
                    && (best.is_null() || (*t).priority < best_prio)
                {
                    best_prio = (*t).priority;
                    best = t;
                }
                t = (*t).all_next;
            }
        }

        if !best.is_null() {
            return best;
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

        let current = crate::CURRENT;

        // Guard: scheduler not yet started (svc_first_task_sp has not run yet).
        if current.is_null() || !matches!((*current).state, TaskState::Running { .. }) {
            return false;
        }

        // cur_prio hoisted before the fused loop so it is available inside it.
        let cur_prio = (*current).priority;
        let mut should_preempt = false;

        // Fused single pass: wake sleeping tasks whose deadline has passed, and
        // simultaneously check whether any ready task has higher priority than
        // the current one. Skip `current` to avoid aliased mutable access.
        let mut t = crate::ALL_TASKS;
        while !t.is_null() {
            if t == current {
                t = (*t).all_next;
                continue;
            }
            if let TaskState::Sleeping(deadline) = (*t).state
                && now >= deadline
            {
                (*t).state = TaskState::Ready { slice_remaining: (*t).time_slice };
            }
            // Covers both freshly-woken sleepers and tasks already ready.
            if matches!((*t).state, TaskState::Ready { .. }) && (*t).priority < cur_prio {
                should_preempt = true;
                // Do not break — remaining sleepers must still be woken.
            }
            t = (*t).all_next;
        }

        if should_preempt {
            return true;
        }

        // Decrement the running task's time slice.
        let cur = &mut *current;
        let expired = if let TaskState::Running { ref mut slice_remaining } = cur.state {
            if *slice_remaining > 0 {
                *slice_remaining -= 1;
            }
            if *slice_remaining == 0 {
                *slice_remaining = cur.time_slice;
                true
            } else {
                false
            }
        } else {
            false
        };

        // On slice expiry, rotate if an equal-priority peer is ready.
        if expired {
            let mut t = crate::ALL_TASKS;
            while !t.is_null() {
                if t != current
                    && matches!((*t).state, TaskState::Ready { .. })
                    && (*t).priority == cur_prio
                {
                    return true;
                }
                t = (*t).all_next;
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
        (*crate::CURRENT).state = TaskState::Blocked;
    }
}

/// Mark `tcb` as `Ready` and return whether PendSV should fire.
///
/// Returns `true` when the newly-ready task has a higher priority (lower
/// number) than the currently running task, indicating that preemption is
/// warranted. The caller must call `SCB::set_pendsv()` in that case.
///
/// # Safety
/// Must be called inside `interrupt::free`.
pub(crate) unsafe fn unblock(tcb: *mut Tcb) -> bool {
    unsafe {
        (*tcb).state = TaskState::Ready { slice_remaining: (*tcb).time_slice };
        (*tcb).priority < (*crate::CURRENT).priority
    }
}

/// Prepend `tcb` to the intrusive wait list rooted at `*head`.
///
/// O(1). The list is LIFO at insertion; priority ordering is enforced on
/// removal by `wait_list_pop_highest`.
///
/// # Safety
/// Must be called inside `interrupt::free`.
pub(crate) unsafe fn wait_list_push(head: &mut *mut Tcb, tcb: *mut Tcb) {
    unsafe {
        (*tcb).wait_next = *head;
        *head = tcb;
    }
}

/// Remove and return the highest-priority (lowest `priority` value) task
/// from the wait list rooted at `*head`.
///
/// O(N waiters). Returns `null` if the list is empty.
///
/// # Safety
/// Must be called inside `interrupt::free`.
pub(crate) unsafe fn wait_list_pop_highest(head: &mut *mut Tcb) -> *mut Tcb {
    if (*head).is_null() {
        return core::ptr::null_mut();
    }

    unsafe {
        // Walk the list to find the entry with the smallest priority value.
        let mut best = *head;
        let mut best_prio = (*best).priority;
        let mut cur = (*best).wait_next;
        while !cur.is_null() {
            let p = (*cur).priority;
            if p < best_prio {
                best_prio = p;
                best = cur;
            }
            cur = (*cur).wait_next;
        }

        // Unlink `best` from the list.
        if *head == best {
            *head = (*best).wait_next;
        } else {
            let mut prev = *head;
            loop {
                let next = (*prev).wait_next;
                if next == best {
                    (*prev).wait_next = (*best).wait_next;
                    break;
                }
                prev = next;
            }
        }

        (*best).wait_next = core::ptr::null_mut();
        best
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
/// pointer in `CURRENT`, and returns its SP so the assembly performs the
/// first `EXC_RETURN` into task context.
#[unsafe(no_mangle)]
#[cfg(not(armv8m))]
unsafe extern "C" fn svc_first_task_sp() -> usize {
    // SAFETY: called from SVCall (Handler mode) before the scheduler starts.
    // Single-core Cortex-M: no concurrent mutation of ALL_TASKS/CURRENT is
    // possible while we are in Handler mode.
    unsafe {
        let mut best_prio = u8::MAX;
        let mut best: *mut Tcb = core::ptr::null_mut();

        let mut t = crate::ALL_TASKS;
        while !t.is_null() {
            if matches!((*t).state, TaskState::Ready { .. }) && (*t).priority < best_prio {
                best_prio = (*t).priority;
                best = t;
            }
            t = (*t).all_next;
        }

        crate::CURRENT = best;
        let TaskState::Ready { slice_remaining } = (*best).state else { unreachable!() };
        (*best).state = TaskState::Running { slice_remaining };
        (*best).sp
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
    // Single-core Cortex-M: exclusive access to ALL_TASKS/CURRENT is
    // guaranteed — no other Handler-mode code runs concurrently, and
    // Thread-mode code only touches these globals inside interrupt::free.
    unsafe {
        let old = crate::CURRENT;
        (*old).sp = current_sp;

        if let TaskState::Running { slice_remaining } = (*old).state {
            (*old).state = TaskState::Ready { slice_remaining };
        }

        let next = find_next();
        crate::CURRENT = next;
        let TaskState::Ready { slice_remaining } = (*next).state else { unreachable!() };
        (*next).state = TaskState::Running { slice_remaining };
        (*next).sp
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(unused_assignments)]
mod tests {
    use super::{TICK, find_next, tick};
    use crate::kernel::tcb::{TaskState, Tcb};
    use std::sync::Mutex;

    // All scheduler tests share global mutable state (ALL_TASKS, CURRENT,
    // TASK_COUNT, TICK).  Serialize them with a single lock so parallel
    // test threads do not corrupt each other's task arrays.
    static SCHED_LOCK: Mutex<()> = Mutex::new(());

    // Set up a stack-allocated task array and register it with the kernel
    // globals.  Returns a guard that resets the globals on drop.
    macro_rules! with_tasks {
        ($tasks:ident, $count:expr, $current:expr) => {
            let mut $tasks: [Tcb; $count] = core::array::from_fn(|_| Tcb::zeroed());
            let _guard = {
                unsafe {
                    // Build the all_next intrusive linked list through the array.
                    for i in 0..$count - 1 {
                        $tasks[i].all_next = core::ptr::addr_of_mut!($tasks[i + 1]);
                    }
                    crate::ALL_TASKS = core::ptr::addr_of_mut!($tasks[0]);
                    crate::CURRENT = core::ptr::addr_of_mut!($tasks[$current]);
                }
                // Return a guard that clears the globals when dropped.
                struct Guard;
                impl Drop for Guard {
                    fn drop(&mut self) {
                        unsafe {
                            crate::ALL_TASKS = core::ptr::null_mut();
                            crate::CURRENT = core::ptr::null_mut();
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
        tasks[0].state = TaskState::Running { slice_remaining: 0 };
        tasks[0].priority = 5;
        tasks[1].state = TaskState::Ready { slice_remaining: 0 };
        tasks[1].priority = 3; // wins
        tasks[2].state = TaskState::Blocked;
        tasks[2].priority = 1; // blocked
        assert_eq!(find_next(), core::ptr::addr_of_mut!(tasks[1]));
    }

    #[test]
    fn find_next_round_robin_equal_priority() {
        let _lock = SCHED_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        with_tasks!(tasks, 3, 0);
        tasks[0].state = TaskState::Running { slice_remaining: 0 };
        tasks[0].priority = 5;
        tasks[1].state = TaskState::Ready { slice_remaining: 0 };
        tasks[1].priority = 5;
        tasks[2].state = TaskState::Ready { slice_remaining: 0 };
        tasks[2].priority = 5;
        // Search starts at current+1 = 1, so task 1 is the round-robin winner.
        assert_eq!(find_next(), core::ptr::addr_of_mut!(tasks[1]));
    }

    #[test]
    fn find_next_skips_non_ready() {
        let _lock = SCHED_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        with_tasks!(tasks, 3, 0);
        tasks[0].state = TaskState::Running { slice_remaining: 0 };
        tasks[0].priority = 5;
        tasks[1].state = TaskState::Blocked;
        tasks[1].priority = 0; // highest prio but blocked
        tasks[2].state = TaskState::Ready { slice_remaining: 0 };
        tasks[2].priority = 5;
        // Only task 2 is ready → it must be selected despite lower prio than task 1.
        assert_eq!(find_next(), core::ptr::addr_of_mut!(tasks[2]));
    }

    #[test]
    fn tick_wakes_sleeping_task_and_requests_preempt() {
        let _lock = SCHED_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        with_tasks!(tasks, 2, 0);
        tasks[0].state = TaskState::Running { slice_remaining: 10 };
        tasks[0].priority = 5;
        tasks[0].time_slice = 10;
        tasks[1].state = TaskState::Sleeping(5);
        tasks[1].priority = 3; // higher prio
        unsafe {
            TICK = 4;
        } // will become 5 after tick() increments
        let preempt = tick(); // TICK → 5 → wakes task 1 → preempt needed
        assert!(
            preempt,
            "expected preemption when a higher-prio task is woken"
        );
        assert!(matches!(tasks[1].state, TaskState::Ready { .. }));
    }

    #[test]
    fn tick_does_not_preempt_when_lower_prio_sleeper_wakes() {
        let _lock = SCHED_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        with_tasks!(tasks, 2, 0);
        tasks[0].state = TaskState::Running { slice_remaining: 10 };
        tasks[0].priority = 2; // higher prio
        tasks[0].time_slice = 10;
        tasks[1].state = TaskState::Sleeping(5);
        tasks[1].priority = 5; // lower prio
        unsafe {
            TICK = 4;
        }
        let preempt = tick();
        assert!(!preempt, "lower-prio wakeup must not trigger preemption");
        assert!(matches!(tasks[1].state, TaskState::Ready { .. }));
    }

    #[test]
    fn tick_slice_expiry_rotates_equal_prio() {
        let _lock = SCHED_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        with_tasks!(tasks, 2, 0);
        tasks[0].state = TaskState::Running { slice_remaining: 1 };
        tasks[0].priority = 5;
        tasks[0].time_slice = 1;
        tasks[1].state = TaskState::Ready { slice_remaining: 0 };
        tasks[1].priority = 5; // same prio peer
        unsafe {
            TICK = 0;
        }
        let preempt = tick(); // slice expires → rotate
        assert!(preempt);
    }

    #[test]
    fn tick_increments_global_counter() {
        let _lock = SCHED_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        with_tasks!(tasks, 1, 0);
        tasks[0].state = TaskState::Running { slice_remaining: 100 };
        tasks[0].priority = 5;
        tasks[0].time_slice = 100;
        unsafe {
            TICK = 999;
        }
        tick();
        assert_eq!(unsafe { TICK }, 1000);
    }
}
