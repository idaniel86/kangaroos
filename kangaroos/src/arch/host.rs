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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::HostContext;
    use crate::arch::ArchContext as _;

    #[test]
    fn canary_init_writes_dead_beef() {
        let mut stack = [0u32; 8];
        HostContext::canary_init(&mut stack);
        const CANARY: u32 = 0xDEAD_BEEF;
        assert_eq!(stack[0], CANARY);
        assert_eq!(stack[1], CANARY);
        assert_eq!(stack[2], CANARY);
        assert_eq!(stack[3], CANARY);
    }

    #[test]
    fn canary_init_does_not_touch_rest_of_stack() {
        let mut stack = [0xABCD_1234u32; 8];
        HostContext::canary_init(&mut stack);
        // Words 4..7 must be unchanged.
        assert!(stack[4..].iter().all(|&w| w == 0xABCD_1234));
    }

    #[test]
    fn canary_check_valid() {
        let mut stack = [0u32; 8];
        HostContext::canary_init(&mut stack);
        assert!(HostContext::canary_check(stack.as_ptr() as usize));
    }

    #[test]
    fn canary_check_detects_single_word_corruption() {
        let mut stack = [0u32; 8];
        HostContext::canary_init(&mut stack);
        for i in 0..4 {
            stack[i] ^= 1; // flip one bit
            assert!(
                !HostContext::canary_check(stack.as_ptr() as usize),
                "canary_check should fail when word {i} is corrupted"
            );
            stack[i] ^= 1; // restore
        }
    }

    #[test]
    fn stack_init_sp_within_stack() {
        fn task_fn() -> ! { loop {} }
        let mut stack = [0u32; 32];
        HostContext::canary_init(&mut stack);
        let sp = HostContext::stack_init(&mut stack, task_fn);

        let stack_start = stack.as_ptr() as usize;
        let stack_end = stack_start + 32 * 4;
        assert!(sp >= stack_start && sp < stack_end, "SP {sp:#x} is outside stack range");
    }

    #[test]
    fn stack_init_frame_xpsr_has_thumb_bit() {
        // The initial xPSR must have bit 24 set (Thumb state) so the first
        // EXC_RETURN enters Thumb mode.
        fn task_fn() -> ! { loop {} }
        let mut stack = [0u32; 32];
        let sp = HostContext::stack_init(&mut stack, task_fn);

        // Frame layout (from sp, low→high):
        //   [0]  R4  (SW frame start / initial SP)
        //   ...
        //   [8]  EXC_RETURN
        //   [9]  R0   ← start of HW frame
        //   ...
        //   [16] xPSR ← offset 16 from initial SP
        let xpsr = unsafe { *(sp as *const u32).add(16) };
        assert_eq!(xpsr, 0x0100_0000, "xPSR Thumb bit not set (got {xpsr:#010x})");
    }

    #[test]
    fn stack_init_frame_exc_return() {
        fn task_fn() -> ! { loop {} }
        let mut stack = [0u32; 32];
        let sp = HostContext::stack_init(&mut stack, task_fn);

        // EXC_RETURN is the 9th word from the initial SP (index 8).
        let exc_return = unsafe { *(sp as *const u32).add(8) };
        assert_eq!(exc_return, 0xFFFF_FFFD, "EXC_RETURN value wrong (got {exc_return:#010x})");
    }
}
