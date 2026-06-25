#![no_std]

pub mod arch;
pub mod channel;
pub(crate) mod kernel;
pub mod mem;
pub mod sync;
pub mod task;
pub mod timer;

// Re-export proc macros so applications need only `kangaroos` as a dependency.
pub use kangaroos_macros::{main, task};

use kernel::tcb::Tcb;

// Re-export the kernel type so applications can declare a `static mut` instance.
pub use kernel::{Kernel, systick_handler};

// Re-export the spawn API so applications need only `use kangaroos::Spawner`.
pub use task::{Spawner, SpawnToken};

// Re-export Phase 6 extended sync primitives at the crate root.
pub use sync::{Condvar, EventGroup};

// Re-export Phase 7 pool allocator at the crate root.
pub use mem::Pool;

// Global state referenced by PendSV and SysTick handlers.
// `TASKS_PTR` and `MAX_TASKS` are set once by `kernel::start` before
// interrupts fire; `TASK_COUNT` and `CURRENT_TASK` are updated by PendSV.
//
// Safety: single-core; accessed from Handler mode (PendSV/SysTick) or
// with interrupts disabled (task::spawn / task::yield_now).
pub(crate) static mut TASKS_PTR: *mut Tcb = core::ptr::null_mut();
pub(crate) static mut MAX_TASKS: usize = 0;
pub(crate) static mut TASK_COUNT: usize = 0;
pub(crate) static mut CURRENT_TASK: usize = 0;

/// Return a mutable reference to the task at slot `i`.
///
/// # Safety
/// `TASKS_PTR` must be initialised (`kernel::start` has been called) and
/// `i` < `TASK_COUNT`.
#[inline(always)]
pub(crate) unsafe fn ktask(i: usize) -> &'static mut Tcb {
    unsafe { &mut *TASKS_PTR.add(i) }
}
