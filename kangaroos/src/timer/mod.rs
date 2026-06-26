//! Time primitives: [`Duration`], [`Instant`], and [`Timer`].
//!
//! All time values are measured in SysTick ticks. The tick rate is determined
//! by the SysTick reload value configured before [`kernel::start`] is called.
//! [`TICKS_PER_SEC`] must match that configuration; it defaults to 1000 (1 kHz).

use core::ops::{Add, AddAssign, Sub};

/// Number of SysTick interrupts per second.
///
/// This must match the reload value used when configuring SysTick.
/// The default of 1000 corresponds to a 1 kHz tick (1 tick = 1 ms).
pub const TICKS_PER_SEC: u64 = 1000;

// ---------------------------------------------------------------------------
// Duration
// ---------------------------------------------------------------------------

/// A span of time measured in SysTick ticks.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Duration(u64);

impl Duration {
    /// Zero duration.
    pub const ZERO: Duration = Duration(0);

    /// Construct from a raw tick count.
    pub const fn from_ticks(ticks: u64) -> Self {
        Duration(ticks)
    }

    /// Construct from milliseconds (rounded down to the nearest tick).
    pub const fn from_millis(ms: u64) -> Self {
        Duration(ms * TICKS_PER_SEC / 1000)
    }

    /// Construct from whole seconds.
    pub const fn from_secs(secs: u64) -> Self {
        Duration(secs * TICKS_PER_SEC)
    }

    /// Return the raw tick count.
    pub const fn as_ticks(self) -> u64 {
        self.0
    }

    /// Return the duration in whole milliseconds (rounded down).
    pub const fn as_millis(self) -> u64 {
        self.0 * 1000 / TICKS_PER_SEC
    }

    /// Return the duration in whole seconds (rounded down).
    pub const fn as_secs(self) -> u64 {
        self.0 / TICKS_PER_SEC
    }
}

impl Add for Duration {
    type Output = Duration;
    fn add(self, rhs: Duration) -> Duration {
        Duration(self.0 + rhs.0)
    }
}

impl Sub for Duration {
    type Output = Duration;
    /// Subtract two durations, saturating at zero rather than panicking on
    /// underflow. Use [`Instant::checked_duration_since`] or
    /// [`Instant::saturating_duration_since`] when the sign is uncertain.
    fn sub(self, rhs: Duration) -> Duration {
        Duration(self.0.saturating_sub(rhs.0))
    }
}

impl AddAssign for Duration {
    fn add_assign(&mut self, rhs: Duration) {
        self.0 += rhs.0;
    }
}

// ---------------------------------------------------------------------------
// Instant
// ---------------------------------------------------------------------------

/// A point in time, represented as a SysTick tick count since boot.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Instant(u64);

impl Instant {
    /// Return the current time.
    ///
    /// Reads the global tick counter inside a critical section to prevent a
    /// torn read on 32-bit architectures.
    ///
    /// # 32-bit torn-read protection
    /// `TICK` is a `u64`. On 32-bit Cortex-M, a bare `u64` load compiles to
    /// two 32-bit `LDR` instructions. If SysTick fires between them and the
    /// low word overflows into the high word, the caller would see a torn
    /// (inconsistent) value. `interrupt::free` sets `PRIMASK = 1`, which
    /// blocks all configurable-priority exceptions — including SysTick even
    /// when it is configured at priority 0x00 — for the duration of the read.
    pub fn now() -> Instant {
        crate::port::interrupt_free(|| {
            // SAFETY: TICK is only written from the SysTick handler; reading it
            // inside interrupt::free guarantees a non-torn u64 read on all
            // Cortex-M variants (PRIMASK blocks SysTick regardless of its
            // configured priority value).
            Instant(unsafe { crate::kernel::scheduler::TICK })
        })
    }

    /// Return the duration elapsed since this instant.
    pub fn elapsed(self) -> Duration {
        Instant::now().saturating_duration_since(self)
    }

    /// Return the duration from `earlier` to `self`.
    ///
    /// Returns `None` if `self` is before `earlier` (e.g. tick counter
    /// wrapped, which cannot happen in any realistic uptime).
    pub fn checked_duration_since(self, earlier: Instant) -> Option<Duration> {
        self.0.checked_sub(earlier.0).map(Duration)
    }

    /// Return the duration from `earlier` to `self`, saturating at zero.
    pub fn saturating_duration_since(self, earlier: Instant) -> Duration {
        Duration(self.0.saturating_sub(earlier.0))
    }
}

// ---------------------------------------------------------------------------
// Timer
// ---------------------------------------------------------------------------

/// A one-shot or periodic timer that blocks the calling task until a deadline.
///
/// `Timer` tracks an *absolute* next-deadline rather than sleeping relative
/// to the current time, which gives drift-free periodic intervals:
///
/// ```ignore
/// let mut t = Timer::every(Duration::from_millis(10));
/// loop {
///     t.wait();        // deadline advances by exactly 10 ms each call
///     do_work();       // processing time does not accumulate as jitter
/// }
/// ```
pub struct Timer {
    /// Absolute tick count of the next deadline.
    next: u64,
    /// Period in ticks for periodic timers; `None` for one-shot.
    period: Option<u64>,
}

impl Timer {
    /// Create a one-shot timer that fires once after `delay`.
    pub fn after(delay: Duration) -> Timer {
        let now = crate::port::interrupt_free(|| unsafe { crate::kernel::scheduler::TICK });
        Timer {
            next: now.wrapping_add(delay.as_ticks()),
            period: None,
        }
    }

    /// Create a periodic timer that fires every `period`, starting `period`
    /// from now.
    pub fn every(period: Duration) -> Timer {
        let ticks = period.as_ticks();
        let now = crate::port::interrupt_free(|| unsafe { crate::kernel::scheduler::TICK });
        Timer {
            next: now.wrapping_add(ticks),
            period: Some(ticks),
        }
    }

    /// Block until the next deadline, then advance the deadline by one period.
    ///
    /// For a periodic timer this produces drift-free intervals. For a one-shot
    /// timer the deadline is not advanced after the first `wait()` completes;
    /// subsequent calls return immediately if the deadline is in the past.
    pub fn wait(&mut self) {
        crate::task::sleep_until(self.next);
        if let Some(p) = self.period {
            self.next = self.next.wrapping_add(p);
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::{Duration, Instant, TICKS_PER_SEC};
    use std::sync::Mutex;

    // Serialize tests that manipulate the global TICK counter so that
    // parallel test threads do not observe each other's writes.
    static TICK_LOCK: Mutex<()> = Mutex::new(());

    // --- Duration ---

    #[test]
    fn duration_zero() {
        assert_eq!(Duration::ZERO.as_ticks(), 0);
    }

    #[test]
    fn duration_from_ticks_round_trip() {
        assert_eq!(Duration::from_ticks(42).as_ticks(), 42);
    }

    #[test]
    fn duration_from_millis() {
        // At TICKS_PER_SEC = 1000: 1 ms == 1 tick.
        assert_eq!(Duration::from_millis(1).as_ticks(), 1);
        assert_eq!(Duration::from_millis(1000).as_ticks(), TICKS_PER_SEC);
    }

    #[test]
    fn duration_from_secs() {
        assert_eq!(Duration::from_secs(1).as_ticks(), TICKS_PER_SEC);
        assert_eq!(Duration::from_secs(5).as_ticks(), 5 * TICKS_PER_SEC);
    }

    #[test]
    fn duration_as_millis() {
        assert_eq!(Duration::from_ticks(500).as_millis(), 500);
        assert_eq!(Duration::from_ticks(TICKS_PER_SEC).as_millis(), 1000);
    }

    #[test]
    fn duration_as_secs() {
        assert_eq!(Duration::from_secs(3).as_secs(), 3);
        // Sub-second ticks round down to zero.
        assert_eq!(Duration::from_ticks(TICKS_PER_SEC - 1).as_secs(), 0);
    }

    #[test]
    fn duration_add() {
        let a = Duration::from_ticks(10);
        let b = Duration::from_ticks(5);
        assert_eq!((a + b).as_ticks(), 15);
    }

    #[test]
    fn duration_sub_normal() {
        assert_eq!((Duration::from_ticks(10) - Duration::from_ticks(3)).as_ticks(), 7);
    }

    #[test]
    fn duration_sub_saturates_at_zero() {
        // Underflow must saturate, not panic or wrap.
        assert_eq!((Duration::from_ticks(3) - Duration::from_ticks(10)).as_ticks(), 0);
    }

    #[test]
    fn duration_add_assign() {
        let mut d = Duration::from_ticks(10);
        d += Duration::from_ticks(5);
        assert_eq!(d.as_ticks(), 15);
    }

    #[test]
    fn duration_ordering() {
        assert!(Duration::from_ticks(5) < Duration::from_ticks(10));
        assert!(Duration::from_ticks(10) > Duration::from_ticks(5));
        assert_eq!(Duration::from_ticks(7), Duration::from_ticks(7));
    }

    // --- Instant ---

    #[test]
    fn instant_checked_duration_since() {
        let _g = TICK_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { crate::kernel::scheduler::TICK = 200; }
        let t1 = Instant::now();
        unsafe { crate::kernel::scheduler::TICK = 350; }
        let t2 = Instant::now();

        assert_eq!(t2.checked_duration_since(t1).unwrap().as_ticks(), 150);
        // t1 is after t2 in logical time → None.
        assert!(t1.checked_duration_since(t2).is_none());
    }

    #[test]
    fn instant_saturating_duration_since() {
        let _g = TICK_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { crate::kernel::scheduler::TICK = 500; }
        let t1 = Instant::now();
        unsafe { crate::kernel::scheduler::TICK = 600; }
        let t2 = Instant::now();

        assert_eq!(t2.saturating_duration_since(t1).as_ticks(), 100);
        // Going backwards saturates to zero.
        assert_eq!(t1.saturating_duration_since(t2).as_ticks(), 0);
    }

    #[test]
    fn instant_same_tick_elapsed_zero() {
        let _g = TICK_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { crate::kernel::scheduler::TICK = 999; }
        let t = Instant::now();
        // TICK is unchanged — elapsed duration must be zero.
        assert_eq!(Instant::now().saturating_duration_since(t).as_ticks(), 0);
    }

    #[test]
    fn instant_ordering() {
        let _g = TICK_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { crate::kernel::scheduler::TICK = 10; }
        let early = Instant::now();
        unsafe { crate::kernel::scheduler::TICK = 20; }
        let late = Instant::now();
        assert!(early < late);
    }
}
