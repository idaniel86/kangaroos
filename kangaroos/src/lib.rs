#![no_std]

pub mod arch;

use arch::ArchContext as _;
use cortex_m::peripheral::scb::SystemHandler;

const MAX_TASKS: usize = 8;

#[derive(Copy, Clone)]
#[repr(C)]
pub(crate) struct Tcb {
    pub(crate) sp: usize,
}

// Safety: single-core; accesses are either before the scheduler starts
// (Thread mode, no preemption yet) or inside PendSV (interrupts masked by
// the processor's exception priority mechanism).
pub(crate) static mut TASKS: [Tcb; MAX_TASKS] = [Tcb { sp: 0 }; MAX_TASKS];
pub(crate) static mut TASK_COUNT: usize = 0;
pub(crate) static mut CURRENT_TASK: usize = 0;

/// Register a task. Safe to call both before `kernel_start` and after the
/// scheduler is running (e.g. from another task).
///
/// The stack slice must live for `'static` (i.e. come from a `static mut`
/// array).  Tasks are added to the round-robin ring and will be scheduled on
/// the next context switch after registration.
pub fn spawn_task(stack: &'static mut [u32], entry: fn() -> !) {
    // Disable all maskable interrupts for the duration so that PendSV cannot
    // fire between stack_init and the TASK_COUNT increment, and so that two
    // concurrent callers cannot both read the same TASK_COUNT slot.
    cortex_m::interrupt::free(|_| unsafe {
        let idx = TASK_COUNT;
        assert!(idx < MAX_TASKS, "maximum task count exceeded");
        TASKS[idx].sp = arch::Arch::stack_init(stack, entry);
        // Release fence: all stack_init stores must be visible to PendSV
        // before it can observe the incremented TASK_COUNT.
        core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
        TASK_COUNT += 1;
    });
}

/// Configure priorities, start SysTick, and launch the first task.
///
/// `cpu_freq_hz` is used to program a 1 ms SysTick period.
/// This function never returns.
pub fn kernel_start(cpu_freq_hz: u32) -> ! {
    unsafe {
        assert!(TASK_COUNT > 0, "no tasks registered — call spawn_task first");

        let mut p = cortex_m::Peripherals::steal();

        // PendSV must be the absolute lowest priority so it never splits a user ISR.
        p.SCB.set_priority(SystemHandler::PendSV, 0xFF);

        // SysTick one level above PendSV: fires the tick, then triggers PendSV.
        p.SCB.set_priority(SystemHandler::SysTick, 0xFE);

        // 1 ms tick.
        let reload = cpu_freq_hz / 1000 - 1;
        p.SYST.set_reload(reload);
        p.SYST.clear_current();
        p.SYST.set_clock_source(cortex_m::peripheral::syst::SystClkSource::Core);
        p.SYST.enable_interrupt();
        p.SYST.enable_counter();

        // Fire SVC #0 to enter Handler mode; the SVCall handler does the
        // EXC_RETURN that launches the first task.  EXC_RETURN from Thread
        // mode is UNPREDICTABLE on ARMv7-M — Handler mode is required.
        core::arch::asm!("svc #0", options(nomem, nostack));

        // Unreachable: SVCall returns to task_a via EXC_RETURN, not here.
        loop {
            cortex_m::asm::wfi();
        }
    }
}

/// Must be called from the SysTick exception handler to trigger PendSV.
///
/// ```rust
/// #[exception]
/// fn SysTick() { kangaroos::systick_handler(); }
/// ```
pub fn systick_handler() {
    cortex_m::peripheral::SCB::set_pendsv();
}