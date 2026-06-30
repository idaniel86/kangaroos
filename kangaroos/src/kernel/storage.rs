use crate::kernel::tcb::Tcb;

/// Combined storage for a task's stack and TCB, suitable for a `static mut`.
///
/// `N` is the stack size in **`u32` words** (bytes ÷ 4).  The `#[task]` macro
/// divides the `stack_size` attribute value by 4 automatically.
///
/// # Layout
/// `#[repr(C)]` places `stack` at the lower address and `tcb` at the higher
/// address.  ARM Cortex-M uses a full-descending stack (SP starts at the top
/// and moves toward lower addresses), so a stack overflow corrupts the bottom
/// of the stack before it can reach the TCB residing above it.
///
/// # Usage
/// Declare one `static mut` per task and pass a mutable reference to
/// `task::spawn_into`:
///
/// ```ignore
/// // 256 words = 1 KiB stack
/// static mut STORAGE: TaskStorage<256> = TaskStorage::new();
/// ```
#[repr(C)]
pub struct TaskStorage<const N: usize> {
    /// Stack area — grows downward from `stack[N - 1]` toward `stack[0]`
    /// at the bottom (lowest address).
    stack: [u32; N],
    /// Task control block, placed above the stack so a stack overflow cannot
    /// corrupt it.
    tcb: Tcb,
}

impl<const N: usize> Default for TaskStorage<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> TaskStorage<N> {
    /// Create a zeroed storage instance suitable for a `static` initialiser.
    pub const fn new() -> Self {
        Self {
            stack: [0u32; N],
            tcb: Tcb::zeroed(),
        }
    }

    /// Return a raw pointer to the embedded TCB.
    pub fn tcb_ptr(&mut self) -> *mut Tcb {
        &mut self.tcb
    }

    /// Return a mutable slice over the stack words.
    pub fn stack_slice(&mut self) -> &mut [u32] {
        &mut self.stack
    }
}
