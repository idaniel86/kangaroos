use core::arch::global_asm;

use super::ArchContext;

/// Zero-sized dispatch token for ARMv8-M (Cortex-M23 / M33 / M55 / M85).
pub(crate) struct V8m;

impl ArchContext for V8m {
    fn stack_init(stack: &mut [u32], entry: fn() -> !) -> usize {
        stack_init(stack, entry)
    }
}

// ---------------------------------------------------------------------------
// Design notes
//
// ARMv8-M covers two sub-architectures:
//   • Baseline (Cortex-M23)  — Thumb-1 only, no FPU, same instruction limits
//     as ARMv6-M (`stmdb` with high-reg list unavailable, etc.)
//   • Mainline (Cortex-M33/M55/M85) — full Thumb-2, optional FPU
//
// To support all three targets from a single module we use v6m-compatible
// integer instructions throughout (push high regs through r4-r7).
//
// PSPLIM — the hardware stack-limit register
//   ARMv8-M enforces PSP >= PSPLIM automatically; no SysTick canary check is
//   needed (though the canary is still initialised for defence-in-depth).
//   We update PSPLIM inside the Rust save-and-switch function, before it
//   returns, so the new task's limit is already in place when PendSV does
//   `msr psp, r0; bx lr`.
//
// Stack frame layout WITHOUT FPU (ascending addresses = TCB.sp is lowest):
//
//   [SP+ 0]  R4          ← TCB.sp
//   [SP+ 4]  R5
//    ...
//   [SP+28]  R11
//   [SP+32]  EXC_RETURN  (= 0xFFFF_FFFD)
//   [SP+36]  hardware exception frame (8 words: R0–R3, R12, LR, PC, xPSR)
//
// Stack frame layout WITH FPU active (FPCA=1, Mainline+FPU targets only):
//
//   [SP+  0]  R4          ← TCB.sp
//    ...
//   [SP+ 32]  EXC_RETURN  (= 0xFFFF_FFED: Thread/PSP/extended)
//   [SP+ 36]  S16
//    ...
//   [SP+ 96]  S31
//   [SP+100]  hardware extended exception frame (26 words)
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// SVCall — launches the very first task (common to all ARMv8-M).
//
// Uses v6m-compatible instructions (valid on Baseline and Mainline).
// PSPLIM is set inside `svc_first_task_sp` before the function returns.
// ---------------------------------------------------------------------------
global_asm!(
    ".syntax unified",
    ".thumb",
    ".thumb_func",
    ".global SVCall",
    "SVCall:",
    "    bl    svc_first_task_sp", // r0 = first task SP; PSPLIM set inside
    // Restore r4-r7
    "    ldmia r0!, {{r4-r7}}", // r0 = base+16
    // Restore r8-r11 (via low-reg pairs)
    "    ldmia r0!, {{r1, r2}}", // r1=saved r8, r2=saved r9
    "    mov   r8, r1",
    "    mov   r9, r2",
    "    ldmia r0!, {{r1, r2}}", // r1=saved r10, r2=saved r11
    "    mov   r10, r1",
    "    mov   r11, r2",
    // Load EXC_RETURN; advance r0 to hardware frame
    "    ldr   r1, [r0]",   // r1 = 0xFFFF_FFFD
    "    adds  r0, r0, #4", // r0 = base+36 = hardware frame address
    // Install PSP and switch Thread mode to use it
    "    msr   psp, r0",
    "    movs  r2, #2",
    "    msr   control, r2", // SPSEL=1: Thread mode → PSP
    "    isb",
    "    bx    r1", // EXC_RETURN: launches first task
);

// ---------------------------------------------------------------------------
// PendSV WITHOUT FPU
//
// Used for:
//   thumbv8m.base-none-eabi   (ARMv8-M Baseline — never has FPU)
//   thumbv8m.main-none-eabi   (ARMv8-M Mainline, no FPU variant)
//
// v6m-compatible integer ops only.  PSPLIM is updated by
// `pendsv_save_and_switch` before it returns.
// ---------------------------------------------------------------------------
#[cfg(not(has_fpu))]
global_asm!(
    ".syntax unified",
    ".thumb",
    ".thumb_func",
    ".global PendSV",
    "PendSV:",
    // ---- Save current task ----
    "    mrs   r0, psp",        // r0 = PSP
    "    subs  r0, r0, #36",    // allocate 9 words; r0 = base
    "    stmia r0!, {{r4-r7}}", // [base+0..12] = r4-r7;   r0 = base+16
    "    mov   r4, r8",         // copy high regs through r4-r7
    "    mov   r5, r9",
    "    mov   r6, r10",
    "    mov   r7, r11",
    "    stmia r0!, {{r4-r7}}", // [base+16..28] = r8-r11;  r0 = base+32
    "    mov   r4, lr",
    "    str   r4, [r0]",       // [base+32] = EXC_RETURN
    "    subs  r0, r0, #32",    // r0 = base
    "    ldmia r0!, {{r4-r7}}", // restore r4-r7; r0 = base+16
    "    subs  r0, r0, #16",    // r0 = base (pass to C function)
    // Updates PSPLIM for the next task; returns new task SP in r0.
    "    bl    pendsv_save_and_switch",
    // ---- Restore new task ----
    "    ldmia r0!, {{r4-r7}}",  // r4-r7 restored;          r0 = base+16
    "    ldmia r0!, {{r1, r2}}", // r1=saved r8, r2=saved r9;r0 = base+24
    "    mov   r8, r1",
    "    mov   r9, r2",
    "    ldmia r0!, {{r1, r2}}", // r1=saved r10, r2=saved r11; r0=base+32
    "    mov   r10, r1",
    "    mov   r11, r2",
    "    ldr   r1, [r0]",   // r1 = EXC_RETURN
    "    adds  r0, r0, #4", // r0 = base+36 = new PSP
    "    msr   psp, r0",
    "    bx    r1",
);

// ---------------------------------------------------------------------------
// PendSV WITH FPU  (ARMv8-M Mainline + FPU — thumbv8m.main-none-eabihf)
//
// Same logic as v7em_fpu but integer ops use the v6m-compatible save/restore
// pattern.  PSPLIM updated by `pendsv_save_and_switch`.
//
// EXC_RETURN bit 4:
//   1 → standard frame (no FPU context) — skip s16-s31
//   0 → extended frame (FPU context active) — save/restore s16-s31
// ---------------------------------------------------------------------------
#[cfg(has_fpu)]
global_asm!(
    ".syntax unified",
    ".thumb",
    ".fpu fpv5-sp-d16",
    ".thumb_func",
    ".global PendSV",
    "PendSV:",
    // ---- Save current task ----
    "    mrs   r0, psp",
    "    mrs   r1, control",
    "    tst   r1, #4", // Z=1 if FPCA==0 (task did not use FPU)
    "    beq   1f",
    "    vstmdb r0!, {{s16-s31}}", // push callee-saved FPU regs; r0 -= 64
    "1:",
    "    subs  r0, r0, #36",    // allocate integer frame; r0 = base
    "    stmia r0!, {{r4-r7}}", // [base+0..12] = r4-r7;   r0 = base+16
    "    mov   r4, r8",
    "    mov   r5, r9",
    "    mov   r6, r10",
    "    mov   r7, r11",
    "    stmia r0!, {{r4-r7}}", // [base+16..28] = r8-r11;  r0 = base+32
    "    mov   r4, lr",
    "    str   r4, [r0]", // [base+32] = EXC_RETURN
    "    subs  r0, r0, #32",
    "    ldmia r0!, {{r4-r7}}",         // restore r4-r7; r0 = base+16
    "    subs  r0, r0, #16",            // r0 = base
    "    bl    pendsv_save_and_switch", // updates PSPLIM; r0 = new SP
    // ---- Restore new task ----
    "    ldmia r0!, {{r4-r7}}",  // r0 = base+16
    "    ldmia r0!, {{r1, r2}}", // r1=saved r8, r2=saved r9
    "    mov   r8, r1",
    "    mov   r9, r2",
    "    ldmia r0!, {{r1, r2}}", // r1=saved r10, r2=saved r11
    "    mov   r10, r1",
    "    mov   r11, r2",
    "    ldr   r1, [r0]",   // r1 = EXC_RETURN
    "    adds  r0, r0, #4", // r0 = base+36
    "    tst   r1, #0x10",  // bit 4=0 → extended (FPU) frame
    "    bne   2f",
    "    vldmia r0!, {{s16-s31}}", // restore callee-saved FPU regs; r0+=64
    "2:",
    "    msr   psp, r0",
    "    bx    r1",
);

/// Selects and activates the first task at launch; called from SVCall stub.
///
/// Sets `PSPLIM` to the first task's stack base so that hardware stack-overflow
/// detection is active from the very first instruction the task executes.
#[unsafe(no_mangle)]
unsafe extern "C" fn svc_first_task_sp() -> usize {
    // SAFETY: called from SVCall (Handler mode) before the scheduler starts.
    // Single-core Cortex-M: no concurrent mutation of TASKS/TASK_COUNT/
    // CURRENT_TASK is possible while we are in Handler mode.
    unsafe {
        use crate::kernel::tcb::TaskState;

        let count = crate::TASK_COUNT;
        let mut best_prio = u8::MAX;
        let mut best_idx = 0usize;

        for i in 0..count {
            let t = crate::ktask(i);
            if matches!(t.state, TaskState::Ready) && t.priority < best_prio {
                best_prio = t.priority;
                best_idx = i;
            }
        }

        crate::CURRENT_TASK = best_idx;
        let task = crate::ktask(best_idx);
        task.state = TaskState::Running;

        // Arm the hardware stack-limit register before the task starts.
        core::arch::asm!(
            "msr psplim, {0}",
            in(reg) task.stack_base,
            options(nomem, nostack),
        );

        task.sp
    }
}

/// Called from the PendSV stub (AAPCS: r0 = arg / return value).
///
/// Saves the current task's SP, selects the next task, updates `PSPLIM` to
/// the next task's stack base, and returns the next task's SP.
#[unsafe(no_mangle)]
unsafe extern "C" fn pendsv_save_and_switch(current_sp: usize) -> usize {
    // SAFETY: called from PendSV (Handler mode, lowest interrupt priority).
    // Single-core Cortex-M: exclusive access to TASKS/TASK_COUNT/CURRENT_TASK
    // is guaranteed — no other Handler-mode code runs concurrently, and
    // Thread-mode code only touches these globals inside interrupt::free.
    unsafe {
        use crate::kernel::tcb::TaskState;

        let old = crate::CURRENT_TASK;
        crate::ktask(old).sp = current_sp;

        if crate::ktask(old).state == TaskState::Running {
            crate::ktask(old).state = TaskState::Ready;
        }

        let next = crate::kernel::scheduler::find_next();
        crate::CURRENT_TASK = next;
        let task = crate::ktask(next);
        task.state = TaskState::Running;

        // Update PSPLIM to the incoming task's stack base.
        // This executes before `msr psp, r0` in the PendSV stub, so the new
        // limit is in effect the moment the new task's stack pointer is
        // installed.
        core::arch::asm!(
            "msr psplim, {0}",
            in(reg) task.stack_base,
            options(nomem, nostack),
        );

        task.sp
    }
}

/// Build the initial stack frame (layout identical to v7m / v6m).
///
/// Tasks start without FPU context (`EXC_RETURN = 0xFFFF_FFFD`).  If a task
/// later executes a VFP instruction the hardware sets `CONTROL.FPCA` and
/// subsequent context switches save/restore s16–s31 automatically.
///
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
pub fn stack_init(stack: &mut [u32], entry: fn() -> !) -> usize {
    let n = stack.len();
    assert!(
        n >= 21,
        "stack must be at least 21 words (84 bytes): 17 frame + 4 canary"
    );

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
    stack[n - 9] = 0xFFFF_FFFD; // EXC_RETURN: Thread + PSP + no FPU
    stack[n - 10] = 0; // R11
    stack[n - 11] = 0; // R10
    stack[n - 12] = 0; // R9
    stack[n - 13] = 0; // R8
    stack[n - 14] = 0; // R7
    stack[n - 15] = 0; // R6
    stack[n - 16] = 0; // R5
    stack[n - 17] = 0; // R4  ← SP points here

    core::ptr::addr_of!(stack[n - 17]) as usize
}

/// Trap if a task function ever returns.
unsafe extern "C" fn task_exit() -> ! {
    loop {
        cortex_m::asm::wfi();
    }
}
