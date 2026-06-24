/// Stack for the idle task. 64 words (256 bytes) is ample for a WFI loop.
static mut IDLE_STACK: [u32; 64] = [0; 64];

/// Idle task body: entered when no user task is runnable.
///
/// Executes `wfi` to halt the CPU until the next interrupt, minimising power
/// consumption during idle periods.
fn idle_task() -> ! {
    loop {
        cortex_m::asm::wfi();
    }
}

/// Register the idle task with the kernel.
///
/// Called once by `kernel_start` after all user tasks have been spawned.
/// The idle task runs at the lowest possible priority (`u8::MAX`) and is
/// always `Ready`, ensuring the scheduler always has a task to run.
pub(crate) fn register<const N: usize>(kernel: &mut super::Kernel<N>) {
    // SAFETY: IDLE_STACK is a module-level static accessed only here (once,
    // before interrupts are enabled) and then owned by the idle task forever.
    let stack = unsafe { &mut *core::ptr::addr_of_mut!(IDLE_STACK) };
    crate::task::spawn(kernel, stack, u8::MAX, 1, idle_task);
}
