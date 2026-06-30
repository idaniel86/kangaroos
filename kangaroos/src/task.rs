//! Public task-management API.
//!
//! These functions are safe to call from any task context. They must **not**
//! be called from interrupt handlers.

use crate::arch::ArchContext as _;
use crate::kernel::tcb::{TaskState, Tcb};

// ---------------------------------------------------------------------------
// SpawnToken + Spawner — Embassy-style spawn API
// ---------------------------------------------------------------------------

/// A task ready to be registered with the kernel.
///
/// Produced by calling a `#[kangaroos::task]`-annotated function with its
/// arguments. Pass the token to [`Spawner::spawn`] inside `#[kangaroos::main]`.
pub struct SpawnToken {
    tcb_ptr: *mut Tcb,
    stack_ptr: *mut u32,
    stack_len: usize, // in words
    priority: u8,
    time_slice: u8,
    entry: fn() -> !,
    name: &'static str,
}

// SAFETY: SpawnToken is only created in `fn main()` before any ISR fires,
// on a single-core device. The raw pointer is to a `'static` stack.
unsafe impl Send for SpawnToken {}
unsafe impl Sync for SpawnToken {}

impl SpawnToken {
    /// Construct a token. Called by `#[task]`-generated factory functions;
    /// not intended for direct use.
    #[doc(hidden)]
    pub fn new(
        tcb_ptr: *mut Tcb,
        stack_ptr: *mut u32,
        stack_len: usize,
        priority: u8,
        time_slice: u8,
        entry: fn() -> !,
        name: &'static str,
    ) -> Self {
        SpawnToken {
            tcb_ptr,
            stack_ptr,
            stack_len,
            priority,
            time_slice,
            entry,
            name,
        }
    }
}

/// Zero-sized spawn helper. Construct with the `Spawner` unit literal.
///
/// ```rust,ignore
/// #[kangaroos::main(cpu_hz = 8_000_000)]
/// fn main(spawner: Spawner) {
///     spawner.spawn(heartbeat());
///     spawner.spawn(blink(5, 500));
/// }
/// ```
pub struct Spawner;

// SAFETY: Spawner is only used in `fn main()` before any ISR fires.
unsafe impl Send for Spawner {}

impl Spawner {
    /// Register one task.
    pub fn spawn(&self, token: SpawnToken) {
        // SAFETY: token fields come from a 'static TaskStorage initialised
        // by a #[task]-generated factory function.
        unsafe {
            spawn_into(
                token.tcb_ptr,
                token.stack_ptr,
                token.stack_len,
                token.priority,
                token.time_slice,
                token.entry,
                token.name,
            );
        }
    }
}

/// Initialise the TCB at `tcb_ptr`, prepend it to `ALL_TASKS`, and increment
/// `TASK_COUNT`. Called by [`Spawner::spawn`] and `kernel::idle::register`.
///
/// # Safety
/// `tcb_ptr` must point to a valid `Tcb` inside a `'static`
/// [`crate::kernel::TaskStorage`]. `stack_ptr` must point to a
/// `'static mut [u32; stack_len]`.
pub(crate) unsafe fn spawn_into(
    tcb_ptr: *mut Tcb,
    stack_ptr: *mut u32,
    stack_len: usize,
    priority: u8,
    time_slice: u8,
    entry: fn() -> !,
    name: &'static str,
) {
    let stack = unsafe { core::slice::from_raw_parts_mut(stack_ptr, stack_len) };

    crate::port::interrupt_free(|| unsafe {
        crate::arch::Arch::canary_init(stack);
        let sp = crate::arch::Arch::stack_init(stack, entry);

        let tcb = &mut *tcb_ptr;
        tcb.sp = sp;
        tcb.state = TaskState::Ready { slice_remaining: time_slice };
        tcb.priority = priority;
        tcb.base_priority = priority;
        tcb.time_slice = time_slice;
        tcb.stack_base = stack_ptr as usize;
        tcb.name = name;
        tcb.wait_next = core::ptr::null_mut();
        tcb.wait_ptr = 0;

        // Prepend to ALL_TASKS intrusive list.
        tcb.all_next = crate::ALL_TASKS;
        crate::ALL_TASKS = tcb_ptr;

        core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
        #[cfg(feature = "defmt")]
        defmt::info!(
            "task '{}': spawned, priority={=u8} stack={=usize}B",
            name,
            priority,
            stack_len * 4
        );
    });
}

/// Voluntarily yield the CPU to the next runnable task.
///
/// Resets the calling task's time slice to zero so the scheduler treats it
/// as having used its quantum, then raises PendSV. If no other task at the
/// same or higher priority is ready, the calling task is re-scheduled
/// immediately.
pub fn yield_now() {
    crate::port::interrupt_free(|| unsafe {
        if let TaskState::Running { ref mut slice_remaining } = (*crate::CURRENT).state {
            *slice_remaining = 0;
        }
    });
    #[cfg(feature = "defmt")]
    defmt::debug!("task '{}': yielding", unsafe { (*crate::CURRENT).name });
    crate::port::trigger_pendsv();
}

/// Return the static priority of the currently running task.
///
/// Lower numbers represent higher priority; `0` is the highest, `u8::MAX`
/// is reserved for the idle task.
pub fn current_priority() -> u8 {
    // SAFETY: CURRENT_TASK is only mutated by PendSV (Handler mode); this
    // read is effectively atomic on single-core Cortex-M.
    unsafe { (*crate::CURRENT).priority }
}

/// Block the calling task for at least `duration`.
///
/// The task enters `Sleeping` state and is woken by the SysTick handler once
/// the global tick counter reaches `now + duration.as_ticks()`. A duration of
/// zero causes a yield to any equal-priority task but returns on the next tick.
pub fn sleep(duration: crate::timer::Duration) {
    // Read TICK inside a critical section: a bare u64 load is two 32-bit
    // instructions on Cortex-M; PRIMASK (set by interrupt::free) blocks
    // SysTick for the duration of the read, preventing a torn value.
    let deadline = crate::port::interrupt_free(|| unsafe {
        crate::kernel::scheduler::TICK.wrapping_add(duration.as_ticks())
    });
    #[cfg(feature = "defmt")]
    defmt::debug!(
        "task '{}': sleeping {=u64}ms",
        unsafe { (*crate::CURRENT).name },
        duration.as_millis()
    );
    sleep_until(deadline);
}

/// Block the calling task until the global tick counter reaches `deadline`.
///
/// Used by [`sleep`] and [`crate::timer::Timer::wait`] for drift-free periodic
/// scheduling. The caller supplies an absolute tick deadline rather than a
/// relative duration.
pub(crate) fn sleep_until(deadline: u64) {
    crate::port::interrupt_free(|| unsafe {
        (*crate::CURRENT).state = TaskState::Sleeping(deadline);
    });
    crate::port::trigger_pendsv();
}

/// Terminate the current task.
///
/// Marks the task as `Dead`, removing it permanently from all run queues,
/// canary checks, and sync-primitive wait lists, then triggers a context
/// switch. This function never returns.
///
/// Under normal RTOS usage tasks are infinite loops and never need to call
/// this; it is provided for completeness and one-shot task patterns.
pub fn exit() -> ! {
    #[cfg(feature = "defmt")]
    defmt::debug!("task '{}': exiting", unsafe { (*crate::CURRENT).name });
    crate::port::interrupt_free(|| unsafe {
        (*crate::CURRENT).state = TaskState::Dead;
    });
    crate::port::trigger_pendsv();
    loop {
        crate::port::wfi();
    }
}
