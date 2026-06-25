use core::cell::UnsafeCell;

use crate::kernel::scheduler;

struct EventGroupInner {
    /// Current 32-bit flag state.
    bits: u32,
    /// Head of the intrusive wait list for [`wait_any`] callers. `0xFF` = empty.
    wait_any_head: u8,
    /// Head of the intrusive wait list for [`wait_all`] callers. `0xFF` = empty.
    wait_all_head: u8,
}

/// A 32-bit event flag group.
///
/// Tasks can block until any or all bits in a specified mask are set.
/// Each bit is independent; multiple tasks may wait on different subsets
/// simultaneously.
///
/// Matched bits are **consumed** (cleared) when a waiting task is unblocked,
/// preventing the same event from satisfying two callers waiting on the same bit.
///
/// ```ignore
/// static EG: EventGroup = EventGroup::new();
///
/// // Task A: wait for bit 0 OR bit 1
/// let matched = EG.wait_any(0b11);
///
/// // Task B: wait for bits 0 AND 1
/// EG.wait_all(0b11);
///
/// // Producer (possibly from an ISR via set_from_isr):
/// EG.set(0b01);
/// ```
pub struct EventGroup(UnsafeCell<EventGroupInner>);

// SAFETY: single-core Cortex-M; all mutations are guarded by `interrupt::free`.
unsafe impl Sync for EventGroup {}
unsafe impl Send for EventGroup {}

impl EventGroup {
    /// Create a new `EventGroup` with all bits cleared.
    pub const fn new() -> Self {
        EventGroup(UnsafeCell::new(EventGroupInner {
            bits: 0,
            wait_any_head: 0xFF,
            wait_all_head: 0xFF,
        }))
    }

    /// Set one or more bits, potentially waking blocked tasks.
    ///
    /// For each blocked task whose condition is now met, the matching bits are
    /// cleared from the group and the task is unblocked. Returns the bit state
    /// after all wakeup-driven clears have been applied.
    ///
    /// Safe to call from both task and ISR context.
    pub fn set(&self, mask: u32) -> u32 {
        #[cfg(feature = "defmt")]
        defmt::debug!("event_group: set mask={=u32:#x}", mask);
        let (new_bits, need_preempt) = cortex_m::interrupt::free(|_| unsafe {
            let inner = &mut *self.0.get();
            inner.bits |= mask;
            let mut preempt = false;

            // --- Scan wait_any list ---
            // Unblock any task whose mask has at least one bit overlapping the
            // current bit state.  Clear the matched bits so they are not
            // delivered twice.
            let mut prev: u8 = 0xFF; // 0xFF → previous is the list head
            let mut cur = inner.wait_any_head;
            while cur != 0xFF {
                let idx = cur as usize;
                let next = crate::ktask(idx).wait_next;
                let task_mask = crate::ktask(idx).wait_ptr as u32;
                let matched = inner.bits & task_mask;
                if matched != 0 {
                    // Remove this node from the list.
                    if prev == 0xFF {
                        inner.wait_any_head = next;
                    } else {
                        crate::ktask(prev as usize).wait_next = next;
                    }
                    crate::ktask(idx).wait_next = 0xFF;
                    // Consume the matched bits.
                    inner.bits &= !matched;
                    // Store the matched bits for the task to read on resume.
                    crate::ktask(idx).wait_ptr = matched as usize;
                    if scheduler::unblock(idx) {
                        preempt = true;
                    }
                    #[cfg(feature = "defmt")]
                    defmt::debug!("event_group: wait_any satisfied, woke '{}' matched={=u32:#x}",
                        crate::ktask(idx).name, matched);
                    // prev unchanged — it now links directly to `next`.
                } else {
                    prev = cur;
                }
                cur = next;
            }

            // --- Scan wait_all list ---
            // Unblock any task whose entire mask is now satisfied.
            prev = 0xFF;
            cur = inner.wait_all_head;
            while cur != 0xFF {
                let idx = cur as usize;
                let next = crate::ktask(idx).wait_next;
                let task_mask = crate::ktask(idx).wait_ptr as u32;
                if inner.bits & task_mask == task_mask {
                    if prev == 0xFF {
                        inner.wait_all_head = next;
                    } else {
                        crate::ktask(prev as usize).wait_next = next;
                    }
                    crate::ktask(idx).wait_next = 0xFF;
                    inner.bits &= !task_mask;
                    if scheduler::unblock(idx) {
                        preempt = true;
                    }
                    #[cfg(feature = "defmt")]
                    defmt::debug!("event_group: wait_all satisfied, woke '{}' mask={=u32:#x}",
                        crate::ktask(idx).name, task_mask);
                } else {
                    prev = cur;
                }
                cur = next;
            }

            (inner.bits, preempt)
        });

        if need_preempt {
            cortex_m::peripheral::SCB::set_pendsv();
        }

        new_bits
    }

    /// Clear one or more bits without waking any waiting tasks.
    pub fn clear(&self, mask: u32) {
        cortex_m::interrupt::free(|_| unsafe {
            (*self.0.get()).bits &= !mask;
        });
    }

    /// Read the current bit state without blocking.
    pub fn get(&self) -> u32 {
        cortex_m::interrupt::free(|_| unsafe { (*self.0.get()).bits })
    }

    /// Block until **any** of the bits in `mask` are set.
    ///
    /// Clears the matched bits and returns them. Must not be called from an
    /// interrupt handler.
    pub fn wait_any(&self, mask: u32) -> u32 {
        let mut must_block = false;

        cortex_m::interrupt::free(|_| unsafe {
            let inner = &mut *self.0.get();
            let matched = inner.bits & mask;
            if matched != 0 {
                // Fast path: at least one requested bit is already set.
                inner.bits &= !matched;
                // Store matched so the post-CS read below works uniformly.
                crate::ktask(crate::CURRENT_TASK).wait_ptr = matched as usize;
            } else {
                // Slow path: store the full requested mask so set() can match.
                #[cfg(feature = "defmt")]
                defmt::debug!("event_group: wait_any blocking, '{}' mask={=u32:#x}",
                    crate::ktask(crate::CURRENT_TASK).name, mask);
                crate::ktask(crate::CURRENT_TASK).wait_ptr = mask as usize;
                scheduler::wait_list_push(&mut inner.wait_any_head, crate::CURRENT_TASK);
                scheduler::block_current();
                must_block = true;
            }
        });

        if must_block {
            // Switch away; set() writes the matched bits to wait_ptr before
            // unblocking, so they are ready when we resume.
            cortex_m::peripheral::SCB::set_pendsv();
        }

        // Return the bits that were matched, written by set() or the fast path.
        // SAFETY: wait_ptr is written inside interrupt::free before unblock(),
        // establishing a happens-before with the task resuming here.
        unsafe { crate::ktask(crate::CURRENT_TASK).wait_ptr as u32 }
    }

    /// Block until **all** of the bits in `mask` are set.
    ///
    /// Clears the bits in `mask` before returning. Must not be called from
    /// an interrupt handler.
    pub fn wait_all(&self, mask: u32) {
        let mut must_block = false;

        cortex_m::interrupt::free(|_| unsafe {
            let inner = &mut *self.0.get();
            if inner.bits & mask == mask {
                // Fast path: all requested bits are already set.
                inner.bits &= !mask;
            } else {
                #[cfg(feature = "defmt")]
                defmt::debug!("event_group: wait_all blocking, '{}' mask={=u32:#x}",
                    crate::ktask(crate::CURRENT_TASK).name, mask);
                crate::ktask(crate::CURRENT_TASK).wait_ptr = mask as usize;
                scheduler::wait_list_push(&mut inner.wait_all_head, crate::CURRENT_TASK);
                scheduler::block_current();
                must_block = true;
            }
        });

        if must_block {
            cortex_m::peripheral::SCB::set_pendsv();
        }
    }

    /// Set bits from an interrupt handler without triggering PendSV directly.
    ///
    /// Returns `true` if a higher-priority task was unblocked and the caller
    /// should trigger a context switch (e.g. by calling `SCB::set_pendsv()`
    /// before returning from the ISR).
    ///
    /// # Safety
    /// Must be called from an interrupt handler or inside `interrupt::free`.
    pub unsafe fn set_from_isr(&self, mask: u32) -> bool {
        cortex_m::interrupt::free(|_| unsafe {
            let inner = &mut *self.0.get();
            inner.bits |= mask;
            let mut preempt = false;

            // Scan wait_any
            let mut prev: u8 = 0xFF;
            let mut cur = inner.wait_any_head;
            while cur != 0xFF {
                let idx = cur as usize;
                let next = crate::ktask(idx).wait_next;
                let task_mask = crate::ktask(idx).wait_ptr as u32;
                let matched = inner.bits & task_mask;
                if matched != 0 {
                    if prev == 0xFF {
                        inner.wait_any_head = next;
                    } else {
                        crate::ktask(prev as usize).wait_next = next;
                    }
                    crate::ktask(idx).wait_next = 0xFF;
                    inner.bits &= !matched;
                    crate::ktask(idx).wait_ptr = matched as usize;
                    if scheduler::unblock(idx) {
                        preempt = true;
                    }
                } else {
                    prev = cur;
                }
                cur = next;
            }

            // Scan wait_all
            prev = 0xFF;
            cur = inner.wait_all_head;
            while cur != 0xFF {
                let idx = cur as usize;
                let next = crate::ktask(idx).wait_next;
                let task_mask = crate::ktask(idx).wait_ptr as u32;
                if inner.bits & task_mask == task_mask {
                    if prev == 0xFF {
                        inner.wait_all_head = next;
                    } else {
                        crate::ktask(prev as usize).wait_next = next;
                    }
                    crate::ktask(idx).wait_next = 0xFF;
                    inner.bits &= !task_mask;
                    if scheduler::unblock(idx) {
                        preempt = true;
                    }
                } else {
                    prev = cur;
                }
                cur = next;
            }

            preempt
        })
    }
}
