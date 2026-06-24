//! Public task-management API.
//!
//! These functions are safe to call from any task context. They must **not**
//! be called from interrupt handlers.

use crate::arch::ArchContext as _;
use crate::kernel::tcb::{TaskState, Tcb};

/// Register a task with the given static priority (`0` = highest).
///
/// The stack slice must have a `'static` lifetime (i.e. come from a
/// `static mut` array). Safe to call both before `kernel_start` and after
/// the scheduler is running (e.g. from another task).
pub fn spawn(stack: &'static mut [u32], priority: u8, time_slice: u8, entry: fn() -> !) {
    cortex_m::interrupt::free(|_| unsafe {
        let idx = crate::TASK_COUNT;
        assert!(idx < crate::MAX_TASKS, "maximum task count exceeded");

        crate::arch::Arch::canary_init(stack);
        let sp = crate::arch::Arch::stack_init(stack, entry);
        let stack_base = stack.as_ptr() as usize;

        crate::TASKS[idx] = Tcb {
            sp,
            state: TaskState::Ready,
            priority,
            time_slice,
            slice_remaining: time_slice,
            stack_base,
            name: "",
        };

        // Release fence: all stores above must be visible to PendSV before
        // it can observe the incremented TASK_COUNT.
        core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
        crate::TASK_COUNT += 1;
    });
}

/// Voluntarily yield the CPU to the next runnable task.
///
/// Resets the calling task's time slice to zero so the scheduler treats it
/// as having used its quantum, then raises PendSV. If no other task at the
/// same or higher priority is ready, the calling task is re-scheduled
/// immediately.
pub fn yield_now() {
    cortex_m::interrupt::free(|_| unsafe {
        crate::TASKS[crate::CURRENT_TASK].slice_remaining = 0;
    });
    cortex_m::peripheral::SCB::set_pendsv();
}

/// Return the static priority of the currently running task.
///
/// Lower numbers represent higher priority; `0` is the highest, `u8::MAX`
/// is reserved for the idle task.
pub fn current_priority() -> u8 {
    // SAFETY: CURRENT_TASK is only mutated by PendSV (Handler mode); this
    // read is effectively atomic on single-core Cortex-M.
    unsafe { crate::TASKS[crate::CURRENT_TASK].priority }
}

/// Terminate the current task.
///
/// Marks the task as permanently `Blocked`, removing it from the run queues,
/// then triggers a context switch. This function never returns.
///
/// Under normal RTOS usage tasks are infinite loops and never need to call
/// this; it is provided for completeness and one-shot task patterns.
pub fn exit() -> ! {
    cortex_m::interrupt::free(|_| unsafe {
        crate::TASKS[crate::CURRENT_TASK].state = TaskState::Blocked;
    });
    cortex_m::peripheral::SCB::set_pendsv();
    loop {
        cortex_m::asm::wfi();
    }
}
