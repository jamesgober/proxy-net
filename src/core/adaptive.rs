//! Idea I-5: a gradient concurrency limit, behind `PoolLimits::adaptive_concurrency`.
//!
//! Hypothesis: a limit that follows an endpoint's latency finds the point
//! where the endpoint starts queueing (the knee) better than a fixed cap,
//! which is either too high (requests queue inside the endpoint and latency
//! grows) or too low (capacity is refused).
//!
//! Each finished exchange reports its latency into a short moving average.
//! Once per window (as many samples as the limit, about one round of
//! requests) the limit grows by its square root while that average is
//! within a quarter of the floor, and shrinks in proportion when it is
//! above. The floor is the lowest window average of the last period of 32
//! windows; the last window of each period runs at half the limit so the
//! endpoint's queue drains and the floor measures the endpoint, not the
//! queue, and an endpoint that got slower is learned within a period.
//! Integer arithmetic only, so it builds without the standard library.
//!
//! Cost: admission is one compare-and-swap on the in-flight count; a sample
//! is one subtraction and, for the one thread that wins a flag, a few relaxed
//! loads and stores. A thread that loses the flag skips its sample rather
//! than wait. About 60 bytes per endpoint.
//!
//! The simulation in the tests sweeps offered load against an endpoint with
//! a knee; the results and the decision are in PLAN (I-5, D-063).

#![cfg_attr(not(any(test, feature = "client")), allow(dead_code))]

use super::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering, update_u32};

/// Fixed-point scale of the gradient.
const ONE: u64 = 1024;

/// Windows per period of the latency floor.
const PERIOD: u32 = 32;

/// A gradient concurrency limit for one endpoint.
#[derive(Debug)]
pub(crate) struct GradientLimit {
    limit: AtomicU32,
    inflight: AtomicU32,
    /// Moving averages of latency in nanoseconds: weight 1/8 and 1/256.
    short: AtomicU64,
    long: AtomicU64,
    updating: AtomicBool,
    /// Samples since the limit last changed.
    since: AtomicU32,
    /// The lowest window average in the current period, and the windows in
    /// it so far.
    period_min: AtomicU64,
    windows: AtomicU32,
    /// The limit before a probe (0 when not probing).
    saved: AtomicU32,
    min: u32,
    max: u32,
}

impl GradientLimit {
    /// A limit starting at `initial`, kept within `min..=max`.
    pub(crate) fn new(initial: u32, min: u32, max: u32) -> GradientLimit {
        let min = min.max(1);
        let max = max.max(min);
        GradientLimit {
            limit: AtomicU32::new(initial.clamp(min, max)),
            inflight: AtomicU32::new(0),
            short: AtomicU64::new(0),
            long: AtomicU64::new(0),
            updating: AtomicBool::new(false),
            since: AtomicU32::new(0),
            period_min: AtomicU64::new(u64::MAX),
            windows: AtomicU32::new(0),
            saved: AtomicU32::new(0),
            min,
            max,
        }
    }

    /// The current limit.
    pub(crate) fn limit(&self) -> u32 {
        self.limit.load(Ordering::Relaxed)
    }

    /// Admits one exchange when fewer than the limit are in flight.
    pub(crate) fn try_acquire(&self) -> bool {
        let limit = self.limit.load(Ordering::Relaxed);
        update_u32(&self.inflight, |n| (n < limit).then(|| n + 1)).is_ok()
    }

    /// An admitted exchange finished after `latency_ns`.
    pub(crate) fn release(&self, latency_ns: u64) {
        let inflight = self
            .inflight
            .fetch_sub(1, Ordering::AcqRel)
            .saturating_sub(1);
        if self
            .updating
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return;
        }
        self.sample(latency_ns.max(1), inflight);
        self.updating.store(false, Ordering::Release);
    }

    fn sample(&self, rtt: u64, inflight: u32) {
        let ewma = |avg: u64, shift: u32| {
            if avg == 0 {
                rtt
            } else if rtt >= avg {
                avg + ((rtt - avg) >> shift)
            } else {
                avg - ((avg - rtt) >> shift)
            }
        };
        let short = ewma(self.short.load(Ordering::Relaxed), 3);
        self.short.store(short, Ordering::Relaxed);
        // The limit changes once per window of as many samples as the limit
        // (about one round of requests), so it acts on latency its last
        // change already produced.
        let limit = u64::from(self.limit.load(Ordering::Relaxed));
        // A probe's window is as long as the limit before it, so what was
        // admitted before the probe has finished by its end.
        let saved = self.saved.load(Ordering::Relaxed);
        let window = if saved == 0 { limit } else { u64::from(saved) };
        let since = self.since.load(Ordering::Relaxed).saturating_add(1);
        if u64::from(since) < window {
            self.since.store(since, Ordering::Relaxed);
            return;
        }
        self.since.store(0, Ordering::Relaxed);
        // The floor: the lowest window average of the last period of
        // `PERIOD` windows (the average, so the spread of single requests
        // does not pull it down). Within a period it only falls; at the end
        // of one it becomes that period's lowest, so an endpoint that got
        // slower is learned within a period, while a queue this limit keeps
        // short does not raise it.
        let period_min = self.period_min.load(Ordering::Relaxed).min(short);
        let mut floor = match self.long.load(Ordering::Relaxed) {
            0 => short,
            f => f.min(short),
        };
        let windows = self.windows.load(Ordering::Relaxed) + 1;
        self.period_min.store(period_min, Ordering::Relaxed);
        self.windows.store(windows, Ordering::Relaxed);
        // The last window of a period runs at half the limit, so the
        // endpoint's queue drains and the period's lowest average is its
        // own latency, not the queue's.
        if windows == PERIOD - 1 {
            self.saved
                .store(u32::try_from(limit).unwrap_or(u32::MAX), Ordering::Relaxed);
            let half = u32::try_from(limit / 2).unwrap_or(u32::MAX);
            self.limit.store(half.max(self.min), Ordering::Relaxed);
            return;
        }
        let mut limit = limit;
        if windows >= PERIOD {
            floor = period_min;
            self.period_min.store(u64::MAX, Ordering::Relaxed);
            self.windows.store(0, Ordering::Relaxed);
            let saved = self.saved.swap(0, Ordering::Relaxed);
            if saved != 0 {
                limit = u64::from(saved);
            }
        }
        self.long.store(floor, Ordering::Relaxed);
        // Recent latency within a quarter of the floor counts as no queue.
        let gradient = (floor.saturating_mul(ONE + ONE / 4) / short.max(1)).clamp(ONE / 2, ONE);
        let mut next = if gradient == ONE {
            limit + u64::from(u32::try_from(limit).unwrap_or(u32::MAX).isqrt()).max(1)
        } else {
            limit * gradient / ONE
        };
        // Grow only while the limit is in use, so an idle endpoint does not
        // collect a limit it never proved.
        if u64::from(inflight) * 2 < limit {
            next = next.min(limit);
        }
        // Smoothing: a fifth of the way to the new value.
        let smoothed = (limit * 4 + next) / 5;
        let smoothed = if next > limit {
            smoothed.max(limit + 1)
        } else {
            smoothed
        };
        let bounded = u32::try_from(smoothed)
            .unwrap_or(u32::MAX)
            .clamp(self.min, self.max);
        self.limit.store(bounded, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use alloc::collections::{BinaryHeap, VecDeque};
    use alloc::vec::Vec;
    use core::cmp::Reverse;

    use super::*;

    #[test]
    fn test_admission_follows_the_limit() {
        let g = GradientLimit::new(2, 1, 10);
        assert!(g.try_acquire());
        assert!(g.try_acquire());
        assert!(!g.try_acquire());
        g.release(1_000);
        assert!(g.try_acquire());
    }

    #[test]
    fn test_the_limit_shrinks_when_latency_rises_and_stays_in_bounds() {
        let g = GradientLimit::new(100, 4, 200);
        for _ in 0..200 {
            assert!(g.try_acquire());
            let _ = g.inflight.fetch_add(99, Ordering::Relaxed);
            g.release(1_000_000);
            let _ = g.inflight.fetch_sub(99, Ordering::Relaxed);
        }
        let steady = g.limit();
        for _ in 0..200 {
            assert!(g.try_acquire());
            g.release(10_000_000);
        }
        assert!(g.limit() < steady, "{} !< {steady}", g.limit());
        assert!(g.limit() >= 4);
    }

    /// How the proxy admits requests in the simulation.
    #[derive(Clone, Copy, Debug)]
    enum Admit {
        Fixed(u32),
        Gradient,
    }

    struct Run {
        goodput: f64,
        p50_us: u64,
        p99_us: u64,
        refused: f64,
    }

    /// A closed-loop simulation: `clients` each send a request as soon as
    /// the last one finished (or 1 ms after a refusal) to an endpoint with
    /// `capacity` workers and about 1 ms of service time; requests above
    /// `capacity` queue inside the endpoint. 5 s simulated; the last 4 s
    /// are measured.
    fn simulate(admit: Admit, clients: usize, capacity: usize) -> Run {
        simulate_with(admit, clients, capacity, None)
    }

    /// As `simulate`; with `slower`, service time doubles from 2.5 s and
    /// only the last 1.5 s are measured.
    fn simulate_with(admit: Admit, clients: usize, capacity: usize, slower: Option<()>) -> Run {
        const MS: u64 = 1_000_000;
        const END: u64 = 5_000 * MS;
        let from: u64 = if slower.is_some() {
            3_500 * MS
        } else {
            1_000 * MS
        };
        let gradient = GradientLimit::new(32, 4, 4096);
        let mut inflight = 0u32;
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut service = |t: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let base = MS * 8 / 10 + seed % (MS * 4 / 10);
            if slower.is_some() && t >= 2_500 * MS {
                base * 2
            } else {
                base
            }
        };
        // (time, client, done): `done` false is a send, true a completion.
        let mut events: BinaryHeap<Reverse<(u64, usize, bool)>> = (0..clients)
            .map(|c| Reverse((c as u64 * 1_000, c, false)))
            .collect();
        let mut sent_at = alloc::vec![0u64; clients];
        let mut queue: VecDeque<usize> = VecDeque::new();
        let mut busy = 0usize;
        let (mut done, mut refused) = (0u64, 0u64);
        let mut latencies: Vec<u64> = Vec::new();
        while let Some(Reverse((t, c, finished))) = events.pop() {
            if t >= END {
                break;
            }
            if finished {
                let latency = t - sent_at[c];
                match admit {
                    Admit::Gradient => gradient.release(latency),
                    Admit::Fixed(_) => inflight -= 1,
                }
                if t >= from {
                    done += 1;
                    latencies.push(latency);
                }
                events.push(Reverse((t, c, false)));
                if let Some(next) = queue.pop_front() {
                    events.push(Reverse((t + service(t), next, true)));
                } else {
                    busy -= 1;
                }
                continue;
            }
            let admitted = match admit {
                Admit::Gradient => gradient.try_acquire(),
                Admit::Fixed(k) if inflight < k => {
                    inflight += 1;
                    true
                }
                Admit::Fixed(_) => false,
            };
            if !admitted {
                if t >= from {
                    refused += 1;
                }
                events.push(Reverse((t + MS, c, false)));
                continue;
            }
            sent_at[c] = t;
            if busy < capacity {
                busy += 1;
                events.push(Reverse((t + service(t), c, true)));
            } else {
                queue.push_back(c);
            }
        }
        latencies.sort_unstable();
        let at = |p: usize| {
            latencies
                .get(latencies.len() * p / 100)
                .copied()
                .unwrap_or(0)
                / 1_000
        };
        let seconds = (END - from) as f64 / 1e9;
        Run {
            goodput: done as f64 / seconds,
            p50_us: at(50),
            p99_us: at(99),
            refused: refused as f64 / seconds,
        }
    }

    #[test]
    #[allow(
        clippy::print_stdout,
        reason = "the sweep's table is the experiment's record"
    )]
    fn test_load_sweep_gradient_against_fixed_caps() {
        // The endpoint serves 32 at once, about 32,000 requests per second.
        let capacity = 32;
        let mut rows = Vec::new();
        for clients in [16, 32, 64, 128, 256] {
            let high = simulate(
                Admit::Fixed(crate::DEFAULT_MAX_CONNECTIONS),
                clients,
                capacity,
            );
            let exact = simulate(Admit::Fixed(32), clients, capacity);
            let grad = simulate(Admit::Gradient, clients, capacity);
            rows.push((
                clients,
                high.p99_us,
                grad.p99_us,
                grad.goodput,
                exact.goodput,
            ));
            std::println!(
                "I-5 clients {clients:3}: fixed 256 {:6.0}/s p50 {:5} us p99 {:5} us | \
                 fixed 32 {:6.0}/s p99 {:5} us refused {:6.0}/s | \
                 gradient {:6.0}/s p50 {:5} us p99 {:5} us refused {:6.0}/s",
                high.goodput,
                high.p50_us,
                high.p99_us,
                exact.goodput,
                exact.p99_us,
                exact.refused,
                grad.goodput,
                grad.p50_us,
                grad.p99_us,
                grad.refused,
            );
            // Below the knee nothing is lost.
            if clients <= capacity {
                assert!(grad.goodput >= high.goodput * 0.95, "clients {clients}");
            }
        }
        // Far past the knee: the capacity kept, and p99 at least a third
        // below what a cap that is too high allows.
        let (_, high_p99, grad_p99, grad_goodput, exact_goodput) = rows[rows.len() - 1];
        assert!(
            grad_goodput >= exact_goodput * 0.95,
            "{grad_goodput} vs {exact_goodput}"
        );
        assert!(grad_p99 * 3 < high_p99 * 2, "{grad_p99} vs {high_p99}");
        // The endpoint halves its speed under overload: the limit follows
        // it down without collapsing.
        let high = simulate_with(
            Admit::Fixed(crate::DEFAULT_MAX_CONNECTIONS),
            256,
            capacity,
            Some(()),
        );
        let grad = simulate_with(Admit::Gradient, 256, capacity, Some(()));
        std::println!(
            "I-5 slower endpoint, 256 clients: fixed 256 {:6.0}/s p99 {:5} us | gradient {:6.0}/s p50 {:5} us p99 {:5} us",
            high.goodput,
            high.p99_us,
            grad.goodput,
            grad.p50_us,
            grad.p99_us
        );
        assert!(
            grad.goodput >= high.goodput * 0.95,
            "{} vs {}",
            grad.goodput,
            high.goodput
        );
        assert!(
            grad.p99_us * 2 < high.p99_us,
            "{} vs {}",
            grad.p99_us,
            high.p99_us
        );
    }
}
