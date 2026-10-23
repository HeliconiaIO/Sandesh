//! Timestamp value object.
//!
//! Stored as microseconds since the Unix epoch, UTC (`STORAGE_DESIGN.md` §2.1).
//! Microsecond resolution keeps `INTERNALDATE` ordering stable for messages
//! delivered within the same millisecond.

use core::fmt;
use time::OffsetDateTime;

/// A UTC instant, at microsecond resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(i64);

impl Timestamp {
    /// The current instant.
    ///
    /// Nanoseconds are divided before narrowing, so the value cannot overflow
    /// `i64` for any date this program will see (the microsecond range extends
    /// past the year 290000).
    #[must_use]
    pub fn now() -> Self {
        let micros = OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000;
        Self(i64::try_from(micros).unwrap_or(i64::MAX))
    }

    /// Wraps microseconds since the Unix epoch.
    #[must_use]
    pub const fn from_micros(micros: i64) -> Self {
        Self(micros)
    }

    /// Microseconds since the Unix epoch, for persistence.
    #[must_use]
    pub const fn as_micros(self) -> i64 {
        self.0
    }

    /// Converts to a calendar date-time, for formatting.
    ///
    /// Returns `None` only for values outside the representable range.
    #[must_use]
    pub fn to_datetime(self) -> Option<OffsetDateTime> {
        OffsetDateTime::from_unix_timestamp_nanos(i128::from(self.0) * 1_000).ok()
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.to_datetime() {
            Some(dt) => write!(f, "{dt}"),
            None => write!(f, "Timestamp({})", self.0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_micros() {
        let ts = Timestamp::from_micros(1_756_732_800_000_000);
        assert_eq!(Timestamp::from_micros(ts.as_micros()), ts);
    }

    #[test]
    fn now_is_after_2020() {
        assert!(Timestamp::now().as_micros() > 1_577_836_800_000_000);
    }

    #[test]
    fn timestamps_order_chronologically() {
        assert!(Timestamp::from_micros(1) < Timestamp::from_micros(2));
    }
}
