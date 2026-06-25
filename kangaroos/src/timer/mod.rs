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
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
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
    fn sub(self, rhs: Duration) -> Duration {
        Duration(self.0 - rhs.0)
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
    pub fn now() -> Instant {
        cortex_m::interrupt::free(|_| {
            // SAFETY: TICK is only written from the SysTick handler; reading it
            // inside interrupt::free on a single-core device is race-free.
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
        let now = cortex_m::interrupt::free(|_| unsafe { crate::kernel::scheduler::TICK });
        Timer {
            next: now.wrapping_add(delay.as_ticks()),
            period: None,
        }
    }

    /// Create a periodic timer that fires every `period`, starting `period`
    /// from now.
    pub fn every(period: Duration) -> Timer {
        let ticks = period.as_ticks();
        let now = cortex_m::interrupt::free(|_| unsafe { crate::kernel::scheduler::TICK });
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
