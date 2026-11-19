//! Endpoint health: passive outlier detection from real traffic, active
//! check results, ejection with growing back-off, and draining.
//!
//! Every endpoint's state is one 64-bit word changed with compare-and-swap, so
//! recording an outcome takes no lock.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::time::Duration;

use super::atomic::{AtomicU32, AtomicU64, Ordering, update_u32};
use super::class::Outcome;
use super::tick::Tick;
use crate::pool::PoolConfig;

/// An endpoint's position in its pool, from 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EndpointId(pub u16);

impl EndpointId {
    /// The position as an index.
    pub fn index(self) -> usize {
        usize::from(self.0)
    }
}

/// The health of one endpoint.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EndpointState {
    /// Not judged yet. Takes traffic when the pool has no active check; with
    /// one, waits for its first `rise` successes.
    Unknown,
    /// Takes traffic.
    Healthy,
    /// Failed its active checks; takes no traffic until it passes them again.
    Unhealthy,
    /// Taken out by passive health until `until`.
    Ejected {
        /// When the ejection ends.
        until: Tick,
    },
    /// Removed by a reload: finishes what it has, takes nothing new.
    Draining,
}

/// A change of an endpoint's state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Transition {
    /// The endpoint.
    pub endpoint: EndpointId,
    /// The state before.
    pub from: EndpointState,
    /// The state after.
    pub to: EndpointState,
}

// The word: bits 0..4 state, 8..24 consecutive passive failures, 24..32 active
// successes in a row, 32..40 active failures in a row, 40..56 ejections so far.
const UNKNOWN: u64 = 0;
const HEALTHY: u64 = 1;
const UNHEALTHY: u64 = 2;
const EJECTED: u64 = 3;
const DRAINING: u64 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Word {
    state: u64,
    failures: u16,
    successes_run: u8,
    failures_run: u8,
    ejections: u16,
}

impl Word {
    fn unpack(w: u64) -> Word {
        Word {
            state: w & 0xf,
            failures: ((w >> 8) & 0xffff) as u16,
            successes_run: ((w >> 24) & 0xff) as u8,
            failures_run: ((w >> 32) & 0xff) as u8,
            ejections: ((w >> 40) & 0xffff) as u16,
        }
    }

    fn pack(self) -> u64 {
        self.state
            | (u64::from(self.failures) << 8)
            | (u64::from(self.successes_run) << 24)
            | (u64::from(self.failures_run) << 32)
            | (u64::from(self.ejections) << 40)
    }
}

struct Slot {
    word: AtomicU64,
    until: AtomicU64,
}

/// The health of every endpoint of one pool.
///
/// ```
/// use proxy_net::core::{EndpointId, EndpointState, HealthTable, Outcome, Tick};
/// use proxy_net::{Outlier, Pool, ProxyBuilder};
///
/// let config = ProxyBuilder::new()
///     .pool("app", Pool::new(["10.0.0.1:80", "10.0.0.2:80"]).outlier(Outlier::default().consecutive_failures(2)))
///     .config()?;
/// let health = HealthTable::new(config.pool("app").expect("pool"));
/// let a = EndpointId(0);
/// assert_eq!(health.record(a, Outcome::Failure, Tick(0)), None);
/// let t = health.record(a, Outcome::Failure, Tick(0)).expect("ejected");
/// assert!(matches!(t.to, EndpointState::Ejected { .. }));
/// assert!(!health.is_healthy(a));
/// # Ok::<(), proxy_net::ConfigError>(())
/// ```
pub struct HealthTable {
    slots: Box<[Slot]>,
    ejected: AtomicU32,
    active_checks: bool,
    rise: u8,
    fall: u8,
    consecutive: u16,
    base_ns: u64,
    max_ns: u64,
    max_ejected_percent: u32,
}

impl core::fmt::Debug for HealthTable {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("HealthTable")
            .field("endpoints", &self.slots.len())
            .field("ejected", &self.ejected.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

fn nanos(d: Duration) -> u64 {
    u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)
}

impl HealthTable {
    /// A table for `pool`'s endpoints, all `Unknown`.
    pub fn new(pool: &PoolConfig) -> HealthTable {
        let n = pool.endpoints().len();
        let slots = (0..n)
            .map(|_| Slot {
                word: AtomicU64::new(UNKNOWN),
                until: AtomicU64::new(0),
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let (rise, fall) = pool
            .health()
            .map_or((1, 1), |h| (h.rise_value(), h.fall_value()));
        let o = pool.outlier();
        HealthTable {
            slots,
            ejected: AtomicU32::new(0),
            active_checks: pool.health().is_some(),
            rise,
            fall,
            consecutive: o.consecutive_value(),
            base_ns: nanos(o.base_ejection_value()),
            max_ns: nanos(o.max_ejection_value()),
            max_ejected_percent: u32::from(o.max_ejected_value()),
        }
    }

    /// The number of endpoints.
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// True when the pool has no endpoints.
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    fn slot(&self, id: EndpointId) -> Option<&Slot> {
        self.slots.get(id.index())
    }

    /// The endpoint's state; an unknown id reads as `Draining`.
    pub fn state(&self, id: EndpointId) -> EndpointState {
        match self.slot(id) {
            Some(slot) => self.decode(slot, Word::unpack(slot.word.load(Ordering::Acquire))),
            None => EndpointState::Draining,
        }
    }

    fn decode(&self, slot: &Slot, w: Word) -> EndpointState {
        match w.state {
            UNKNOWN => EndpointState::Unknown,
            HEALTHY => EndpointState::Healthy,
            UNHEALTHY => EndpointState::Unhealthy,
            EJECTED => EndpointState::Ejected {
                until: Tick(slot.until.load(Ordering::Acquire)),
            },
            _ => EndpointState::Draining,
        }
    }

    /// True when the endpoint takes traffic: `Healthy`, or `Unknown` in a
    /// pool without active checks.
    pub fn is_healthy(&self, id: EndpointId) -> bool {
        match self.state(id) {
            EndpointState::Healthy => true,
            EndpointState::Unknown => !self.active_checks,
            _ => false,
        }
    }

    /// The number of endpoints that take traffic.
    pub fn healthy_count(&self) -> usize {
        (0..self.slots.len())
            .filter(|&i| self.is_healthy(EndpointId(i as u16)))
            .count()
    }

    /// True when the endpoint is draining or out of range.
    pub fn is_draining(&self, id: EndpointId) -> bool {
        self.state(id) == EndpointState::Draining
    }

    /// Applies `f` to the endpoint's word until the swap succeeds; returns the
    /// transition when the state changed.
    fn update(
        &self,
        id: EndpointId,
        mut f: impl FnMut(Word) -> Option<Word>,
    ) -> Option<Transition> {
        let slot = self.slot(id)?;
        let mut current = slot.word.load(Ordering::Acquire);
        loop {
            let old = Word::unpack(current);
            let new = f(old)?;
            match slot.word.compare_exchange_weak(
                current,
                new.pack(),
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) if new.state != old.state => {
                    return Some(Transition {
                        endpoint: id,
                        from: self.decode(slot, old),
                        to: self.decode(slot, new),
                    });
                }
                Ok(_) => return None,
                Err(seen) => current = seen,
            }
        }
    }

    /// Records one attempt's outcome from real traffic (passive health).
    ///
    /// A success resets the endpoint's failure count. A failure counts; at
    /// `Outlier::consecutive_failures` in a row the endpoint is ejected for
    /// `base_ejection` times the number of its ejections so far (up to
    /// `max_ejection`), unless that would eject more than `max_ejected`
    /// percent of the pool. Outcomes for an ejected or draining endpoint are
    /// ignored.
    pub fn record(&self, id: EndpointId, outcome: Outcome, now: Tick) -> Option<Transition> {
        match outcome {
            Outcome::Neutral => None,
            Outcome::Success => self.update(id, |mut w| {
                if w.failures == 0 || w.state == EJECTED || w.state == DRAINING {
                    return None;
                }
                w.failures = 0;
                Some(w)
            }),
            Outcome::Failure => self.record_failure(id, now),
        }
    }

    fn record_failure(&self, id: EndpointId, now: Tick) -> Option<Transition> {
        let slot = self.slot(id)?;
        loop {
            let current = slot.word.load(Ordering::Acquire);
            let mut w = Word::unpack(current);
            if w.state == EJECTED || w.state == DRAINING {
                return None;
            }
            w.failures = w.failures.saturating_add(1);
            let eject = self.consecutive > 0
                && w.failures >= self.consecutive
                && (w.state == HEALTHY || w.state == UNKNOWN);
            if eject && self.reserve_ejection() {
                w.ejections = w.ejections.saturating_add(1);
                let length = self
                    .base_ns
                    .saturating_mul(u64::from(w.ejections))
                    .min(self.max_ns);
                let old_state = Word::unpack(current);
                w.state = EJECTED;
                w.failures = 0;
                slot.until
                    .store(now.0.saturating_add(length), Ordering::Release);
                match slot.word.compare_exchange(
                    current,
                    w.pack(),
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => {
                        return Some(Transition {
                            endpoint: id,
                            from: self.decode(slot, old_state),
                            to: self.decode(slot, w),
                        });
                    }
                    Err(_) => {
                        let _ = self.ejected.fetch_sub(1, Ordering::AcqRel);
                        continue;
                    }
                }
            }
            if slot
                .word
                .compare_exchange(current, w.pack(), Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return None;
            }
        }
    }

    /// Takes one ejection place if the cap allows it.
    fn reserve_ejection(&self) -> bool {
        let n = u32::try_from(self.slots.len()).unwrap_or(u32::MAX);
        update_u32(&self.ejected, |e| {
            let after = u64::from(e) + 1;
            (after * 100 <= u64::from(self.max_ejected_percent) * u64::from(n)).then_some(e + 1)
        })
        .is_ok()
    }

    /// Records one active check result. `rise` successes in a row make an
    /// `Unknown` or `Unhealthy` endpoint healthy; `fall` failures in a row make
    /// an `Unknown` or `Healthy` one unhealthy. An ejected endpoint stays
    /// ejected until its time ends.
    pub fn check_result(&self, id: EndpointId, ok: bool, _now: Tick) -> Option<Transition> {
        let (rise, fall) = (self.rise, self.fall);
        self.update(id, |mut w| {
            if w.state == DRAINING {
                return None;
            }
            if ok {
                w.successes_run = w.successes_run.saturating_add(1);
                w.failures_run = 0;
                if (w.state == UNKNOWN || w.state == UNHEALTHY) && w.successes_run >= rise {
                    w.state = HEALTHY;
                    w.failures = 0;
                }
            } else {
                w.failures_run = w.failures_run.saturating_add(1);
                w.successes_run = 0;
                if (w.state == UNKNOWN || w.state == HEALTHY) && w.failures_run >= fall {
                    w.state = UNHEALTHY;
                }
            }
            Some(w)
        })
    }

    /// Ends the ejections whose time has come and pushes each change to
    /// `changes`. The caller runs it on its own schedule (the health checker
    /// does, from 0.4); nothing here reads a clock.
    pub fn expire(&self, now: Tick, changes: &mut Vec<Transition>) {
        for (i, slot) in self.slots.iter().enumerate() {
            let w = Word::unpack(slot.word.load(Ordering::Acquire));
            if w.state != EJECTED || slot.until.load(Ordering::Acquire) > now.0 {
                continue;
            }
            let id = EndpointId(i as u16);
            if let Some(t) = self.update(id, |mut w| {
                if w.state != EJECTED {
                    return None;
                }
                w.state = HEALTHY;
                w.failures = 0;
                Some(w)
            }) {
                let _ = self.ejected.fetch_sub(1, Ordering::AcqRel);
                changes.push(t);
            }
        }
    }

    /// Marks the endpoint draining (a reload removed it): it takes nothing
    /// new. Returns the transition, if any.
    pub fn drain(&self, id: EndpointId) -> Option<Transition> {
        let was_ejected = self.state(id) != EndpointState::Draining
            && matches!(self.state(id), EndpointState::Ejected { .. });
        let t = self.update(id, |mut w| {
            if w.state == DRAINING {
                return None;
            }
            w.state = DRAINING;
            Some(w)
        });
        if t.is_some() && was_ejected {
            let _ = self.ejected.fetch_sub(1, Ordering::AcqRel);
        }
        t
    }

    /// The number of endpoints ejected now.
    pub fn ejected_count(&self) -> u32 {
        self.ejected.load(Ordering::Acquire)
    }

    /// Copies endpoint `from_id`'s state in `from` to endpoint `id` here (a
    /// reload keeps the health of endpoints present in both).
    #[cfg(feature = "client")]
    pub(crate) fn adopt(&self, id: EndpointId, from: &HealthTable, from_id: EndpointId) {
        let (Some(slot), Some(old)) = (self.slot(id), from.slot(from_id)) else {
            return;
        };
        let word = old.word.load(Ordering::Acquire);
        slot.until
            .store(old.until.load(Ordering::Acquire), Ordering::Release);
        slot.word.store(word, Ordering::Release);
        if Word::unpack(word).state == EJECTED {
            let _ = self.ejected.fetch_add(1, Ordering::AcqRel);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{HealthCheck, Outlier, Pool, PoolConfig, ProxyBuilder};

    fn pool(n: usize, outlier: Outlier, check: Option<HealthCheck>) -> PoolConfig {
        let endpoints: Vec<alloc::string::String> = (0..n)
            .map(|i| alloc::format!("10.0.0.{}:80", i + 1))
            .collect();
        let mut p = Pool::new(endpoints).outlier(outlier);
        if let Some(c) = check {
            p = p.health(c);
        }
        ProxyBuilder::new()
            .pool("p", p)
            .config()
            .expect("config")
            .pool("p")
            .expect("pool")
            .clone()
    }

    const S: u64 = 1_000_000_000;

    #[test]
    fn test_passive_ejection_and_growing_backoff() {
        let t = HealthTable::new(&pool(4, Outlier::default().consecutive_failures(3), None));
        let a = EndpointId(1);
        assert!(t.is_healthy(a));
        assert_eq!(t.record(a, Outcome::Failure, Tick(0)), None);
        assert_eq!(t.record(a, Outcome::Failure, Tick(0)), None);
        // A success resets the run.
        assert_eq!(t.record(a, Outcome::Success, Tick(0)), None);
        assert_eq!(t.record(a, Outcome::Failure, Tick(0)), None);
        assert_eq!(t.record(a, Outcome::Failure, Tick(0)), None);
        let tr = t.record(a, Outcome::Failure, Tick(0)).expect("ejected");
        assert_eq!(tr.from, EndpointState::Unknown);
        assert_eq!(
            tr.to,
            EndpointState::Ejected {
                until: Tick(30 * S)
            }
        );
        assert_eq!(t.ejected_count(), 1);
        // Outcomes while ejected are ignored.
        assert_eq!(t.record(a, Outcome::Failure, Tick(1)), None);
        let mut changes = Vec::new();
        t.expire(Tick(30 * S - 1), &mut changes);
        assert!(changes.is_empty());
        t.expire(Tick(30 * S), &mut changes);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].to, EndpointState::Healthy);
        assert_eq!(t.ejected_count(), 0);
        // The second ejection lasts twice as long.
        for _ in 0..3 {
            let _ = t.record(a, Outcome::Failure, Tick(40 * S));
        }
        assert_eq!(
            t.state(a),
            EndpointState::Ejected {
                until: Tick(100 * S)
            }
        );
        assert_eq!(t.record(EndpointId(9), Outcome::Failure, Tick(0)), None);
        assert_eq!(t.record(a, Outcome::Neutral, Tick(0)), None);
    }

    #[test]
    fn test_ejection_is_capped_at_max_ejection() {
        let o = Outlier::default()
            .consecutive_failures(1)
            .base_ejection(Duration::from_secs(100))
            .max_ejection(Duration::from_secs(150))
            .max_ejected(100);
        let t = HealthTable::new(&pool(1, o, None));
        let a = EndpointId(0);
        let mut changes = Vec::new();
        let _ = t.record(a, Outcome::Failure, Tick(0));
        assert_eq!(
            t.state(a),
            EndpointState::Ejected {
                until: Tick(100 * S)
            }
        );
        t.expire(Tick(100 * S), &mut changes);
        let _ = t.record(a, Outcome::Failure, Tick(100 * S));
        assert_eq!(
            t.state(a),
            EndpointState::Ejected {
                until: Tick(250 * S)
            }
        );
    }

    #[test]
    fn test_max_ejected_percent_with_small_pools() {
        // E-143: 50% of 1 endpoint is 0, of 2 is 1, of 3 is 1.
        for (n, ejectable) in [(1, 0), (2, 1), (3, 1), (4, 2)] {
            let t = HealthTable::new(&pool(n, Outlier::default().consecutive_failures(1), None));
            for i in 0..n {
                let _ = t.record(EndpointId(i as u16), Outcome::Failure, Tick(0));
            }
            assert_eq!(t.ejected_count() as usize, ejectable, "n = {n}");
            assert_eq!(t.healthy_count(), n - ejectable, "n = {n}");
        }
        let off = HealthTable::new(&pool(2, Outlier::default().consecutive_failures(0), None));
        for _ in 0..100 {
            let _ = off.record(EndpointId(0), Outcome::Failure, Tick(0));
        }
        assert_eq!(off.ejected_count(), 0);
    }

    #[test]
    fn test_active_checks_rise_and_fall() {
        let check = HealthCheck::http("/h").rise(2).fall(3);
        let t = HealthTable::new(&pool(2, Outlier::default(), Some(check)));
        let a = EndpointId(0);
        // With active checks, Unknown takes no traffic.
        assert!(!t.is_healthy(a));
        assert_eq!(t.check_result(a, true, Tick(0)), None);
        let up = t.check_result(a, true, Tick(0)).expect("rise");
        assert_eq!(
            (up.from, up.to),
            (EndpointState::Unknown, EndpointState::Healthy)
        );
        assert!(t.is_healthy(a));
        assert_eq!(t.check_result(a, false, Tick(0)), None);
        assert_eq!(t.check_result(a, false, Tick(0)), None);
        // A success in between resets the failure run.
        assert_eq!(t.check_result(a, true, Tick(0)), None);
        for _ in 0..2 {
            assert_eq!(t.check_result(a, false, Tick(0)), None);
        }
        let down = t.check_result(a, false, Tick(0)).expect("fall");
        assert_eq!(down.to, EndpointState::Unhealthy);
        // Passive failures do not eject an unhealthy endpoint.
        for _ in 0..10 {
            assert_eq!(t.record(a, Outcome::Failure, Tick(0)), None);
        }
        assert_eq!(t.state(a), EndpointState::Unhealthy);
    }

    #[test]
    fn test_drain() {
        let t = HealthTable::new(&pool(3, Outlier::default().consecutive_failures(1), None));
        let a = EndpointId(0);
        let _ = t.record(a, Outcome::Failure, Tick(0));
        assert_eq!(t.ejected_count(), 1);
        let d = t.drain(a).expect("drain");
        assert_eq!(d.to, EndpointState::Draining);
        assert_eq!(t.ejected_count(), 0);
        assert!(t.is_draining(a));
        assert_eq!(t.drain(a), None);
        assert_eq!(t.check_result(a, true, Tick(0)), None);
        assert_eq!(t.record(a, Outcome::Failure, Tick(0)), None);
        assert!(t.is_draining(EndpointId(99)));
        assert_eq!(t.len(), 3);
        assert!(!t.is_empty());
        assert!(alloc::format!("{t:?}").contains("HealthTable"));
    }

    #[test]
    fn test_word_round_trips() {
        let w = Word {
            state: EJECTED,
            failures: 65535,
            successes_run: 255,
            failures_run: 7,
            ejections: 1234,
        };
        assert_eq!(Word::unpack(w.pack()), w);
    }
}
