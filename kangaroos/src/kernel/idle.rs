use crate::kernel::storage::TaskStorage;

/// Storage for the idle task: 64 words (256 bytes) is ample for a WFI loop.
static mut IDLE_STORAGE: TaskStorage<64> = TaskStorage::new();

/// Idle task body: entered when no user task is runnable.
///
/// Executes `wfi` to halt the CPU until the next interrupt, minimising power
/// consumption during idle periods.
fn idle_task() -> ! {
    loop {
        crate::port::wfi();
    }
}

/// Register the idle task with the kernel.
///
/// Called once by `kernel::start` after all user tasks have been spawned.
/// The idle task runs at the lowest possible priority (`u8::MAX`) and is
/// always `Ready`, ensuring the scheduler always has a task to run.
pub(crate) fn register() {
    // SAFETY: IDLE_STORAGE is a module-level static accessed only here (once,
    // before interrupts are enabled) and then owned by the idle task forever.
    let storage = unsafe { &mut *core::ptr::addr_of_mut!(IDLE_STORAGE) };
    let tcb_ptr = storage.tcb_ptr();
    let stack = storage.stack_slice();
    let stack_ptr = stack.as_mut_ptr();
    let stack_len = stack.len();
    unsafe {
        crate::task::spawn_into(tcb_ptr, stack_ptr, stack_len, u8::MAX, 1, idle_task, "idle");
    }
}
