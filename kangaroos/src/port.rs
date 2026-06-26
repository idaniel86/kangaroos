// Thin hardware-abstraction wrappers over Cortex-M peripherals.
//
// On `target_arch = "arm"` the real `cortex-m` hardware is used.
// On every other target (host unit-test builds) these are no-op stubs so
// all scheduler and sync logic compiles and runs without a physical CPU.
//
// All call sites in the kernel use `crate::port::*`; no module except
// `kernel::mod` and `arch::*` may reach into `cortex_m` directly.

// ---------------------------------------------------------------------------
// interrupt_free — critical section
// ---------------------------------------------------------------------------

/// Execute `f` with interrupts disabled, returning its result.
///
/// On ARM this lowers to `cpsid i` / `cpsie i` (PRIMASK). On the host the
/// closure is called directly — the test environment is single-threaded so
/// no synchronisation is needed.
#[cfg(target_arch = "arm")]
#[inline(always)]
pub(crate) fn interrupt_free<T>(f: impl FnOnce() -> T) -> T {
    cortex_m::interrupt::free(|_| f())
}

#[cfg(not(target_arch = "arm"))]
#[inline(always)]
pub(crate) fn interrupt_free<T>(f: impl FnOnce() -> T) -> T {
    f()
}

// ---------------------------------------------------------------------------
// trigger_pendsv — request a context switch
// ---------------------------------------------------------------------------

/// Set the PendSV pending bit to trigger a context switch.
///
/// On the host this is a no-op; test code that drives the scheduler calls
/// `scheduler::find_next()` directly instead of relying on PendSV.
#[cfg(target_arch = "arm")]
#[inline(always)]
pub(crate) fn trigger_pendsv() {
    cortex_m::peripheral::SCB::set_pendsv();
}

#[cfg(not(target_arch = "arm"))]
#[inline(always)]
pub(crate) fn trigger_pendsv() {}

// ---------------------------------------------------------------------------
// wfi — wait for interrupt / idle
// ---------------------------------------------------------------------------

/// Execute a `WFI` instruction (power-saving idle).
///
/// On the host this is a no-op.
#[cfg(target_arch = "arm")]
#[inline(always)]
pub(crate) fn wfi() {
    cortex_m::asm::wfi();
}

#[cfg(not(target_arch = "arm"))]
#[inline(always)]
pub(crate) fn wfi() {}
