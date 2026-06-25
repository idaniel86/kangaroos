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
