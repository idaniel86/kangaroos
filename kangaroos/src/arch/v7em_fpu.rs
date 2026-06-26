use core::arch::global_asm;

use super::ArchContext;

/// Zero-sized dispatch token for ARMv7E-M + FPU (Cortex-M4F / M7).
pub(crate) struct V7emFpu;

impl ArchContext for V7emFpu {
    fn stack_init(stack: &mut [u32], entry: fn() -> !) -> usize {
        stack_init(stack, entry)
    }
}

// ---------------------------------------------------------------------------
// PendSV — context switch handler with optional FPU context save/restore.
//
// The hardware provides *lazy stacking*: on exception entry with FPCA=1 it
// allocates space in the PSP frame for s0–s15 + FPSCR but defers the actual
// write until the first VFP instruction in the handler.  The first `vstmdb`
// below triggers that flush automatically, so no special FPCCR manipulation
// is required.
//
// EXC_RETURN encoding (bit 4):
//   1 → standard 8-word hardware frame (no FPU context was active)
//   0 → extended 26-word hardware frame (s0–s15 + FPSCR saved by hardware)
//
// Full stack layout when FPU context IS saved (ascending addresses):
//
//   [SP+ 0]  S16          ← TCB.sp points here
//   [SP+ 4]  S17
//    ...
//   [SP+60]  S31
//   [SP+64]  R4
//    ...
//   [SP+92]  R11
//   [SP+96]  EXC_RETURN   (= 0xFFFF_FFED: Thread/PSP/extended-frame)
//   [SP+100] R0            ← hardware extended exception frame (26 words)
//    ...   FPSCR, S0–S15, reserved, xPSR, PC, LR, R12, R3–R0
//
// Without FPU context (layout identical to v7m):
//
//   [SP+ 0]  R4            ← TCB.sp
//    ...
//   [SP+32]  EXC_RETURN   (= 0xFFFF_FFFD: Thread/PSP/standard-frame)
//   [SP+36]  R0            ← hardware standard exception frame (8 words)
//
// Save sequence:
//   1. mrs r0, psp          — get task PSP
//   2. mrs r1, control
//      tst r1, #4           — check CONTROL.FPCA (bit 2)
//      beq 1f               — skip FPU save if task did not use FPU
//   3. vstmdb r0!, {s16-s31} — push callee-saved FPU regs; r0 -= 64
// 1:
//   4. stmdb r0!, {r4-r11, lr} — push integer regs + EXC_RETURN; r0 -= 36
//   5. bl pendsv_save_and_switch — store old SP, select next, return new SP
//
// Restore sequence:
//   6. ldmia r0!, {r4-r11, lr} — pop integer regs; lr = saved EXC_RETURN
//   7. tst lr, #0x10         — test EXC_RETURN bit 4
//      bne 2f                — bit 4=1: no FPU frame, skip
//   8. vldmia r0!, {s16-s31} — restore callee-saved FPU regs; r0 += 64
// 2:
//   9. msr psp, r0           — install new PSP (points at hardware frame)
//  10. bx lr                 — EXC_RETURN → resumes task
// ---------------------------------------------------------------------------
global_asm!(
    ".syntax unified",
    ".thumb",
    ".thumb_func",
    ".global PendSV",
    "PendSV:",

    // ---- Save current task ----
    "    mrs   r0, psp",
    "    mrs   r1, control",
    "    tst   r1, #4",              // Z=1 if FPCA==0 (task did not use FPU)
    "    beq   1f",                  // skip FPU save
    "    vstmdb r0!, {{s16-s31}}",   // push callee-saved FPU regs (s16-s31)
    "1:",
    "    stmdb r0!, {{r4-r11, lr}}", // push integer callee-saved + EXC_RETURN

    "    bl    pendsv_save_and_switch",

    // ---- Restore new task ----
    "    ldmia r0!, {{r4-r11, lr}}", // pop integer regs; lr = EXC_RETURN
    "    tst   lr, #0x10",           // bit 4=0 → extended (FPU) frame
    "    bne   2f",                  // bit 4=1 → no FPU frame, skip restore
    "    vldmia r0!, {{s16-s31}}",   // restore callee-saved FPU regs (s16-s31)
    "2:",
    "    msr   psp, r0",
    "    bx    lr",
);

// ---------------------------------------------------------------------------
// SVCall — launches the very first task.
//
// The initial software frame always has EXC_RETURN = 0xFFFF_FFFD (no FPU),
// so no FPU logic is needed here.
// ---------------------------------------------------------------------------
global_asm!(
    ".syntax unified",
    ".thumb",
    ".thumb_func",
    ".global SVCall",
    "SVCall:",
    "    bl    svc_first_task_sp",
    "    msr   psp, r0",
    "    movs  r1, #2",
    "    msr   control, r1",         // SPSEL=1: Thread mode uses PSP
    "    isb",
    "    ldmia r0!, {{r4-r11, lr}}", // pop software frame; lr = 0xFFFF_FFFD
    "    msr   psp, r0",             // PSP → hardware frame
    "    bx    lr",                  // EXC_RETURN: Thread/PSP/no-FPU → task runs
);

/// Build the initial stack frame for a new task (no FPU context saved).
///
/// Layout (ascending addresses):
/// ```text
///  [n-1]  xPSR  = 0x0100_0000   ← hardware frame (8 words)
///  [n-2]  PC    = entry & !1
///  [n-3]  LR    = task_exit | 1
///  [n-4]  R12   = 0
///  [n-5]  R3    = 0
///  [n-6]  R2    = 0
///  [n-7]  R1    = 0
///  [n-8]  R0    = 0
///  [n-9]  EXC_RETURN = 0xFFFF_FFFD  ← software frame (9 words)
///  [n-10] R11   = 0
///   ...
///  [n-17] R4    = 0              ← initial TCB.sp
/// ```
///
/// Tasks start without FPU context.  If a task later executes a VFP
/// instruction, the hardware sets CONTROL.FPCA and subsequent context
/// switches will save/restore s16–s31 automatically.
pub fn stack_init(stack: &mut [u32], entry: fn() -> !) -> usize {
    let n = stack.len();
    assert!(n >= 21, "stack must be at least 21 words (84 bytes): 17 frame + 4 canary");

    // Hardware exception frame
    stack[n - 1] = 0x0100_0000;
    stack[n - 2] = entry as usize as u32 & !1;
    stack[n - 3] = task_exit as *const () as usize as u32 | 1;
    stack[n - 4] = 0; // R12
    stack[n - 5] = 0; // R3
    stack[n - 6] = 0; // R2
    stack[n - 7] = 0; // R1
    stack[n - 8] = 0; // R0

    // Software frame (as-if saved by PendSV — no FPU context)
    stack[n - 9]  = 0xFFFF_FFFD; // EXC_RETURN: Thread + PSP + no FPU
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

/// Trap if a task function ever returns.
unsafe extern "C" fn task_exit() -> ! {
    loop {
        cortex_m::asm::wfi();
    }
}
