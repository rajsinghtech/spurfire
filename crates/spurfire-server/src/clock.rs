//! Injectable wall clock used by lifecycle and expiry checks.

use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{SystemTime, UNIX_EPOCH},
};

use spurfire_protocol::UnixMillis;

/// Source of Unix time for the service.
pub trait Clock: Send + Sync {
    /// Returns the current time in Unix milliseconds.
    fn now(&self) -> UnixMillis;
}

/// Production wall clock.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> UnixMillis {
        let milliseconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| {
                u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
            });
        UnixMillis::new(milliseconds)
    }
}

/// Deterministic clock for router and cleanup tests.
#[derive(Clone, Debug)]
pub struct ManualClock {
    milliseconds: Arc<AtomicU64>,
}

impl ManualClock {
    /// Creates a clock fixed at `now` until explicitly changed.
    #[must_use]
    pub fn new(now: UnixMillis) -> Self {
        Self {
            milliseconds: Arc::new(AtomicU64::new(now.as_millis())),
        }
    }

    /// Replaces the current time.
    pub fn set(&self, now: UnixMillis) {
        self.milliseconds.store(now.as_millis(), Ordering::SeqCst);
    }

    /// Advances the clock, saturating at `u64::MAX`.
    pub fn advance(&self, milliseconds: u64) {
        let _ = self
            .milliseconds
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
                Some(current.saturating_add(milliseconds))
            });
    }
}

impl Clock for ManualClock {
    fn now(&self) -> UnixMillis {
        UnixMillis::new(self.milliseconds.load(Ordering::SeqCst))
    }
}
