/// Host (non-ARM) mock implementation of [`super::ArchContext`].
///
/// Used exclusively by the test suite (`cargo test` on the development
/// machine). Every method is pure Rust — no assembly, no Cortex-M
/// peripherals — so the scheduler and sync-primitive unit tests can run
/// on `x86_64-apple-darwin` (or any other host target).
///
/// The stack-frame layout mirrors the real ARMv7-M layout defined in
/// `v7m.rs`; tests that inspect individual frame words can therefore
/// share expectations with the embedded target.

use super::ArchContext;

/// Zero-sized dispatch token for the host mock arch.
pub(crate) struct HostContext;

impl ArchContext for HostContext {
    /// Build a synthetic initial stack frame.
    ///
    /// The layout is identical to the ARMv7-M software + hardware frame
    /// written by `v7m::stack_init`:
    ///
    /// ```text
    ///  [n-1]  xPSR  = 0x0100_0000   (Thumb bit)
    ///  [n-2]  PC    = entry as u32 & !1
    ///  [n-3]  LR    = 0xDEAD_0001   (sentinel; tasks must not return)
    ///  [n-4]  R12   = 0
    ///  [n-5]  R3    = 0
    ///  [n-6]  R2    = 0
    ///  [n-7]  R1    = 0
    ///  [n-8]  R0    = 0
    ///  [n-9]  LR    = 0xFFFF_FFFD   (EXC_RETURN: Thread + PSP + no FPU)
    ///  [n-10] R11   = 0
    ///  ...
    ///  [n-17] R4    = 0   ← TCB.sp
    /// ```
    fn stack_init(stack: &mut [u32], entry: fn() -> !) -> usize {
        let n = stack.len();
        assert!(
            n >= 21,
            "stack must be at least 21 words (84 bytes): 17 frame + 4 canary"
        );

        // Hardware exception frame (8 words)
        stack[n - 1] = 0x0100_0000;                 // xPSR: Thumb bit
        stack[n - 2] = entry as usize as u32 & !1;  // PC
        stack[n - 3] = 0xDEAD_0001;                 // LR sentinel
        stack[n - 4] = 0;                            // R12
        stack[n - 5] = 0;                            // R3
        stack[n - 6] = 0;                            // R2
        stack[n - 7] = 0;                            // R1
        stack[n - 8] = 0;                            // R0

        // Software frame (9 words, as-if-PendSV-saved)
        stack[n - 9]  = 0xFFFF_FFFD;                // EXC_RETURN
        stack[n - 10] = 0;                           // R11
        stack[n - 11] = 0;                           // R10
        stack[n - 12] = 0;                           // R9
        stack[n - 13] = 0;                           // R8
        stack[n - 14] = 0;                           // R7
        stack[n - 15] = 0;                           // R6
        stack[n - 16] = 0;                           // R5
        stack[n - 17] = 0;                           // R4  ← initial SP

        core::ptr::addr_of!(stack[n - 17]) as usize
    }
}
