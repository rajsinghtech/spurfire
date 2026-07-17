//! Wire-safe absolute timestamps.

use serde::{Deserialize, Serialize};

/// Milliseconds since the Unix epoch.
///
/// Spurfire uses an integer timestamp on the wire so freshness and TTL checks do
/// not depend on a platform's floating-point or date/time implementation.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct UnixMillis(u64);

impl UnixMillis {
    /// Creates a timestamp from milliseconds since the Unix epoch.
    #[must_use]
    pub const fn new(milliseconds: u64) -> Self {
        Self(milliseconds)
    }

    /// Returns milliseconds since the Unix epoch.
    #[must_use]
    pub const fn as_millis(self) -> u64 {
        self.0
    }

    /// Returns the elapsed milliseconds, or `None` if `earlier` is in the future.
    #[must_use]
    pub const fn checked_duration_since(self, earlier: Self) -> Option<u64> {
        self.0.checked_sub(earlier.0)
    }

    /// Adds a duration, saturating at the largest representable timestamp.
    #[must_use]
    pub const fn saturating_add(self, milliseconds: u64) -> Self {
        Self(self.0.saturating_add(milliseconds))
    }
}

impl From<u64> for UnixMillis {
    fn from(value: u64) -> Self {
        Self::new(value)
    }
}

impl From<UnixMillis> for u64 {
    fn from(value: UnixMillis) -> Self {
        value.as_millis()
    }
}
