use core::arch::global_asm;

use super::ArchContext;

/// Zero-sized dispatch token for ARMv6-M (Cortex-M0 / M0+).
pub(crate) struct V6m;

impl ArchContext for V6m {
    fn stack_init(stack: &mut [u32], entry: fn() -> !) -> usize {
        stack_init(stack, entry)
    }
}

// ---------------------------------------------------------------------------
// PendSV — context switch handler (pure assembly, ARMv6-M Thumb-1)
//
// ARMv6-M restrictions vs. ARMv7-M:
//   • `stmdb` / `ldmia` reglist restricted to r0–r7 (no high regs).
//   • High registers (r8–r11) must be saved/restored via `mov` + low-reg loads.
//   • Non-updating `ldmia` is not available; writeback (`!`) is always implied
//     when the base register is not in the reglist.
//
// Stack frame layout (identical to v7m, ascending addresses):
//
//   [SP+ 0]  R4          ← TCB.sp points here
//   [SP+ 4]  R5
//   [SP+ 8]  R6
//   [SP+12]  R7
//   [SP+16]  R8
//   [SP+20]  R9
//   [SP+24]  R10
//   [SP+28]  R11
//   [SP+32]  EXC_RETURN  ← 0xFFFF_FFFD for Thread/PSP/no-FPU
//   [SP+36]  hardware exception frame (R0–R3, R12, LR, PC, xPSR)
//
// Save sequence:
//   1. mrs  r0, psp        — get current task's PSP
//   2. subs r0, r0, #36   — allocate 9 words below PSP
//   3. stmia r0!, {r4-r7} — store R4-R7 at slots 0-3; r0 += 16
//   4. mov r4-r7 ← r8-r11 — copy high regs through low regs
//   5. stmia r0!, {r4-r7} — store R8-R11 at slots 4-7; r0 += 16
//   6. mov r4, lr; str r4,[r0] — store EXC_RETURN at slot 8
//   7. subs r0, r0, #32   \ restore r4-r7; r0 still = base
//      ldmia r0!, {r4-r7} |  r0 = base+16 after ldmia
//      subs r0, r0, #16   /
//   8. bl pendsv_save_and_switch — save old SP; select next; returns new SP
//
// Restore sequence (r0 = new task's SP = base):
//   1. ldmia r0!, {r4-r7}      — restore R4-R7; r0 = base+16
//   2. ldmia r0!, {r1,r2}      — load saved R8,R9; r0 = base+24
//      mov r8,r1; mov r9,r2
//   3. ldmia r0!, {r1,r2}      — load saved R10,R11; r0 = base+32
//      mov r10,r1; mov r11,r2
//   4. ldr r1, [r0]            — load EXC_RETURN into r1
//      adds r0, r0, #4         — r0 = base+36 = new PSP (points at hw frame)
//   5. msr psp, r0; bx r1      — install new PSP and return via EXC_RETURN
// ---------------------------------------------------------------------------
global_asm!(
    ".syntax unified",
    ".thumb",
    ".thumb_func",
    ".global PendSV",
    "PendSV:",

    // ---- Save current task ----
    "    mrs   r0, psp",
    "    subs  r0, r0, #36",       // allocate 9 words below PSP; r0 = base

    "    stmia r0!, {{r4-r7}}",    // [base+0..12] = r4-r7;  r0 = base+16

    "    mov   r4, r8",            // copy high regs through low regs
    "    mov   r5, r9",
    "    mov   r6, r10",
    "    mov   r7, r11",
    "    stmia r0!, {{r4-r7}}",    // [base+16..28] = r8-r11; r0 = base+32

    "    mov   r4, lr",
    "    str   r4, [r0]",          // [base+32] = EXC_RETURN

    "    subs  r0, r0, #32",       // r0 = base
    "    ldmia r0!, {{r4-r7}}",    // restore r4-r7;          r0 = base+16
    "    subs  r0, r0, #16",       // r0 = base  (pass to C)

    "    bl    pendsv_save_and_switch",  // r0 in = old SP; r0 out = new SP

    // ---- Restore new task ----
    "    ldmia r0!, {{r4-r7}}",    // restore r4-r7; r0 = base+16
    "    ldmia r0!, {{r1, r2}}",   // r1=saved r8, r2=saved r9
    "    mov   r8, r1",
    "    mov   r9, r2",
    "    ldmia r0!, {{r1, r2}}",   // r1=saved r10, r2=saved r11
    "    mov   r10, r1",
    "    mov   r11, r2",
    "    ldr   r1, [r0]",          // r1 = EXC_RETURN
    "    adds  r0, r0, #4",        // r0 = new PSP (points at hardware frame)
    "    msr   psp, r0",
    "    bx    r1",                // EXC_RETURN → Thread mode / PSP
);

// ---------------------------------------------------------------------------
// SVCall — used once to launch the first task.
//
// On entry (Handler mode): lr = EXC_RETURN set by hardware.
// `bl svc_first_task_sp` clobbers lr, but we recover EXC_RETURN from the
// software frame stored in the task's initial stack.
// ---------------------------------------------------------------------------
global_asm!(
    ".syntax unified",
    ".thumb",
    ".thumb_func",
    ".global SVCall",
    "SVCall:",
    "    bl    svc_first_task_sp",   // r0 = first task's SP (= base of sw frame)

    // Restore r4-r7
    "    ldmia r0!, {{r4-r7}}",      // r0 = base+16

    // Restore r8-r11 via r1-r2 pairs
    "    ldmia r0!, {{r1, r2}}",     // r1=saved r8, r2=saved r9
    "    mov   r8, r1",
    "    mov   r9, r2",
    "    ldmia r0!, {{r1, r2}}",     // r1=saved r10, r2=saved r11
    "    mov   r10, r1",
    "    mov   r11, r2",

    // Load EXC_RETURN; advance r0 to hardware frame
    "    ldr   r1, [r0]",            // r1 = 0xFFFF_FFFD
    "    adds  r0, r0, #4",          // r0 = base+36 = hardware frame address

    // Install PSP and switch Thread mode to use it
    "    msr   psp, r0",
    "    movs  r2, #2",
    "    msr   control, r2",         // SPSEL=1: Thread mode uses PSP
    "    isb",

    "    bx    r1",                  // EXC_RETURN → launches task
);

/// Selects and activates the first task at launch; called from the SVCall stub.
#[unsafe(no_mangle)]
unsafe extern "C" fn svc_first_task_sp() -> usize {
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
        crate::ktask(best_idx).state = TaskState::Running;
        crate::ktask(best_idx).sp
    }
}

/// Called from the PendSV stub (AAPCS: r0 = arg / return value).
#[unsafe(no_mangle)]
unsafe extern "C" fn pendsv_save_and_switch(current_sp: usize) -> usize {
    unsafe {
        use crate::kernel::tcb::TaskState;

        let old = crate::CURRENT_TASK;
        crate::ktask(old).sp = current_sp;

        if crate::ktask(old).state == TaskState::Running {
            crate::ktask(old).state = TaskState::Ready;
        }

        let next = crate::kernel::scheduler::find_next();
        crate::CURRENT_TASK = next;
        crate::ktask(next).state = TaskState::Running;

        crate::ktask(next).sp
    }
}

/// Build an initial stack frame (layout identical to v7m).
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
    assert!(n >= 21, "stack must be at least 21 words (84 bytes): 17 frame + 4 canary");

    // Hardware exception frame
    stack[n - 1] = 0x0100_0000;
    stack[n - 2] = entry as usize as u32 & !1;
    stack[n - 3] = task_exit as usize as u32 | 1;
    stack[n - 4] = 0; // R12
    stack[n - 5] = 0; // R3
    stack[n - 6] = 0; // R2
    stack[n - 7] = 0; // R1
    stack[n - 8] = 0; // R0

    // Software frame (as-if saved by PendSV)
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
