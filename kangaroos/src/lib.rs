#![no_std]

pub mod arch;
pub mod kernel;
pub mod task;

use kernel::tcb::Tcb;

pub(crate) const MAX_TASKS: usize = 8;

// Safety: single-core; accesses are either before the scheduler starts
// (Thread mode, no preemption yet) or inside PendSV (interrupts masked by
// the processor's exception-priority mechanism).
pub(crate) static mut TASKS: [Tcb; MAX_TASKS] = [Tcb::zeroed(); MAX_TASKS];
pub(crate) static mut TASK_COUNT: usize = 0;
pub(crate) static mut CURRENT_TASK: usize = 0;
