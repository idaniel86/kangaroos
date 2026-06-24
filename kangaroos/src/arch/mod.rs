#[cfg(any(armv7m, armv7em))]
/// Architecture abstraction — implemented by every supported Cortex-M variant.
///
/// All methods are free functions (no `self`) because the arch structs are
/// zero-sized types used purely as dispatch tokens.
///
/// The kernel calls these exclusively through the `Arch` type alias so that
/// the rest of the codebase stays free of arch-specific `#[cfg]` guards.
pub(crate) trait ArchContext {
    /// Build the initial stack frame for a newly registered task.
    ///
    /// `stack` is a statically-allocated `[u32]` slice (≥ 17 words). `entry`
    /// is the task function; it must never return. Returns the SP value to
    /// store in `Tcb::sp` — the address of the lowest word of the pre-built
    /// software save frame.
    fn stack_init(stack: &mut [u32], entry: fn() -> !) -> usize;

    // Future phases will add:
    //   fn canary_init(stack: &mut [u32])     — Phase 2: write 0xDEAD_BEEF words
    //   fn canary_check(tcb: &Tcb) -> bool    — Phase 2: verify canary in SysTick
    //   fn mpu_guard(stack_base: *const u8)   — Phase 2: reprogram MPU guard region
}

// ---------------------------------------------------------------------------
// Arch-specific modules (gated by custom cfgs emitted by build.rs)
// ---------------------------------------------------------------------------

#[cfg(any(armv7m, armv7em))]
pub(crate) mod v7m;

// Catch unsupported targets at compile time rather than silently producing a
// binary with no scheduler code.
#[cfg(not(any(armv6m, armv7m, armv7em, armv8m)))]
compile_error!(
    "No supported Cortex-M variant detected. \
     Build with --target thumbv6m-none-eabi, thumbv7m-none-eabi, \
     thumbv7em-none-eabi, or thumbv7em-none-eabihf."
);

// ---------------------------------------------------------------------------
// Arch type alias — the kernel uses `arch::Arch::stack_init(...)` everywhere
// instead of a concrete module path, so adding a new variant only requires
// changing this alias and adding a module.
// ---------------------------------------------------------------------------

#[cfg(any(armv7m, armv7em))]
pub(crate) use v7m::V7m as Arch;
