//! The retry budget: retries in flight stay a bounded share of the pool's
//! active exchanges, so a failing pool cannot multiply its own load.

use core::sync::atomic::{AtomicU32, Ordering};

use super::atomic::update_u32;
use crate::pool::Retry;

/// True for the idempotent methods of RFC 9110 9.2.2: `GET`, `HEAD`,
/// `OPTIONS`, `TRACE`, `PUT`, `DELETE`. Only these are retried.
///
/// ```
/// use proxy_net::core::is_idempotent;
/// assert!(is_idempotent(b"PUT"));
/// assert!(!is_idempotent(b"POST"));
/// assert!(!is_idempotent(b"get"));
/// ```
pub fn is_idempotent(method: &[u8]) -> bool {
    matches!(
        method,
        b"GET" | b"HEAD" | b"OPTIONS" | b"TRACE" | b"PUT" | b"DELETE"
    )
}

/// Counts a pool's active exchanges and retries and grants retries within
/// the budget of [`Retry::budget`].
///
/// ```
/// use proxy_net::core::RetryBudget;
/// let budget = RetryBudget::new(&proxy_net::Retry::default()); // 20%, at least 3
/// for _ in 0..10 {
///     budget.exchange_started();
/// }
/// assert!(budget.try_retry());
/// assert!(budget.try_retry());
/// assert!(budget.try_retry());
/// assert!(!budget.try_retry()); // max(3, 20% of 10) = 3 in flight
/// budget.retry_finished();
/// assert!(budget.try_retry());
/// ```
#[derive(Debug)]
pub struct RetryBudget {
    active: AtomicU32,
    retries: AtomicU32,
    percent: u32,
    min: u32,
}

impl RetryBudget {
    /// A budget with the pool's settings.
    pub fn new(retry: &Retry) -> RetryBudget {
        let (percent, min) = retry.budget_value();
        RetryBudget {
            active: AtomicU32::new(0),
            retries: AtomicU32::new(0),
            percent: u32::from(percent),
            min: u32::from(min),
        }
    }

    /// An exchange started.
    pub fn exchange_started(&self) {
        let _ = self.active.fetch_add(1, Ordering::AcqRel);
    }

    /// An exchange finished.
    pub fn exchange_finished(&self) {
        let _ = update_u32(&self.active, |a| a.checked_sub(1));
    }

    /// Takes a retry place if the budget allows one; `false` means fail the
    /// exchange instead of retrying.
    pub fn try_retry(&self) -> bool {
        let active = u64::from(self.active.load(Ordering::Acquire));
        let cap = (active * u64::from(self.percent) / 100).max(u64::from(self.min));
        update_u32(&self.retries, |r| (u64::from(r) < cap).then_some(r + 1)).is_ok()
    }

    /// A retry granted by [`try_retry`](RetryBudget::try_retry) finished.
    pub fn retry_finished(&self) {
        let _ = update_u32(&self.retries, |r| r.checked_sub(1));
    }

    /// Active exchanges.
    pub fn active(&self) -> u32 {
        self.active.load(Ordering::Acquire)
    }

    /// Retries in flight.
    pub fn retries(&self) -> u32 {
        self.retries.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_budget_scales_with_load() {
        let b = RetryBudget::new(&Retry::default().budget(50, 0));
        assert!(!b.try_retry());
        for _ in 0..4 {
            b.exchange_started();
        }
        assert!(b.try_retry());
        assert!(b.try_retry());
        assert!(!b.try_retry());
        assert_eq!(b.retries(), 2);
        b.retry_finished();
        b.retry_finished();
        b.retry_finished();
        assert_eq!(b.retries(), 0);
        for _ in 0..5 {
            b.exchange_finished();
        }
        assert_eq!(b.active(), 0);
    }

    #[test]
    fn test_idempotent_methods() {
        for m in ["GET", "HEAD", "OPTIONS", "TRACE", "PUT", "DELETE"] {
            assert!(is_idempotent(m.as_bytes()), "{m}");
        }
        for m in ["POST", "PATCH", "CONNECT", "get", ""] {
            assert!(!is_idempotent(m.as_bytes()), "{m}");
        }
    }
}
