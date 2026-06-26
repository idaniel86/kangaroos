use core::cell::UnsafeCell;
use core::marker::PhantomData;
use core::mem::MaybeUninit;

use crate::kernel::scheduler;

// ---------------------------------------------------------------------------
// Internal storage
// ---------------------------------------------------------------------------

struct PoolInner<T, const N: usize> {
    /// Slot storage.  `nodes[i]` holds a live `T` while slot `i` is allocated;
    /// the bytes are uninitialised while the slot is on the free list.
    nodes: [MaybeUninit<T>; N],
    /// Intrusive free-list next-pointer array.
    /// `next[i]` is the index of the next free slot when slot `i` is free;
    /// `0xFF` terminates the list.  The value is irrelevant while slot `i`
    /// is allocated.
    next: [u8; N],
    /// Index of the first free slot; `0xFF` = pool exhausted.
    free_head: u8,
    /// Head of the blocked-alloc intrusive wait list; `0xFF` = nobody waiting.
    wait_head: u8,
}

/// A fixed-capacity O(1) static memory pool.
///
/// Holds at most `N` concurrently-allocated values of type `T`.  All storage
/// is inline — no heap allocator is required.  Both [`alloc`] and [`Drop`] are
/// O(1); the free list is threaded through a separate `next` index array.
///
/// Declare as a `static`:
///
/// ```ignore
/// static BUF_POOL: Pool<[u8; 64], 4> = Pool::new();
///
/// // Non-blocking allocation:
/// if let Some(buf) = BUF_POOL.alloc([0u8; 64]) {
///     buf[0] = 0xAB;
/// } // buf dropped → slot automatically returned
///
/// // Blocking allocation (task-context only):
/// let buf = BUF_POOL.alloc_blocking([0u8; 64]);
/// ```
///
/// [`alloc`]: Pool::alloc
pub struct Pool<T, const N: usize>(UnsafeCell<PoolInner<T, N>>);

// SAFETY: single-core Cortex-M; all mutations are guarded by `interrupt::free`.
unsafe impl<T: Send, const N: usize> Sync for Pool<T, N> {}

impl<T: Send, const N: usize> Default for Pool<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Send, const N: usize> Pool<T, N> {
    /// Create an empty pool.  `const fn` so it can initialise a `static`.
    pub const fn new() -> Self {
        assert!(
            N <= 255,
            "Pool<T, N>: capacity N must be \u{2264} 255 (u8 index limit)"
        );

        // Build the initial free list: slot 0 → 1 → … → N-1 → 0xFF.
        let mut next = [0u8; N];
        let mut i = 0usize;
        while i < N {
            next[i] = if i + 1 < N { (i + 1) as u8 } else { 0xFF };
            i += 1;
        }

        Pool(UnsafeCell::new(PoolInner {
            nodes: [const { MaybeUninit::uninit() }; N],
            next,
            free_head: if N == 0 { 0xFF } else { 0 },
            wait_head: 0xFF,
        }))
    }

    /// Attempt to allocate a slot and initialise it with `val`.
    ///
    /// Returns `Some(box)` on success or `None` if all `N` slots are in use.
    /// O(1).  Safe to call from both task and ISR context.
    pub fn alloc(&self, val: T) -> Option<PoolBox<'_, T, N>> {
        // `src` takes ownership of `val`.  MaybeUninit's drop glue is a no-op,
        // so on the success path T::drop is NOT called when `src` goes out of
        // scope — the bytes now live in `nodes[slot]`.  On the failure path we
        // re-materialise the value and drop it explicitly.
        let src = MaybeUninit::new(val);
        let mut slot: Option<u8> = None;

        crate::port::interrupt_free(|| unsafe {
            let inner = &mut *self.0.get();
            if inner.free_head != 0xFF {
                let s = inner.free_head;
                inner.free_head = inner.next[s as usize];
                core::ptr::copy_nonoverlapping(
                    src.as_ptr(),
                    inner.nodes[s as usize].as_mut_ptr(),
                    1,
                );
                slot = Some(s);
            }
        });

        match slot {
            Some(s) => Some(PoolBox {
                pool: self,
                slot: s,
                _not_sync: PhantomData,
            }),
            None => {
                // Pool exhausted: re-materialise `val` and drop it properly.
                drop(unsafe { src.assume_init() });
                None
            }
        }
    }

    /// Allocate a slot, blocking if the pool is currently exhausted.
    ///
    /// When all `N` slots are in use the calling task is suspended until
    /// another task drops a [`PoolBox`].  The freed slot is handed directly
    /// to the highest-priority waiter (priority-ordered, not FIFO).
    ///
    /// Must not be called from an interrupt handler.
    pub fn alloc_blocking(&self, val: T) -> PoolBox<'_, T, N> {
        // Park the value on the stack so that PoolBox::drop can bitwise-copy
        // it into the freed slot via wait_ptr — identical to Channel's send
        // path.  MaybeUninit prevents a double-drop when `src` later goes out
        // of scope after the bytes have been moved into the pool slot.
        let src = MaybeUninit::new(val);
        let mut must_block = false;

        crate::port::interrupt_free(|| unsafe {
            let inner = &mut *self.0.get();
            if inner.free_head != 0xFF {
                // Fast path: slot available — copy value in immediately.
                let s = inner.free_head;
                inner.free_head = inner.next[s as usize];
                core::ptr::copy_nonoverlapping(
                    src.as_ptr(),
                    inner.nodes[s as usize].as_mut_ptr(),
                    1,
                );
                // Store the slot index in wait_ptr so the post-CS read below
                // works uniformly for both fast and slow paths.
                crate::ktask(crate::CURRENT_TASK).wait_ptr = s as usize;
            } else {
                // Slow path: park this task.
                // Store the address of `src` (on our frozen stack) so that
                // PoolBox::drop can copy the value out on our behalf.
                // SAFETY: `src` outlives alloc_blocking; the stack is frozen
                // while this task is blocked.
                crate::ktask(crate::CURRENT_TASK).wait_ptr = src.as_ptr() as usize;
                scheduler::wait_list_push(&mut inner.wait_head, crate::CURRENT_TASK);
                scheduler::block_current();
                must_block = true;
            }
        });

        if must_block {
            // Switch away.  Execution resumes here after PoolBox::drop copies
            // our value into the freed slot and writes the slot index back into
            // wait_ptr.
            crate::port::trigger_pendsv();
        }

        // Both paths write the assigned slot index into wait_ptr before we
        // reach this point.
        let slot = unsafe { crate::ktask(crate::CURRENT_TASK).wait_ptr as u8 };

        // `src` (MaybeUninit<T>) goes out of scope here.  Its drop glue is a
        // no-op, so T is NOT dropped — the bytes now live in nodes[slot].
        PoolBox {
            pool: self,
            slot,
            _not_sync: PhantomData,
        }
    }

    /// Return the number of free slots.
    ///
    /// Walks the free list — O(free slots).  Intended for diagnostics only.
    pub fn available(&self) -> usize {
        crate::port::interrupt_free(|| unsafe {
            let inner = &*self.0.get();
            let mut count = 0usize;
            let mut cur = inner.free_head;
            while cur != 0xFF {
                count += 1;
                cur = inner.next[cur as usize];
            }
            count
        })
    }
}

// ---------------------------------------------------------------------------
// PoolBox
// ---------------------------------------------------------------------------

/// RAII handle to a slot allocated from a [`Pool`].
///
/// Derefs to `&T` / `&mut T`.  When dropped, the contained `T` is destroyed
/// and the slot is returned to the pool, potentially unblocking a task waiting
/// in [`Pool::alloc_blocking`].
pub struct PoolBox<'pool, T, const N: usize> {
    pool: &'pool Pool<T, N>,
    slot: u8,
    // PhantomData<*mut T> makes PoolBox !Send + !Sync by default.
    // We re-add Send below (the slot can be transferred to another task)
    // but keep !Sync (exclusive handle — not shareable).
    _not_sync: PhantomData<*mut T>,
}

// SAFETY: the slot is owned exclusively; `T: Send` allows moving it.
unsafe impl<T: Send, const N: usize> Send for PoolBox<'_, T, N> {}

impl<T, const N: usize> core::ops::Deref for PoolBox<'_, T, N> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: this task holds the slot exclusively and the value is initialised.
        unsafe { (*self.pool.0.get()).nodes[self.slot as usize].assume_init_ref() }
    }
}

impl<T, const N: usize> core::ops::DerefMut for PoolBox<'_, T, N> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { (*self.pool.0.get()).nodes[self.slot as usize].assume_init_mut() }
    }
}

impl<T, const N: usize> Drop for PoolBox<'_, T, N> {
    fn drop(&mut self) {
        let slot = self.slot;

        // Step 1: Destroy the contained T before entering the critical section.
        // This keeps the CS short and lets T::drop run unrestricted (e.g. it
        // may itself call pool.alloc() or acquire other primitives).
        unsafe {
            (*self.pool.0.get()).nodes[slot as usize].assume_init_drop();
        }

        // Step 2: Return the slot to the pool or hand it to the highest-priority
        // task blocked in alloc_blocking.
        let need_preempt = crate::port::interrupt_free(|| unsafe {
            let inner = &mut *self.pool.0.get();
            if inner.wait_head != 0xFF {
                // Direct handoff: copy the waiter's pending T (parked on its
                // frozen stack at wait_ptr) into the now-empty slot, then
                // overwrite wait_ptr with the slot index as the result.
                let waiter_idx = scheduler::wait_list_pop_highest(&mut inner.wait_head);
                let src = crate::ktask(waiter_idx).wait_ptr as *const T;
                core::ptr::copy_nonoverlapping(src, inner.nodes[slot as usize].as_mut_ptr(), 1);
                crate::ktask(waiter_idx).wait_ptr = slot as usize;
                scheduler::unblock(waiter_idx)
            } else {
                // No waiters: prepend slot to the free list (O(1)).
                inner.next[slot as usize] = inner.free_head;
                inner.free_head = slot;
                false
            }
        });

        if need_preempt {
            crate::port::trigger_pendsv();
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::Pool;
    use core::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn pool_new_all_available() {
        let pool: Pool<u32, 4> = Pool::new();
        assert_eq!(pool.available(), 4);
    }

    #[test]
    fn pool_zero_capacity() {
        let pool: Pool<u32, 0> = Pool::new();
        assert_eq!(pool.available(), 0);
        assert!(pool.alloc(1).is_none());
    }

    #[test]
    fn pool_alloc_decrements_and_drop_restores() {
        let pool: Pool<u32, 3> = Pool::new();
        let b1 = pool.alloc(10).unwrap();
        assert_eq!(pool.available(), 2);
        let b2 = pool.alloc(20).unwrap();
        assert_eq!(pool.available(), 1);
        let b3 = pool.alloc(30).unwrap();
        assert_eq!(pool.available(), 0);

        drop(b1);
        assert_eq!(pool.available(), 1);
        drop(b2);
        drop(b3);
        assert_eq!(pool.available(), 3);
    }

    #[test]
    fn pool_exhausted_returns_none() {
        let pool: Pool<u32, 2> = Pool::new();
        let _b1 = pool.alloc(1).unwrap();
        let _b2 = pool.alloc(2).unwrap();
        assert!(pool.alloc(3).is_none());
        // After dropping _b2 a new alloc should succeed.
        drop(_b2);
        assert!(pool.alloc(99).is_some());
    }

    #[test]
    fn pool_box_deref() {
        let pool: Pool<u32, 1> = Pool::new();
        let b = pool.alloc(42).unwrap();
        assert_eq!(*b, 42);
    }

    #[test]
    fn pool_box_deref_mut() {
        let pool: Pool<u32, 1> = Pool::new();
        let mut b = pool.alloc(10).unwrap();
        *b = 99;
        assert_eq!(*b, 99);
    }

    #[test]
    fn pool_drop_calls_t_drop() {
        static DROP_COUNT: AtomicUsize = AtomicUsize::new(0);

        struct Tracked;
        impl Drop for Tracked {
            fn drop(&mut self) {
                DROP_COUNT.fetch_add(1, Ordering::Relaxed);
            }
        }

        // Ensure counter is reset for this test (tests may run in any order).
        DROP_COUNT.store(0, Ordering::Relaxed);

        let pool: Pool<Tracked, 2> = Pool::new();
        let b1 = pool.alloc(Tracked).unwrap();
        let b2 = pool.alloc(Tracked).unwrap();
        assert_eq!(DROP_COUNT.load(Ordering::Relaxed), 0);

        drop(b1);
        assert_eq!(DROP_COUNT.load(Ordering::Relaxed), 1);
        drop(b2);
        assert_eq!(DROP_COUNT.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn pool_alloc_rejected_val_is_dropped() {
        // When a pool is exhausted, the value passed to alloc() must be dropped.
        static DROP_COUNT: AtomicUsize = AtomicUsize::new(0);

        struct Tracked;
        impl Drop for Tracked {
            fn drop(&mut self) {
                DROP_COUNT.fetch_add(1, Ordering::Relaxed);
            }
        }

        DROP_COUNT.store(0, Ordering::Relaxed);

        let pool: Pool<Tracked, 1> = Pool::new();
        let _b = pool.alloc(Tracked).unwrap(); // succeeds, now full
        let rejected = pool.alloc(Tracked); // fails → value must be dropped
        assert!(rejected.is_none());
        assert_eq!(
            DROP_COUNT.load(Ordering::Relaxed),
            1,
            "rejected value not dropped"
        );
    }
}
