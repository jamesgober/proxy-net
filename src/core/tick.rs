//! Time as the core sees it: a number the caller supplies.

use core::time::Duration;

/// A point in time: nanoseconds from an origin the caller chooses (the
/// proxy's start, for example). The core never reads a clock; every function
/// that needs the time takes a `Tick`, so tests drive time explicitly.
///
/// ```
/// use std::time::Duration;
/// use proxy_net::core::Tick;
/// let start = Tick(1_000);
/// let later = start.after(Duration::from_micros(2));
/// assert_eq!(later, Tick(3_000));
/// assert_eq!(later.since(start), Duration::from_nanos(2_000));
/// assert_eq!(start.since(later), Duration::ZERO);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Tick(pub u64);

impl Tick {
    /// `d` after this tick, saturating at the largest tick.
    #[must_use]
    pub fn after(self, d: Duration) -> Tick {
        let ns = u64::try_from(d.as_nanos()).unwrap_or(u64::MAX);
        Tick(self.0.saturating_add(ns))
    }

    /// The time from `earlier` to this tick, or zero when `earlier` is later.
    #[must_use]
    pub fn since(self, earlier: Tick) -> Duration {
        Duration::from_nanos(self.0.saturating_sub(earlier.0))
    }

    /// The nanoseconds.
    pub fn nanos(self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tick_saturates() {
        assert_eq!(
            Tick(u64::MAX - 1).after(Duration::from_secs(1)),
            Tick(u64::MAX)
        );
        assert_eq!(Tick(5).after(Duration::MAX), Tick(u64::MAX));
        assert_eq!(Tick(5).since(Tick(9)), Duration::ZERO);
        assert_eq!(Tick(9).since(Tick(5)), Duration::from_nanos(4));
        assert_eq!(Tick(7).nanos(), 7);
    }
}
