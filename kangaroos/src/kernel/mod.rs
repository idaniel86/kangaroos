pub(crate) mod idle;
pub(crate) mod scheduler;
pub(crate) mod tcb;

use crate::arch::ArchContext as _;
use cortex_m::peripheral::scb::SystemHandler;
use tcb::Tcb;

/// Kernel instance — owns the task-control-block array.
///
/// Declare a single `static mut` in your application and pass it to every
/// `task::spawn` call and to `kernel::start`:
///
/// ```ignore
/// static mut KERNEL: Kernel<8> = Kernel::new();
/// ```
pub struct Kernel<const N: usize> {
    pub(crate) tasks: [Tcb; N],
}

impl<const N: usize> Kernel<N> {
    /// Construct a kernel with all task slots uninitialised.
    /// `const fn` so it can initialise a `static`.
    pub const fn new() -> Self {
        Kernel {
            tasks: [Tcb::zeroed(); N],
        }
    }

    /// Configure SysTick + PendSV priorities, register the idle task, and
    /// launch the first task. This function never returns.
    ///
    /// `cpu_freq_hz` is used to program a 1 ms SysTick period.
    pub fn start(&mut self, cpu_freq_hz: u32) -> ! {
        unsafe {
            assert!(crate::TASK_COUNT > 0, "no tasks — call task::spawn first");

            // Publish the task-array pointer so interrupt handlers can reach it.
            crate::TASKS_PTR = self.tasks.as_mut_ptr();
            crate::MAX_TASKS = N;

            // Register the always-ready idle task at the lowest priority.
            idle::register(self);

            // Enable the FPU on Cortex-M4F / M7 by granting full access to
            // CP10 and CP11 in the Coprocessor Access Control Register.
            // Must be done before any VFP instruction (including vstmdb in PendSV).
            #[cfg(has_fpu)]
            {
                const CPACR: *mut u32 = 0xE000_ED88 as *mut u32;
                CPACR.write_volatile(CPACR.read_volatile() | (0xF << 20));
                cortex_m::asm::dsb();
                cortex_m::asm::isb();
            }

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

            // Fire SVC #0 to enter Handler mode; the SVCall handler performs the
            // EXC_RETURN that launches the first task. EXC_RETURN from Thread
            // mode is UNPREDICTABLE on ARMv7-M — Handler mode is required.
            core::arch::asm!("svc #0", options(nomem, nostack));

            // Unreachable: SVCall returns to the first task via EXC_RETURN.
            loop {
                cortex_m::asm::wfi();
            }
        }
    }
}

/// Drive the scheduler from the SysTick exception handler.
///
/// ```rust
/// #[exception]
/// fn SysTick() { kernel::systick_handler(); }
/// ```
pub fn systick_handler() {
    if scheduler::tick() {
        cortex_m::peripheral::SCB::set_pendsv();
    }

    // Verify stack canaries for all live tasks.
    unsafe {
        for i in 0..crate::TASK_COUNT {
            if !crate::arch::Arch::canary_check(crate::ktask(i).stack_base) {
                // Stack overflow detected — halt with a debugger trap.
                loop {
                    cortex_m::asm::bkpt();
                }
            }
        }
    }
}
