#[cfg(target_arch = "arm")]
pub(crate) mod idle;
pub(crate) mod scheduler;
pub(crate) mod tcb;

use crate::arch::ArchContext as _;
#[cfg(target_arch = "arm")]
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

impl<const N: usize> Default for Kernel<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> Kernel<N> {
    /// Construct a kernel with all task slots uninitialised.
    /// `const fn` so it can initialise a `static`.
    ///
    /// # Panics (compile time)
    /// Panics if `N > 254`. The intrusive wait-lists use `u8` indices with
    /// `0xFF` (255) as the empty-list sentinel; allowing index 255 would
    /// make task 255 indistinguishable from "no task".
    pub const fn new() -> Self {
        assert!(
            N <= 254,
            "Kernel<N>: N must be \u{2264} 254 (0xFF is the wait-list sentinel)"
        );
        Kernel {
            tasks: [Tcb::zeroed(); N],
        }
    }

    /// Configure SysTick + PendSV priorities, register the idle task, and
    /// launch the first task. This function never returns.
    ///
    /// `cpu_freq_hz` is used to program a 1 ms SysTick period.
    #[cfg(target_arch = "arm")]
    pub fn start(&mut self, cpu_freq_hz: u32) -> ! {
        unsafe {
            #[cfg(feature = "defmt")]
            {
                let n = crate::TASK_COUNT;
                defmt::info!(
                    "kernel: starting, initial tasks={=usize} cpu={=u32}Hz",
                    n,
                    cpu_freq_hz
                );
            }

            // Publish the task-array pointer so interrupt handlers can reach it.
            crate::TASKS_PTR = self.tasks.as_mut_ptr();

            // Register the always-ready idle task at the lowest priority.
            idle::register(self);

            // Enable the FPU on Cortex-M4F / M7 by granting full access to
            // CP10 and CP11 in the Coprocessor Access Control Register.
            // Must be done before any VFP instruction (including vstmdb in PendSV).
            #[cfg(has_fpu)]
            {
                #[cfg(feature = "defmt")]
                defmt::debug!("kernel: FPU enabled");
                const CPACR: *mut u32 = 0xE000_ED88 as *mut u32;
                CPACR.write_volatile(CPACR.read_volatile() | (0xF << 20));
                cortex_m::asm::dsb();
                cortex_m::asm::isb();
            }

            let mut p = cortex_m::Peripherals::steal();

            // PendSV: lowest effective priority on every Cortex-M variant.
            // Writing 0xFF sets all implemented priority bits to 1; unimplemented
            // bits are RAZ/WI, so the effective value is always the lowest
            // possible level (0xC0 on 2-bit ARMv6-M, 0xE0 on 3-bit, 0xF0 on
            // 4-bit, 0xFF on 8-bit). PendSV therefore never splits a user ISR.
            p.SCB.set_priority(SystemHandler::PendSV, 0xFF);

            // SysTick: priority 0 (highest on every Cortex-M profile).
            // 0x00 is always maximum precedence regardless of how many priority
            // bits the chip implements, keeping the tick non-interruptable by
            // any user ISR. Together 0x00 and 0xFF are guaranteed distinct on
            // every Cortex-M variant (unlike the old 0xFE/0xFF pair which
            // collapsed to the same effective level on chips with ≤3 bits).
            //
            // PRIMASK interaction: `cortex_m::interrupt::free` sets PRIMASK=1
            // (CPSID I), which blocks ALL exceptions with configurable priority
            // — including SysTick — regardless of the numeric priority value.
            // Only NMI and HardFault (fixed priority −2/−1) are immune to
            // PRIMASK. This means short critical sections (e.g. reading the
            // 64-bit TICK counter) still correctly gate SysTick even at
            // priority 0x00. Ticks that arrive while PRIMASK=1 are pended and
            // execute immediately after the critical section exits; no ticks
            // are lost, but they may be delayed by the critical section length.
            //
            // Constraint for application code: any user ISR that calls into
            // the kernel API must be configured with a priority value strictly
            // greater than 0x00 (numerically), i.e. lower precedence than
            // SysTick. An ISR at priority 0x00 would be at the same level as
            // SysTick and could not be preempted by it (and vice versa).
            p.SCB.set_priority(SystemHandler::SysTick, 0x00);

            // 1 ms tick.
            let reload = cpu_freq_hz / 1000 - 1;
            p.SYST.set_reload(reload);
            p.SYST.clear_current();
            p.SYST
                .set_clock_source(cortex_m::peripheral::syst::SystClkSource::Core);
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
/// ```rust,ignore
/// #[exception]
/// fn SysTick() { kernel::systick_handler(); }
/// ```
pub fn systick_handler() {
    if scheduler::tick() {
        crate::port::trigger_pendsv();
    }

    // Verify stack canaries for all live tasks (skip Dead and Uninit slots).
    unsafe {
        for i in 0..crate::TASK_COUNT {
            if matches!(
                crate::ktask(i).state,
                crate::kernel::tcb::TaskState::Dead | crate::kernel::tcb::TaskState::Uninit
            ) {
                continue;
            }
            if !crate::arch::Arch::canary_check(crate::ktask(i).stack_base) {
                #[cfg(not(feature = "defmt"))]
                panic!("stack overflow");
                #[cfg(feature = "defmt")]
                defmt::panic!("kernel: stack overflow in task '{}'", crate::ktask(i).name);
            }
        }
    }
}
