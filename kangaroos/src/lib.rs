#![no_std]

// The test harness links libstd unconditionally. Bringing it into the
// name-space here lets every `#[cfg(test)]` module in this crate use
// `std::sync::Mutex` etc. without repeating the declaration.
#[cfg(test)]
extern crate std;

pub mod arch;
pub mod channel;
pub(crate) mod kernel;
pub mod mem;
pub(crate) mod port;
pub mod sync;
pub mod task;
pub mod timer;

// Re-export proc macros so applications need only `kangaroos` as a dependency.
pub use kangaroos_macros::{main, task};

// Re-export kernel helpers at the crate root.
pub use kernel::{TaskStorage, systick_handler};
// Re-export Tcb so that #[task]-generated code (which calls TaskStorage::tcb_ptr())
// can name the return type from outside the crate. `#[doc(hidden)]` keeps it off
// the public docs while remaining part of the stable proc-macro ABI.
#[cfg(target_arch = "arm")]
pub use kernel::start;
#[doc(hidden)]
pub use kernel::tcb::Tcb;

// Re-export the spawn API so applications need only `use kangaroos::Spawner`.
pub use task::{SpawnToken, Spawner};

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
        static $var: $crate::sync::Condvar = $crate::sync::Condvar::new_named(stringify!($var));
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
        static $var: $crate::sync::Once = $crate::sync::Once::new_named(stringify!($var));
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
//
// - `CURRENT`    — pointer to the currently running TCB; updated by PendSV.
// - `ALL_TASKS`  — head of the all_next intrusive list; prepended by spawn_into.
// - `TASK_COUNT` — number of spawned tasks (including idle).
//
// Safety: single-core; accessed from Handler mode (PendSV/SysTick) or
// with interrupts disabled (task::spawn_into / task::yield_now).
pub(crate) static mut CURRENT: *mut Tcb = core::ptr::null_mut();
pub(crate) static mut ALL_TASKS: *mut Tcb = core::ptr::null_mut();
