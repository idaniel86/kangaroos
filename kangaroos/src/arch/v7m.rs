use core::arch::global_asm;

use super::ArchContext;

/// Zero-sized dispatch token for ARMv7-M (Cortex-M3 / M4 without FPU context).
pub(crate) struct V7m;

impl ArchContext for V7m {
    fn stack_init(stack: &mut [u32], entry: fn() -> !) -> usize {
        stack_init(stack, entry)
    }
}

// ---------------------------------------------------------------------------
// PendSV — context switch handler (pure assembly)
//
// On entry: MSP = handler stack, PSP = current task's stack.
//
// Protocol:
//   1. Read PSP into r0.
//   2. Push r4–r11 + EXC_RETURN (lr) onto the current task's PSP stack.
//   3. Call pendsv_save_and_switch(r0) — stores old SP, selects next, returns new SP.
//   4. Pop  r4–r11 + EXC_RETURN from the new task's stack.
//   5. Update PSP; BX lr (valid EXC_RETURN because we ARE in Handler mode here).
// ---------------------------------------------------------------------------
global_asm!(
    ".syntax unified",
    ".thumb",
    ".thumb_func",
    ".global PendSV",
    "PendSV:",
    "    mrs   r0, psp",
    "    stmdb r0!, {{r4-r11, lr}}",
    "    bl    pendsv_save_and_switch",
    "    ldmia r0!, {{r4-r11, lr}}",
    "    msr   psp, r0",
    "    bx    lr",
);

// ---------------------------------------------------------------------------
// SVCall — used once to launch the first task.
//
// EXC_RETURN (bx lr) is only valid from Handler mode; Thread-mode use is
// UNPREDICTABLE on ARMv7-M and triggers a UsageFault → HardFault.
// kernel_start fires `svc #0` to enter Handler mode so the EXC_RETURN here
// is architecturally correct.
// ---------------------------------------------------------------------------
global_asm!(
    ".syntax unified",
    ".thumb",
    ".thumb_func",
    ".global SVCall",
    "SVCall:",
    "    bl    svc_first_task_sp",    // r0 = TASKS[0].sp  (clobbers lr — fine,
    "                    ",           //   we restore EXC_RETURN from the task stack)
    "    msr   psp, r0",
    "    movs  r1, #2",
    "    msr   control, r1",          // SPSEL=1: Thread mode uses PSP after return
    "    isb",
    "    ldmia r0!, {{r4-r11, lr}}",  // pop software frame; lr = 0xFFFFFFFD
    "    msr   psp, r0",              // PSP now → hardware frame
    "    bx    lr",                   // valid EXC_RETURN from Handler mode → task runs
);

/// Build an initial stack frame so that the first restore from PendSV (or
/// `start_first_task`) launches the task cleanly.
///
/// Memory layout after this call (address increases upward):
///
/// ```text
///  [n-1]  xPSR  = 0x0100_0000   ← hardware frame (8 words)
///  [n-2]  PC    = entry & !1
///  [n-3]  LR    = task_exit | 1  (Thumb sentinel if task returns)
///  [n-4]  R12   = 0
///  [n-5]  R3    = 0
///  [n-6]  R2    = 0
///  [n-7]  R1    = 0
///  [n-8]  R0    = 0
///  [n-9]  LR    = 0xFFFF_FFFD   ← software frame (9 words, as-if-PendSV-saved)
///  [n-10] R11   = 0
///   ...
///  [n-17] R4    = 0              ← initial TCB.sp
/// ```
///
/// Returns the value to store in `Tcb::sp`.
pub fn stack_init(stack: &mut [u32], entry: fn() -> !) -> usize {
    let n = stack.len();
    assert!(n >= 21, "stack must be at least 21 words (84 bytes): 17 frame + 4 canary");

    // Hardware exception frame
    stack[n - 1] = 0x0100_0000;                    // xPSR: Thumb bit
    stack[n - 2] = entry as usize as u32 & !1;     // PC (bit 0 cleared; T-bit in xPSR)
    stack[n - 3] = task_exit as *const () as usize as u32 | 1;  // LR: trap if task returns
    stack[n - 4] = 0;                               // R12
    stack[n - 5] = 0;                               // R3
    stack[n - 6] = 0;                               // R2
    stack[n - 7] = 0;                               // R1
    stack[n - 8] = 0;                               // R0

    // Software frame (as if PendSV just ran for this task)
    stack[n - 9]  = 0xFFFF_FFFD; // EXC_RETURN: Thread + PSP + no FPU frame
    stack[n - 10] = 0;           // R11
    stack[n - 11] = 0;           // R10
    stack[n - 12] = 0;           // R9
    stack[n - 13] = 0;           // R8
    stack[n - 14] = 0;           // R7
    stack[n - 15] = 0;           // R6
    stack[n - 16] = 0;           // R5
    stack[n - 17] = 0;           // R4  ← SP points here

    core::ptr::addr_of!(stack[n - 17]) as usize
}


/// Trap executed if a task function ever returns.
/// Tasks should loop forever; this is just a safety net.
unsafe extern "C" fn task_exit() -> ! {
    loop {
        cortex_m::asm::wfi();
    }
}