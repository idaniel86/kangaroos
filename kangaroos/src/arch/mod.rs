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
    /// `stack` is a statically-allocated `[u32]` slice (≥ 21 words: 17 for
    /// the initial frame + 4 for the stack-overflow canary). `entry` is the
    /// task function; it must never return. Returns the SP value to store in
    /// `Tcb::sp` — the address of the lowest word of the pre-built software
    /// save frame.
    fn stack_init(stack: &mut [u32], entry: fn() -> !) -> usize;

    /// Write the stack-overflow canary pattern (`0xDEAD_BEEF` × 4) to the
    /// bottom four words of the stack. Called by `spawn_task` before
    /// `stack_init` so the canary is in place before the task ever runs.
    fn canary_init(stack: &mut [u32]) {
        const CANARY: u32 = 0xDEAD_BEEF;
        stack[0] = CANARY;
        stack[1] = CANARY;
        stack[2] = CANARY;
        stack[3] = CANARY;
    }

    /// Return `true` if the canary at `stack_base` is intact.
    ///
    /// Checked once per SysTick in `systick_handler`. A `false` return means
    /// the task has overflowed its stack.
    ///
    /// # Safety (internal)
    /// `stack_base` must be the start address of a live `'static` stack slice;
    /// the first four words are always mapped and accessible.
    fn canary_check(stack_base: usize) -> bool {
        const CANARY: u32 = 0xDEAD_BEEF;
        // SAFETY: stack_base was recorded from a &'static mut [u32] in
        // spawn_task and is valid for the lifetime of the program.
        unsafe {
            let p = stack_base as *const u32;
            *p == CANARY
                && *p.add(1) == CANARY
                && *p.add(2) == CANARY
                && *p.add(3) == CANARY
        }
    }

    // Future phases will add:
    //   fn mpu_guard(stack_base: *const u8)   — Phase 2+: reprogram MPU guard region
}

// ---------------------------------------------------------------------------
// Arch-specific modules (gated by custom cfgs emitted by build.rs)
// ---------------------------------------------------------------------------

#[cfg(armv6m)]
pub(crate) mod v6m;

#[cfg(any(armv7m, all(armv7em, not(has_fpu))))]
pub(crate) mod v7m;

#[cfg(all(armv7em, has_fpu))]
pub(crate) mod v7em_fpu;

#[cfg(armv8m)]
pub(crate) mod v8m;

// Host mock arch — used by unit tests running on the development machine.
// Not compiled for any ARM target.
#[cfg(not(target_arch = "arm"))]
pub(crate) mod host;

// Catch unsupported ARM targets at compile time rather than silently producing
// a binary with no scheduler code.  The guard intentionally excludes non-ARM
// builds (host unit tests) so `cargo test` does not trip this error.
#[cfg(all(target_arch = "arm", not(any(armv6m, armv7m, armv7em, armv8m))))]
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

#[cfg(armv6m)]
pub(crate) use v6m::V6m as Arch;

#[cfg(any(armv7m, all(armv7em, not(has_fpu))))]
pub(crate) use v7m::V7m as Arch;

#[cfg(all(armv7em, has_fpu))]
pub(crate) use v7em_fpu::V7emFpu as Arch;

#[cfg(armv8m)]
pub(crate) use v8m::V8m as Arch;

// Host: resolve to the mock context so the rest of the crate compiles
// on non-ARM targets (needed for `cargo test`).
#[cfg(not(target_arch = "arm"))]
pub(crate) use host::HostContext as Arch;
