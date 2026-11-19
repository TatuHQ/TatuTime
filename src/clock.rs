//! Time source port. The domain and handlers never call `Timestamp::now()` directly;
//! they ask a `Clock`. This keeps time-dependent behaviour (the timer #14,
//! reminders #22, budget alerts #30) deterministic under test via `FixedClock`.

use crate::time::*;
pub trait Clock: Send + Sync {
    fn now(&self) -> Timestamp;
    fn today(&self) -> CivilDate {
        self.now().to_zoned_datetime(jiff::tz::TimeZone::UTC).date()
    }
}

/// Production clock: the system wall clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp::now()
    }
}

/// Test/frozen clock: a fixed instant. Shipped (not `cfg(test)`) so the
/// integration-test crate and any deterministic deployment can use it.
#[derive(Debug, Clone, Copy)]
pub struct FixedClock(pub Timestamp);

impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        self.0
    }
}
