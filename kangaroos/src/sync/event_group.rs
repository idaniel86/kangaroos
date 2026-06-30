use core::cell::UnsafeCell;

use crate::kernel::scheduler;
use crate::kernel::tcb::{TaskState, Tcb};

struct EventGroupInner {
    /// Current 32-bit flag state.
    bits: u32,
    /// Head of the intrusive wait list for [`wait_any`] callers. `null` = empty.
    wait_any_head: *mut Tcb,
    /// Head of the intrusive wait list for [`wait_all`] callers. `null` = empty.
    wait_all_head: *mut Tcb,
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
pub struct EventGroup {
    inner: UnsafeCell<EventGroupInner>,
    /// Optional human-readable name. `None` when constructed with [`EventGroup::new`];
    /// set by [`EventGroup::new_named`] or the [`event_group!`] macro.
    pub name: Option<&'static str>,
}

// SAFETY: single-core Cortex-M; all mutations are guarded by `interrupt::free`.
unsafe impl Sync for EventGroup {}
unsafe impl Send for EventGroup {}

impl Default for EventGroup {
    fn default() -> Self {
        Self::new()
    }
}

impl EventGroup {
    /// Create a new unnamed `EventGroup` with all bits cleared. Prefer the
    /// [`event_group!`] macro for named statics.
    pub const fn new() -> Self {
        EventGroup {
            inner: UnsafeCell::new(EventGroupInner {
                bits: 0,
                wait_any_head: core::ptr::null_mut(),
                wait_all_head: core::ptr::null_mut(),
            }),
            name: None,
        }
    }

    /// Create a named `EventGroup`. Called by the [`event_group!`] macro;
    /// prefer that macro over calling this directly.
    pub const fn new_named(name: &'static str) -> Self {
        EventGroup {
            inner: UnsafeCell::new(EventGroupInner {
                bits: 0,
                wait_any_head: core::ptr::null_mut(),
                wait_all_head: core::ptr::null_mut(),
            }),
            name: Some(name),
        }
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
        let id = super::PrimName(self.name, self as *const _ as u32);
        #[cfg(feature = "defmt")]
        defmt::debug!("event_group {}: set mask={=u32:#x}", id, mask);
        let (new_bits, need_preempt) = crate::port::interrupt_free(|| unsafe {
            let inner = &mut *self.inner.get();
            inner.bits |= mask;
            let mut preempt = false;

            // --- Scan wait_any list ---
            // Unblock any task whose mask has at least one bit overlapping the
            // current bit state.  Clear the matched bits so they are not
            // delivered twice.
            let mut prev: *mut Tcb = core::ptr::null_mut();
            let mut cur = inner.wait_any_head;
            while !cur.is_null() {
                let TaskState::Blocked { wait_next: next } = (*cur).state else {
                    unreachable!()
                };
                let task_mask = (*cur).wait_ptr as u32;
                let matched = inner.bits & task_mask;
                if matched != 0 {
                    // Remove this node from the list.
                    if prev.is_null() {
                        inner.wait_any_head = next;
                    } else if let TaskState::Blocked { ref mut wait_next } = (*prev).state {
                        *wait_next = next;
                    }
                    // Matched node transitions to Ready via unblock(); the
                    // Blocked payload (wait_next) is implicitly discarded.
                    // Consume the matched bits.
                    inner.bits &= !matched;
                    // Store the matched bits for the task to read on resume.
                    (*cur).wait_ptr = matched as usize;
                    if scheduler::unblock(cur) {
                        preempt = true;
                    }
                    #[cfg(feature = "defmt")]
                    defmt::debug!(
                        "event_group {}: wait_any satisfied, woke '{}' matched={=u32:#x}",
                        id,
                        (*cur).name,
                        matched
                    );
                    // prev unchanged — it now links directly to `next`.
                } else {
                    prev = cur;
                }
                cur = next;
            }

            // --- Scan wait_all list ---
            // Unblock any task whose entire mask is now satisfied.
            prev = core::ptr::null_mut();
            cur = inner.wait_all_head;
            while !cur.is_null() {
                let TaskState::Blocked { wait_next: next } = (*cur).state else {
                    unreachable!()
                };
                let task_mask = (*cur).wait_ptr as u32;
                if inner.bits & task_mask == task_mask {
                    if prev.is_null() {
                        inner.wait_all_head = next;
                    } else if let TaskState::Blocked { ref mut wait_next } = (*prev).state {
                        *wait_next = next;
                    }
                    // Matched node transitions to Ready via unblock(); the
                    // Blocked payload (wait_next) is implicitly discarded.
                    inner.bits &= !task_mask;
                    if scheduler::unblock(cur) {
                        preempt = true;
                    }
                    #[cfg(feature = "defmt")]
                    defmt::debug!(
                        "event_group {}: wait_all satisfied, woke '{}' mask={=u32:#x}",
                        id,
                        (*cur).name,
                        task_mask
                    );
                } else {
                    prev = cur;
                }
                cur = next;
            }

            (inner.bits, preempt)
        });

        if need_preempt {
            crate::port::trigger_pendsv();
        }

        new_bits
    }

    /// Clear one or more bits without waking any waiting tasks.
    pub fn clear(&self, mask: u32) {
        crate::port::interrupt_free(|| unsafe {
            (*self.inner.get()).bits &= !mask;
        });
    }

    /// Read the current bit state without blocking.
    pub fn get(&self) -> u32 {
        crate::port::interrupt_free(|| unsafe { (*self.inner.get()).bits })
    }

    /// Block until **any** of the bits in `mask` are set.
    ///
    /// Clears the matched bits and returns them. Must not be called from an
    /// interrupt handler.
    pub fn wait_any(&self, mask: u32) -> u32 {
        let mut must_block = false;
        #[cfg(feature = "defmt")]
        let id = super::PrimName(self.name, self as *const _ as u32);

        crate::port::interrupt_free(|| unsafe {
            let inner = &mut *self.inner.get();
            let matched = inner.bits & mask;
            if matched != 0 {
                // Fast path: at least one requested bit is already set.
                inner.bits &= !matched;
                // Store matched so the post-CS read below works uniformly.
                (*crate::CURRENT).wait_ptr = matched as usize;
            } else {
                // Slow path: store the full requested mask so set() can match.
                #[cfg(feature = "defmt")]
                defmt::debug!(
                    "event_group {}: wait_any blocking, '{}' mask={=u32:#x}",
                    id,
                    (*crate::CURRENT).name,
                    mask
                );
                (*crate::CURRENT).wait_ptr = mask as usize;
                scheduler::block_and_push(&mut inner.wait_any_head, crate::CURRENT);
                must_block = true;
            }
        });

        if must_block {
            // Switch away; set() writes the matched bits to wait_ptr before
            // unblocking, so they are ready when we resume.
            crate::port::trigger_pendsv();
        }

        // Return the bits that were matched, written by set() or the fast path.
        // SAFETY: wait_ptr is written inside interrupt::free before unblock(),
        // establishing a happens-before with the task resuming here.
        unsafe { (*crate::CURRENT).wait_ptr as u32 }
    }

    /// Block until **all** of the bits in `mask` are set.
    ///
    /// Clears the bits in `mask` before returning. Must not be called from
    /// an interrupt handler.
    pub fn wait_all(&self, mask: u32) {
        let mut must_block = false;
        #[cfg(feature = "defmt")]
        let id = super::PrimName(self.name, self as *const _ as u32);

        crate::port::interrupt_free(|| unsafe {
            let inner = &mut *self.inner.get();
            if inner.bits & mask == mask {
                // Fast path: all requested bits are already set.
                inner.bits &= !mask;
            } else {
                #[cfg(feature = "defmt")]
                defmt::debug!(
                    "event_group {}: wait_all blocking, '{}' mask={=u32:#x}",
                    id,
                    (*crate::CURRENT).name,
                    mask
                );
                (*crate::CURRENT).wait_ptr = mask as usize;
                scheduler::block_and_push(&mut inner.wait_all_head, crate::CURRENT);
                must_block = true;
            }
        });

        if must_block {
            crate::port::trigger_pendsv();
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
        crate::port::interrupt_free(|| unsafe {
            let inner = &mut *self.inner.get();
            inner.bits |= mask;
            let mut preempt = false;

            // Scan wait_any
            let mut prev: *mut Tcb = core::ptr::null_mut();
            let mut cur = inner.wait_any_head;
            while !cur.is_null() {
                let TaskState::Blocked { wait_next: next } = (*cur).state else {
                    unreachable!()
                };
                let task_mask = (*cur).wait_ptr as u32;
                let matched = inner.bits & task_mask;
                if matched != 0 {
                    if prev.is_null() {
                        inner.wait_any_head = next;
                    } else if let TaskState::Blocked { ref mut wait_next } = (*prev).state {
                        *wait_next = next;
                    }
                    // Matched node transitions to Ready via unblock(); the
                    // Blocked payload (wait_next) is implicitly discarded.
                    inner.bits &= !matched;
                    (*cur).wait_ptr = matched as usize;
                    if scheduler::unblock(cur) {
                        preempt = true;
                    }
                } else {
                    prev = cur;
                }
                cur = next;
            }

            // Scan wait_all
            prev = core::ptr::null_mut();
            cur = inner.wait_all_head;
            while !cur.is_null() {
                let TaskState::Blocked { wait_next: next } = (*cur).state else {
                    unreachable!()
                };
                let task_mask = (*cur).wait_ptr as u32;
                if inner.bits & task_mask == task_mask {
                    if prev.is_null() {
                        inner.wait_all_head = next;
                    } else if let TaskState::Blocked { ref mut wait_next } = (*prev).state {
                        *wait_next = next;
                    }
                    // Matched node transitions to Ready via unblock(); the
                    // Blocked payload (wait_next) is implicitly discarded.
                    inner.bits &= !task_mask;
                    if scheduler::unblock(cur) {
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::EventGroup;

    #[test]
    fn event_group_initial_zero() {
        let eg = EventGroup::new();
        assert_eq!(eg.get(), 0);
    }

    #[test]
    fn event_group_set_accumulates_bits() {
        let eg = EventGroup::new();
        // With empty wait lists set() just ORs the bits in and returns the new value.
        let bits = eg.set(0b0001);
        assert_eq!(bits, 0b0001);
        let bits = eg.set(0b0010);
        assert_eq!(bits, 0b0011);
        assert_eq!(eg.get(), 0b0011);
    }

    #[test]
    fn event_group_clear_masks_bits() {
        let eg = EventGroup::new();
        eg.set(0b1111);
        eg.clear(0b0101);
        assert_eq!(eg.get(), 0b1010);
    }

    #[test]
    fn event_group_set_returns_post_clear_state() {
        // With no waiters the returned value == the new bit state.
        let eg = EventGroup::new();
        eg.set(0xFF);
        let after = eg.set(0x00); // no new bits — old bits remain
        assert_eq!(after, 0xFF);
    }

    #[test]
    fn event_group_clear_all() {
        let eg = EventGroup::new();
        eg.set(0xFFFF_FFFF);
        eg.clear(0xFFFF_FFFF);
        assert_eq!(eg.get(), 0);
    }

    #[test]
    fn event_group_independent_bits() {
        // Setting one bit must not disturb others.
        let eg = EventGroup::new();
        eg.set(1 << 0);
        eg.set(1 << 15);
        eg.set(1 << 31);
        assert_eq!(eg.get(), (1 << 0) | (1 << 15) | (1 << 31));
        eg.clear(1 << 15);
        assert_eq!(eg.get(), (1 << 0) | (1 << 31));
    }
}
