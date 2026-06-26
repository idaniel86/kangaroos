use core::cell::UnsafeCell;
use core::mem::MaybeUninit;

use crate::kernel::scheduler;

// ---------------------------------------------------------------------------
// Private type-erasure traits
//
// `Channel<T, N>` implements both, letting `Sender<'a, T>` and
// `Receiver<'a, T>` hold fat-pointer references (`&'a dyn …`) without
// carrying the capacity constant `N` in their types.
// ---------------------------------------------------------------------------

trait SendChannel<T> {
    fn send_blocking(&self, val: T);
    fn try_send_now(&self, val: T) -> bool;
}

trait RecvChannel<T> {
    fn recv_blocking(&self) -> T;
    fn try_recv_now(&self) -> Option<T>;
}

// ---------------------------------------------------------------------------
// Internal storage
// ---------------------------------------------------------------------------

struct ChannelInner<T, const N: usize> {
    buf: [MaybeUninit<T>; N],
    head: usize,
    tail: usize,
    count: usize,
    /// Head of the blocked-senders intrusive wait list (`0xFF` = empty).
    /// Invariant: `send_head != 0xFF` ⟹ `count == N` (buffer full).
    send_head: u8,
    /// Head of the blocked-receivers intrusive wait list (`0xFF` = empty).
    /// Invariant: `recv_head != 0xFF` ⟹ `count == 0` (buffer empty).
    recv_head: u8,
}

/// A statically-allocated MPMC bounded channel.
///
/// Declare as a `static` and obtain typed send/receive handles via
/// [`Channel::sender`] and [`Channel::receiver`]:
///
/// ```ignore
/// static CH: Channel<u32, 8> = Channel::new();
///
/// // In setup / task bodies:
/// let tx = CH.sender();    // Sender<'_, u32>  — Copy + Clone
/// let rx = CH.receiver();  // Receiver<'_, u32> — Copy + Clone
///
/// tx.send(42);
/// let v = rx.recv();
/// ```
pub struct Channel<T, const N: usize>(UnsafeCell<ChannelInner<T, N>>);

// SAFETY: single-core Cortex-M; all mutations are guarded by `interrupt::free`.
unsafe impl<T: Send, const N: usize> Sync for Channel<T, N> {}

impl<T: Send, const N: usize> Channel<T, N> {
    /// Create an empty channel.  `const fn` so it can initialise a `static`.
    ///
    /// # Panics (compile time)
    /// Panics if `N > 254`. The blocked-sender/receiver wait-lists use `u8`
    /// indices with `0xFF` (255) as the empty-list sentinel.
    pub const fn new() -> Self {
        assert!(N <= 254, "Channel<T, N>: N must be \u{2264} 254 (0xFF is the wait-list sentinel)");
        Channel(UnsafeCell::new(ChannelInner {
            buf: [const { MaybeUninit::uninit() }; N],
            head: 0,
            tail: 0,
            count: 0,
            send_head: 0xFF,
            recv_head: 0xFF,
        }))
    }

    /// Return a [`Sender`] handle that borrows this channel.
    ///
    /// `Sender` is `Copy + Clone`; multiple tasks may hold independent copies.
    pub fn sender(&self) -> Sender<'_, T> {
        Sender { inner: self }
    }

    /// Return a [`Receiver`] handle that borrows this channel.
    ///
    /// `Receiver` is `Copy + Clone`; multiple tasks may hold independent copies.
    pub fn receiver(&self) -> Receiver<'_, T> {
        Receiver { inner: self }
    }

    // -----------------------------------------------------------------------
    // Core implementation — called through the trait vtable
    // -----------------------------------------------------------------------

    fn send_impl(&self, val: T) {
        // Wrap in MaybeUninit to prevent automatic drop in all paths:
        //   • fast path  — value is moved out via ptr::copy_nonoverlapping
        //   • block path — value sits on the frozen stack; receiver copies it
        // In all cases MaybeUninit falls out of scope without calling T::drop.
        let src = MaybeUninit::new(val);
        let mut must_block = false;
        let mut need_preempt = false;

        crate::port::interrupt_free(|| unsafe {
            let inner = &mut *self.0.get();

            if inner.recv_head != 0xFF {
                // Direct handoff to the highest-priority blocked receiver.
                // The receiver parked a pointer to its stack slot in wait_ptr.
                let recv_idx = scheduler::wait_list_pop_highest(&mut inner.recv_head);
                let dst = crate::ktask(recv_idx).wait_ptr as *mut T;
                core::ptr::copy_nonoverlapping(src.as_ptr(), dst, 1);
                if scheduler::unblock(recv_idx) {
                    need_preempt = true;
                }
            } else if inner.count < N {
                // Room in the ring buffer.
                core::ptr::copy_nonoverlapping(
                    src.as_ptr(),
                    inner.buf[inner.tail].as_mut_ptr(),
                    1,
                );
                inner.tail = (inner.tail + 1) % N;
                inner.count += 1;
            } else {
                // Buffer full: park this sender.
                // Store the address of `src` (on our stack) so that a future
                // recv() can copy the value out when space opens up.
                // SAFETY: `src` lives for the lifetime of send_impl; the task
                // is blocked (stack frozen) until the receiver copies it out.
                crate::ktask(crate::CURRENT_TASK).wait_ptr = src.as_ptr() as usize;
                scheduler::wait_list_push(&mut inner.send_head, crate::CURRENT_TASK);
                scheduler::block_current();
                must_block = true;
            }
        });

        if need_preempt || must_block {
            crate::port::trigger_pendsv();
        }
        // `src` (MaybeUninit<T>) drops here without calling T::drop — correct:
        //   • direct handoff / buffer write: value was bitwise-moved to destination
        //   • block: value was bitwise-copied by the receiver while we were parked
    }

    fn try_send_impl(&self, val: T) -> bool {
        // Use MaybeUninit so we can do a raw ownership transfer in the success
        // paths without T::drop running on the source.
        let src = MaybeUninit::new(val);
        let mut sent = false;
        let mut need_preempt = false;

        crate::port::interrupt_free(|| unsafe {
            let inner = &mut *self.0.get();

            if inner.recv_head != 0xFF {
                let recv_idx = scheduler::wait_list_pop_highest(&mut inner.recv_head);
                let dst = crate::ktask(recv_idx).wait_ptr as *mut T;
                core::ptr::copy_nonoverlapping(src.as_ptr(), dst, 1);
                if scheduler::unblock(recv_idx) {
                    need_preempt = true;
                }
                sent = true;
            } else if inner.count < N {
                core::ptr::copy_nonoverlapping(
                    src.as_ptr(),
                    inner.buf[inner.tail].as_mut_ptr(),
                    1,
                );
                inner.tail = (inner.tail + 1) % N;
                inner.count += 1;
                sent = true;
            }
            // else: buffer full — `sent` stays false
        });

        if need_preempt {
            crate::port::trigger_pendsv();
        }

        if !sent {
            // Value was not consumed: re-take ownership so T::drop runs normally.
            drop(unsafe { src.assume_init() });
        }
        // else: MaybeUninit falls out of scope without dropping — value was moved.

        sent
    }

    fn recv_impl(&self) -> T {
        // Allocate the destination slot upfront on the stack.
        // In the block path its address is registered as wait_ptr so a future
        // sender can write directly into it via direct handoff.
        let mut slot = MaybeUninit::<T>::uninit();
        let mut got_it = false;
        let mut must_block = false;
        let mut need_preempt = false;

        crate::port::interrupt_free(|| unsafe {
            let inner = &mut *self.0.get();

            if inner.count > 0 {
                // Pop one item from the ring buffer into `slot`.
                core::ptr::copy_nonoverlapping(
                    inner.buf[inner.head].as_ptr(),
                    slot.as_mut_ptr(),
                    1,
                );
                inner.head = (inner.head + 1) % N;
                inner.count -= 1;

                // The freed slot may now admit one blocked sender.
                if inner.send_head != 0xFF {
                    let sender_idx = scheduler::wait_list_pop_highest(&mut inner.send_head);
                    let src = crate::ktask(sender_idx).wait_ptr as *const T;
                    core::ptr::copy_nonoverlapping(src, inner.buf[inner.tail].as_mut_ptr(), 1);
                    inner.tail = (inner.tail + 1) % N;
                    inner.count += 1;
                    if scheduler::unblock(sender_idx) {
                        need_preempt = true;
                    }
                }

                got_it = true;
            } else {
                // Buffer empty: park this receiver.
                // Store the address of `slot` so a future sender can write
                // directly into it (direct handoff path in send_impl).
                // SAFETY: `slot` lives for the lifetime of recv_impl; the task
                // is blocked (stack frozen) until the sender fills it.
                crate::ktask(crate::CURRENT_TASK).wait_ptr = slot.as_mut_ptr() as usize;
                scheduler::wait_list_push(&mut inner.recv_head, crate::CURRENT_TASK);
                scheduler::block_current();
                must_block = true;
            }
        });

        if need_preempt {
            crate::port::trigger_pendsv();
        }

        if must_block {
            crate::port::trigger_pendsv();
            // Resume here: a sender has written into `slot` via our wait_ptr.
            return unsafe { slot.assume_init() };
        }

        debug_assert!(got_it);
        unsafe { slot.assume_init() }
    }

    fn try_recv_impl(&self) -> Option<T> {
        let mut slot = MaybeUninit::<T>::uninit();
        let mut got_it = false;
        let mut need_preempt = false;

        crate::port::interrupt_free(|| unsafe {
            let inner = &mut *self.0.get();

            if inner.count > 0 {
                core::ptr::copy_nonoverlapping(
                    inner.buf[inner.head].as_ptr(),
                    slot.as_mut_ptr(),
                    1,
                );
                inner.head = (inner.head + 1) % N;
                inner.count -= 1;

                if inner.send_head != 0xFF {
                    let sender_idx = scheduler::wait_list_pop_highest(&mut inner.send_head);
                    let src = crate::ktask(sender_idx).wait_ptr as *const T;
                    core::ptr::copy_nonoverlapping(src, inner.buf[inner.tail].as_mut_ptr(), 1);
                    inner.tail = (inner.tail + 1) % N;
                    inner.count += 1;
                    if scheduler::unblock(sender_idx) {
                        need_preempt = true;
                    }
                }

                got_it = true;
            }
        });

        if need_preempt {
            crate::port::trigger_pendsv();
        }

        if got_it { Some(unsafe { slot.assume_init() }) } else { None }
    }
}

// ---------------------------------------------------------------------------
// Trait impls — bridge Channel<T,N> to the type-erased handles
// ---------------------------------------------------------------------------

impl<T: Send, const N: usize> SendChannel<T> for Channel<T, N> {
    fn send_blocking(&self, val: T) { self.send_impl(val) }
    fn try_send_now(&self, val: T) -> bool { self.try_send_impl(val) }
}

impl<T: Send, const N: usize> RecvChannel<T> for Channel<T, N> {
    fn recv_blocking(&self) -> T { self.recv_impl() }
    fn try_recv_now(&self) -> Option<T> { self.try_recv_impl() }
}

// ---------------------------------------------------------------------------
// Public split handles
// ---------------------------------------------------------------------------

/// Sending half of a [`Channel`].
///
/// `Copy + Clone` — multiple tasks may hold independent `Sender` copies
/// (MPMC semantics).  Each handle is a fat pointer (2 words) with one
/// indirect vtable call per operation.
#[derive(Copy, Clone)]
pub struct Sender<'a, T> {
    inner: &'a dyn SendChannel<T>,
}

/// Receiving half of a [`Channel`].
///
/// `Copy + Clone` — multiple tasks may hold independent `Receiver` copies
/// (MPMC semantics).
#[derive(Copy, Clone)]
pub struct Receiver<'a, T> {
    inner: &'a dyn RecvChannel<T>,
}

impl<'a, T> Sender<'a, T> {
    /// Send `val`, blocking the calling task until a slot is available.
    ///
    /// Must not be called from interrupt handlers.
    pub fn send(&self, val: T) {
        self.inner.send_blocking(val);
    }

    /// Attempt to send `val` without blocking.
    ///
    /// Returns `true` if the value was enqueued (or handed off to a waiting
    /// receiver), `false` if the channel was full.  `val` is dropped on
    /// failure.  Safe to call from interrupt handlers.
    pub fn try_send(&self, val: T) -> bool {
        self.inner.try_send_now(val)
    }
}

impl<'a, T> Receiver<'a, T> {
    /// Receive a value, blocking the calling task until one is available.
    ///
    /// Must not be called from interrupt handlers.
    pub fn recv(&self) -> T {
        self.inner.recv_blocking()
    }

    /// Attempt to receive a value without blocking.
    ///
    /// Returns `Some(val)` if an item was available, `None` otherwise.
    /// Safe to call from interrupt handlers.
    pub fn try_recv(&self) -> Option<T> {
        self.inner.try_recv_now()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::Channel;
    use core::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn channel_try_recv_empty_returns_none() {
        let ch: Channel<u32, 4> = Channel::new();
        assert_eq!(ch.receiver().try_recv(), None);
    }

    #[test]
    fn channel_try_send_full_returns_false() {
        let ch: Channel<u32, 2> = Channel::new();
        let tx = ch.sender();
        assert!(tx.try_send(1));
        assert!(tx.try_send(2));
        assert!(!tx.try_send(3)); // full
    }

    #[test]
    fn channel_fifo_order() {
        let ch: Channel<u32, 4> = Channel::new();
        let tx = ch.sender();
        let rx = ch.receiver();

        assert!(tx.try_send(10));
        assert!(tx.try_send(20));
        assert!(tx.try_send(30));

        assert_eq!(rx.try_recv(), Some(10));
        assert_eq!(rx.try_recv(), Some(20));
        assert_eq!(rx.try_recv(), Some(30));
        assert_eq!(rx.try_recv(), None);
    }

    #[test]
    fn channel_ring_wrap_around() {
        // Fill 2-slot channel, drain one, fill again — tests head/tail wrap.
        let ch: Channel<u32, 2> = Channel::new();
        let tx = ch.sender();
        let rx = ch.receiver();

        assert!(tx.try_send(1));
        assert!(tx.try_send(2));
        assert_eq!(rx.try_recv(), Some(1)); // head advances to slot 1
        assert!(tx.try_send(3));            // tail wraps back to slot 0
        assert_eq!(rx.try_recv(), Some(2));
        assert_eq!(rx.try_recv(), Some(3));
        assert_eq!(rx.try_recv(), None);
    }

    #[test]
    fn channel_rejected_send_drops_value() {
        // When try_send fails (full), the value must be dropped immediately.
        static DROP_COUNT: AtomicUsize = AtomicUsize::new(0);

        struct Tracked;
        impl Drop for Tracked {
            fn drop(&mut self) {
                DROP_COUNT.fetch_add(1, Ordering::Relaxed);
            }
        }

        DROP_COUNT.store(0, Ordering::Relaxed);

        let ch: Channel<Tracked, 1> = Channel::new();
        let tx = ch.sender();
        let rx = ch.receiver();

        assert!(tx.try_send(Tracked));  // buffered
        assert!(!tx.try_send(Tracked)); // full → rejected and dropped
        assert_eq!(DROP_COUNT.load(Ordering::Relaxed), 1, "rejected value not dropped");

        drop(rx.try_recv()); // consume buffered item
        assert_eq!(DROP_COUNT.load(Ordering::Relaxed), 2, "buffered value not dropped on recv+drop");
    }

    #[test]
    fn channel_sender_receiver_are_copy() {
        let ch: Channel<u32, 4> = Channel::new();
        let tx = ch.sender();
        let tx2 = tx; // Copy
        let rx = ch.receiver();
        let rx2 = rx; // Copy

        assert!(tx2.try_send(42));
        assert_eq!(rx2.try_recv(), Some(42));
    }
}
