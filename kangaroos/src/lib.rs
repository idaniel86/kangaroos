#![no_std]

pub mod arch;
pub mod channel;
pub(crate) mod port;
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

/// Declare a named static [`sync::Semaphore`].
///
/// ```ignore
/// semaphore!(SEM, 0, 1); // static SEM: Semaphore named "SEM"
/// ```
#[macro_export]
macro_rules! semaphore {
    ($var:ident, $initial:expr, $max:expr) => {
        static $var: $crate::sync::Semaphore =
            $crate::sync::Semaphore::new_named($initial, $max, stringify!($var));
    };
}

/// Declare a named static [`sync::Mutex`].
///
/// ```ignore
/// mutex!(COUNTER, u32, 0); // static COUNTER: Mutex<u32> named "COUNTER"
/// ```
#[macro_export]
macro_rules! mutex {
    ($var:ident, $ty:ty, $data:expr) => {
        static $var: $crate::sync::Mutex<$ty> =
            $crate::sync::Mutex::new_named($data, stringify!($var));
    };
}

/// Declare a named static [`sync::Condvar`].
///
/// ```ignore
/// condvar!(CV); // static CV: Condvar named "CV"
/// ```
#[macro_export]
macro_rules! condvar {
    ($var:ident) => {
        static $var: $crate::sync::Condvar =
            $crate::sync::Condvar::new_named(stringify!($var));
    };
}

/// Declare a named static [`sync::EventGroup`].
///
/// ```ignore
/// event_group!(FLAGS); // static FLAGS: EventGroup named "FLAGS"
/// ```
#[macro_export]
macro_rules! event_group {
    ($var:ident) => {
        static $var: $crate::sync::EventGroup =
            $crate::sync::EventGroup::new_named(stringify!($var));
    };
}

/// Declare a named static [`sync::Once`].
///
/// ```ignore
/// once!(INIT); // static INIT: Once named "INIT"
/// ```
#[macro_export]
macro_rules! once {
    ($var:ident) => {
        static $var: $crate::sync::Once =
            $crate::sync::Once::new_named(stringify!($var));
    };
}

// Provide a real millisecond timestamp for defmt when the feature is enabled.
// TICK is incremented once per SysTick (1 kHz), so it equals ms since boot.
// Truncating to u32 wraps every ~49 days — acceptable for debug sessions.
#[cfg(all(feature = "defmt", target_arch = "arm"))]
defmt::timestamp!("{=u32}", {
    cortex_m::interrupt::free(|_| unsafe { kernel::scheduler::TICK as u32 })
});

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
