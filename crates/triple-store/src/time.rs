//! Timestamps, intervals, and an injectable clock for deterministic tests.

use std::time::SystemTime;

use serde::{Deserialize, Serialize};

/// Nanoseconds since the UNIX epoch. `u64` range covers up to ~year 2554.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Timestamp(pub u64);

impl Timestamp {
    pub const fn from_nanos(n: u64) -> Self {
        Self(n)
    }

    pub const fn as_nanos(self) -> u64 {
        self.0
    }
}

/// Half-open interval `[start, end)`. `end == None` means the interval is
/// currently open (no known end).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Interval {
    pub start: Timestamp,
    pub end: Option<Timestamp>,
}

impl Interval {
    /// Construct an open-ended interval starting at `start`.
    pub fn new_open(start: Timestamp) -> Self {
        Self { start, end: None }
    }

    /// Construct a closed interval `[start, end)`.
    ///
    /// Requires: `end >= start`.
    pub fn new_closed(start: Timestamp, end: Timestamp) -> Self {
        debug_assert!(end >= start, "Interval end must be >= start");
        Self {
            start,
            end: Some(end),
        }
    }

    /// Test whether `t` lies in `[self.start, self.end)`.
    ///
    /// Ensures: returns `t >= self.start && (self.end.is_none() || t < self.end.unwrap())`.
    pub fn contains(&self, t: Timestamp) -> bool {
        t >= self.start && self.end.is_none_or(|e| t < e)
    }

    /// Whether the interval has no known end.
    ///
    /// Ensures: returns `self.end.is_none()`.
    pub fn is_open(&self) -> bool {
        self.end.is_none()
    }

    /// Close an open interval at `at`.
    ///
    /// Requires: `self.is_open()` and `at >= self.start`.
    /// Ensures: `!self.is_open()` and `self.end == Some(at)`.
    pub fn close(&mut self, at: Timestamp) {
        debug_assert!(self.is_open(), "interval is already closed");
        debug_assert!(at >= self.start, "close timestamp must be >= start");
        self.end = Some(at);
    }
}

/// Source of "current time". The store accepts a `Clock` so tests can drive
/// time deterministically (PRD §6). All production code that needs the
/// current time must obtain it via a `Clock`; there is no static accessor on
/// `Timestamp` for wall-clock now, by design.
pub trait Clock: Send + Sync {
    /// Returns the current time according to this clock.
    fn now(&self) -> Timestamp;
}

/// Wall-clock implementation backed by `std::time::SystemTime`.
#[derive(Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    /// Returns wall-clock now, truncated to `u64` nanoseconds since the UNIX epoch.
    ///
    /// Requires: the system clock is at or after the UNIX epoch.
    /// Panics: if the system clock predates the UNIX epoch.
    fn now(&self) -> Timestamp {
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("system clock predates UNIX epoch")
            .as_nanos();
        Timestamp(nanos as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn ts(n: u64) -> Timestamp {
        Timestamp::from_nanos(n)
    }

    #[test]
    fn open_interval_contains_at_or_after_start() {
        let i = Interval::new_open(ts(10));
        assert!(!i.contains(ts(9)));
        assert!(i.contains(ts(10)));
        assert!(i.contains(ts(u64::MAX)));
        assert!(i.is_open());
    }

    #[test]
    fn closed_interval_is_half_open() {
        let i = Interval::new_closed(ts(10), ts(20));
        assert!(!i.contains(ts(9)));
        assert!(i.contains(ts(10)));
        assert!(i.contains(ts(19)));
        assert!(!i.contains(ts(20)));
        assert!(!i.contains(ts(21)));
        assert!(!i.is_open());
    }

    #[test]
    fn empty_closed_interval_contains_nothing() {
        let i = Interval::new_closed(ts(10), ts(10));
        assert!(!i.contains(ts(9)));
        assert!(!i.contains(ts(10)));
        assert!(!i.contains(ts(11)));
    }

    #[test]
    fn close_open_interval() {
        let mut i = Interval::new_open(ts(10));
        i.close(ts(20));
        assert_eq!(i, Interval::new_closed(ts(10), ts(20)));
    }

    #[test]
    fn system_clock_is_monotonic_within_a_call_pair() {
        let c = SystemClock;
        let a = c.now();
        let b = c.now();
        assert!(b >= a);
    }

    proptest! {
        #[test]
        fn open_interval_contains_iff_t_ge_start(start: u64, t: u64) {
            let i = Interval::new_open(ts(start));
            prop_assert_eq!(i.contains(ts(t)), t >= start);
        }

        #[test]
        fn closed_interval_contains_iff_in_half_open_range(start: u64, len in 0u64..10_000) {
            let end = start.saturating_add(len);
            let i = Interval::new_closed(ts(start), ts(end));
            for t in [
                start.saturating_sub(1),
                start,
                end.saturating_sub(1),
                end,
                end.saturating_add(1),
            ] {
                let expected = t >= start && t < end;
                prop_assert_eq!(i.contains(ts(t)), expected, "t={}, start={}, end={}", t, start, end);
            }
        }
    }
}
