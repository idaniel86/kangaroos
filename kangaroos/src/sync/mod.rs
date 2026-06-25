pub mod condvar;
pub mod event_group;
pub mod mutex;
pub mod once;
pub mod semaphore;

pub use condvar::Condvar;
pub use event_group::EventGroup;
pub use mutex::{Mutex, MutexGuard};
pub use once::Once;
pub use semaphore::Semaphore;

/// Name/address identifier carried by all sync primitives for debug logs.
#[cfg(feature = "defmt")]
#[derive(Copy, Clone)]
pub(super) struct PrimName(pub Option<&'static str>, pub u32);

#[cfg(feature = "defmt")]
impl defmt::Format for PrimName {
    fn format(&self, f: defmt::Formatter) {
        match self.0 {
            Some(n) => defmt::write!(f, "'{}'", n),
            None    => defmt::write!(f, "@{:#010x}", self.1),
        }
    }
}
