//! Public task-management API.
//!
//! These functions are safe to call from any task context. They must **not**
//! be called from interrupt handlers.

use crate::arch::ArchContext as _;
use crate::kernel::{
    Kernel,
    tcb::{TaskState, Tcb},
};

// ---------------------------------------------------------------------------
// SpawnToken + Spawner — Embassy-style spawn API
// ---------------------------------------------------------------------------

/// A task ready to be registered with the kernel.
///
/// Produced by calling a `#[kangaroos::task]`-annotated function with its
/// arguments. Pass the token to [`Spawner::spawn`] inside `#[kangaroos::main]`.
pub struct SpawnToken {
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
        stack_ptr: *mut u32,
        stack_len: usize,
        priority: u8,
        time_slice: u8,
        entry: fn() -> !,
        name: &'static str,
    ) -> Self {
        SpawnToken {
            stack_ptr,
            stack_len,
            priority,
            time_slice,
            entry,
            name,
        }
    }
}

/// Registers tasks with the kernel before it starts.
///
/// Injected as a parameter by `#[kangaroos::main]`. Call [`Spawner::spawn`]
/// once per task:
///
/// ```rust,ignore
/// #[kangaroos::main(cpu_hz = 8_000_000, max_tasks = 3)]
/// fn main(spawner: &mut Spawner) {
///     spawner.spawn(heartbeat());
///     spawner.spawn(blink(5, 500));
/// }
/// ```
pub struct Spawner {
    tasks_ptr: *mut Tcb,
    max_tasks: usize,
}

// SAFETY: Spawner is only used in `fn main()` before any ISR fires.
unsafe impl Send for Spawner {}

impl Spawner {
    /// Create a `Spawner` from a mutable kernel reference.
    /// Called by `#[main]`-generated code; not intended for direct use.
    #[doc(hidden)]
    pub fn new<const N: usize>(kernel: &mut Kernel<N>) -> Self {
        Spawner {
            tasks_ptr: kernel.tasks.as_mut_ptr(),
            max_tasks: N,
        }
    }

    /// Register one task. Panics if the kernel task slots are full.
    pub fn spawn(&mut self, token: SpawnToken) {
        // SAFETY: tasks_ptr comes from Kernel<N>.tasks and remains valid for
        // the lifetime of the kernel (static). Token fields are validated by
        // the #[task] macro generator.
        unsafe {
            spawn_into(
                self.tasks_ptr,
                self.max_tasks,
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

/// Low-level non-generic spawn used by [`Spawner`].
///
/// # Safety
/// `tasks_ptr` must point to an array of at least `max_tasks` [`Tcb`] slots.
/// `stack_ptr` must point to a `'static mut [u32]` of `stack_len` words.
#[allow(clippy::too_many_arguments)]
unsafe fn spawn_into(
    tasks_ptr: *mut Tcb,
    max_tasks: usize,
    stack_ptr: *mut u32,
    stack_len: usize,
    priority: u8,
    time_slice: u8,
    entry: fn() -> !,
    name: &'static str,
) {
    // Reconstruct the slice. Caller guarantees the memory is 'static.
    let stack = unsafe { core::slice::from_raw_parts_mut(stack_ptr, stack_len) };

    crate::port::interrupt_free(|| unsafe {
        let idx = crate::TASK_COUNT;
        assert!(idx < max_tasks, "maximum task count exceeded");

        crate::arch::Arch::canary_init(stack);
        let sp = crate::arch::Arch::stack_init(stack, entry);

        *tasks_ptr.add(idx) = Tcb {
            sp,
            state: TaskState::Ready,
            priority,
            base_priority: priority,
            time_slice,
            slice_remaining: time_slice,
            stack_base: stack_ptr as usize,
            name,
            wait_next: 0xFF,
            wait_ptr: 0,
        };

        core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
        crate::TASK_COUNT += 1;
        #[cfg(feature = "defmt")]
        defmt::info!(
            "task '{}': spawned, priority={=u8} stack={=usize}B",
            name,
            priority,
            stack_len * 4
        );
    });
}

///
/// The stack slice must have a `'static` lifetime (i.e. come from a
/// `static mut` array). Call this before `kernel::start`, passing the same
/// `Kernel<N>` instance.
pub fn spawn<const N: usize>(
    kernel: &mut Kernel<N>,
    stack: &'static mut [u32],
    priority: u8,
    time_slice: u8,
    entry: fn() -> !,
    name: &'static str,
) {
    crate::port::interrupt_free(|| unsafe {
        let idx = crate::TASK_COUNT;
        assert!(idx < N, "maximum task count exceeded");

        crate::arch::Arch::canary_init(stack);
        let sp = crate::arch::Arch::stack_init(stack, entry);
        let stack_base = stack.as_ptr() as usize;

        kernel.tasks[idx] = Tcb {
            sp,
            state: TaskState::Ready,
            priority,
            base_priority: priority,
            time_slice,
            slice_remaining: time_slice,
            stack_base,
            name,
            wait_next: 0xFF,
            wait_ptr: 0,
        };

        // Release fence: all stores above must be visible to PendSV before
        // it can observe the incremented TASK_COUNT.
        core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
        crate::TASK_COUNT += 1;
        #[cfg(feature = "defmt")]
        defmt::info!(
            "task '{}': spawned, priority={=u8} stack={=usize}B",
            name,
            priority,
            stack.len() * 4
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
        crate::ktask(crate::CURRENT_TASK).slice_remaining = 0;
    });
    #[cfg(feature = "defmt")]
    defmt::debug!("task '{}': yielding", unsafe {
        crate::ktask(crate::CURRENT_TASK).name
    });
    crate::port::trigger_pendsv();
}

/// Return the static priority of the currently running task.
///
/// Lower numbers represent higher priority; `0` is the highest, `u8::MAX`
/// is reserved for the idle task.
pub fn current_priority() -> u8 {
    // SAFETY: CURRENT_TASK is only mutated by PendSV (Handler mode); this
    // read is effectively atomic on single-core Cortex-M.
    unsafe { crate::ktask(crate::CURRENT_TASK).priority }
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
        unsafe { crate::ktask(crate::CURRENT_TASK).name },
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
        crate::ktask(crate::CURRENT_TASK).state = TaskState::Sleeping(deadline);
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
    defmt::debug!("task '{}': exiting", unsafe {
        crate::ktask(crate::CURRENT_TASK).name
    });
    crate::port::interrupt_free(|| unsafe {
        crate::ktask(crate::CURRENT_TASK).state = TaskState::Dead;
    });
    crate::port::trigger_pendsv();
    loop {
        crate::port::wfi();
    }
}
