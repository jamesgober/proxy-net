//! Balancing: which endpoint of a pool takes the next request.
//!
//! Picks read atomics only. The eligible set (healthy endpoints of the lowest
//! tier that has one, or every endpoint under the panic threshold) is
//! recomputed by [`Balancer::refresh`] after a health change, not per pick.

use alloc::boxed::Box;
use alloc::string::ToString;
use alloc::vec::Vec;

use super::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering, update_u32, update_u64};
use super::hash::{hash_key, mix};
use super::health::{EndpointId, HealthTable};
use crate::pool::{Balance, PoolConfig};

/// The longest round-robin schedule; larger weight sums are scaled down.
const MAX_SCHEDULE: u64 = 65_536;
/// The smallest Maglev table.
const MIN_TABLE: usize = 65_537;
/// Keys of the Maglev permutation: fixed, so every node builds the same table.
const OFFSET_KEY: [u64; 2] = [0x7072_6f78_792d_6e65, 0x742d_6f66_6673_6574];
const SKIP_KEY: [u64; 2] = [0x7072_6f78_792d_6e65, 0x742d_736b_6970_2d31];
/// The key for request hash keys (header, cookie, path, client address).
const REQUEST_KEY: [u64; 2] = [0, 0];

struct Member {
    weight: u64,
    tier: u8,
    active: AtomicU32,
    /// Latency estimate in nanoseconds (EWMA with a weight of 1/8).
    ewma: AtomicU64,
    eligible: AtomicBool,
}

enum Policy {
    RoundRobin {
        schedule: Box<[u16]>,
    },
    LeastConnections,
    PowerOfTwo {
        prefix: Box<[u64]>,
    },
    ConsistentHash {
        table: Box<[u16]>,
        load_percent: u16,
    },
    Random {
        prefix: Box<[u64]>,
    },
    First,
}

/// Balancing state for one pool.
///
/// ```
/// use proxy_net::core::{Balancer, HealthTable};
/// use proxy_net::{Pool, ProxyBuilder};
///
/// let config = ProxyBuilder::new()
///     .pool("app", Pool::new(["10.0.0.1:80", "10.0.0.2:80", "10.0.0.3:80"]))
///     .config()?;
/// let pool = config.pool("app").expect("pool");
/// let health = HealthTable::new(pool);
/// let balancer = Balancer::new(pool);
/// balancer.refresh(&health);
/// let picks: Vec<u16> = (0..6).map(|_| balancer.pick(None, &[]).expect("pick").0).collect();
/// assert_eq!(picks, [0, 1, 2, 0, 1, 2]);
/// # Ok::<(), proxy_net::ConfigError>(())
/// ```
pub struct Balancer {
    members: Box<[Member]>,
    policy: Policy,
    counter: AtomicU64,
    seed: u64,
    total_active: AtomicU64,
    eligible_count: AtomicU32,
    panic_below: u32,
    request_key: [u64; 2],
}

impl core::fmt::Debug for Balancer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Balancer")
            .field("endpoints", &self.members.len())
            .field("eligible", &self.eligible_count.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// Weights divided by their greatest common divisor and, when their sum is
/// larger than `cap`, scaled down (each at least 1).
fn reduced_weights(weights: &[u64], cap: u64) -> Vec<u64> {
    let g = weights.iter().copied().fold(0, gcd).max(1);
    let mut w: Vec<u64> = weights.iter().map(|x| x / g).collect();
    let sum: u64 = w.iter().sum();
    if sum > cap {
        for x in &mut w {
            *x = (*x * cap / sum).max(1);
        }
    }
    w
}

/// A smooth weighted round-robin schedule: endpoint `i` appears `w[i]` times
/// per cycle, its appearances spread evenly (stride scheduling: the k-th
/// appearance of `i` is at virtual time (2k + 1) / (2 w[i]); ties go to the
/// lower index).
fn schedule(weights: &[u64]) -> Box<[u16]> {
    let w = reduced_weights(weights, MAX_SCHEDULE);
    let mut events: Vec<(u64, u64, u16)> = Vec::new();
    for (i, &wi) in w.iter().enumerate() {
        for k in 0..wi {
            events.push((2 * k + 1, wi, i as u16));
        }
    }
    // Compare (2a + 1) / w_a with (2b + 1) / w_b by cross-multiplying.
    events.sort_by(|a, b| (a.0 * b.1).cmp(&(b.0 * a.1)).then(a.2.cmp(&b.2)));
    events.into_iter().map(|e| e.2).collect()
}

fn prefix_sums(weights: &[u64]) -> Box<[u64]> {
    let mut sum = 0;
    weights
        .iter()
        .map(|w| {
            sum += w;
            sum
        })
        .collect()
}

fn is_prime(n: usize) -> bool {
    if n < 2 {
        return false;
    }
    let mut d = 2;
    while d * d <= n {
        if n % d == 0 {
            return false;
        }
        d += 1;
    }
    true
}

/// The Maglev lookup table (Eisenbud et al., NSDI 2016) for endpoints named
/// `names` with weights `weights`: each endpoint fills the slots of its own
/// permutation in turn, `weight` slots per round, until the table is full.
fn maglev(names: &[Vec<u8>], weights: &[u64]) -> Box<[u16]> {
    let n = names.len();
    let mut m = (n * 100).max(MIN_TABLE);
    while !is_prime(m) {
        m += 1;
    }
    let w = reduced_weights(weights, (m / 2) as u64);
    let perms: Vec<(usize, usize)> = names
        .iter()
        .map(|name| {
            let offset = (hash_key(OFFSET_KEY, name) % m as u64) as usize;
            let skip = (hash_key(SKIP_KEY, name) % (m as u64 - 1)) as usize + 1;
            (offset, skip)
        })
        .collect();
    let mut next = alloc::vec![0usize; n];
    let mut table = alloc::vec![u16::MAX; m];
    let mut filled = 0;
    'fill: loop {
        for i in 0..n {
            for _ in 0..w[i] {
                let (offset, skip) = perms[i];
                let mut c = (offset + next[i] * skip) % m;
                while table[c] != u16::MAX {
                    next[i] += 1;
                    c = (offset + next[i] * skip) % m;
                }
                table[c] = i as u16;
                next[i] += 1;
                filled += 1;
                if filled == m {
                    break 'fill;
                }
            }
        }
    }
    table.into_boxed_slice()
}

impl Balancer {
    /// Balancing state for `pool`, with nothing eligible until the first
    /// [`refresh`](Balancer::refresh). Request keys hash with the all-zero
    /// key; see [`with_key`](Balancer::with_key).
    pub fn new(pool: &PoolConfig) -> Balancer {
        Balancer::with_key(pool, [0; 16])
    }

    /// As [`new`](Balancer::new), hashing request keys with `key` (the
    /// configuration's `hash_key`). Every node must use the same key to send
    /// a request key to the same endpoint.
    pub fn with_key(pool: &PoolConfig, key: [u8; 16]) -> Balancer {
        let endpoints = pool.endpoints();
        let weights: Vec<u64> = endpoints
            .iter()
            .map(|e| u64::from(e.weight_value()))
            .collect();
        let members = endpoints
            .iter()
            .map(|e| Member {
                weight: u64::from(e.weight_value()),
                tier: e.tier_value(),
                active: AtomicU32::new(0),
                ewma: AtomicU64::new(0),
                eligible: AtomicBool::new(false),
            })
            .collect();
        let policy = match pool.balance() {
            Balance::LeastConnections => Policy::LeastConnections,
            Balance::PowerOfTwo => Policy::PowerOfTwo {
                prefix: prefix_sums(&weights),
            },
            Balance::ConsistentHash { load_percent, .. } => {
                let names: Vec<Vec<u8>> = endpoints
                    .iter()
                    .map(|e| e.addr().to_string().into_bytes())
                    .collect();
                Policy::ConsistentHash {
                    table: maglev(&names, &weights),
                    load_percent: *load_percent,
                }
            }
            Balance::Random => Policy::Random {
                prefix: prefix_sums(&weights),
            },
            Balance::First => Policy::First,
            _ => Policy::RoundRobin {
                schedule: schedule(&weights),
            },
        };
        Balancer {
            members,
            policy,
            counter: AtomicU64::new(0),
            seed: hash_key([1, 2], pool.name().as_bytes()),
            total_active: AtomicU64::new(0),
            eligible_count: AtomicU32::new(0),
            panic_below: u32::from(pool.outlier().panic_below_value()),
            request_key: [
                u64::from_le_bytes([
                    key[0], key[1], key[2], key[3], key[4], key[5], key[6], key[7],
                ]),
                u64::from_le_bytes([
                    key[8], key[9], key[10], key[11], key[12], key[13], key[14], key[15],
                ]),
            ],
        }
    }

    /// The hash of a request key (a header value, a cookie, the path, or the
    /// client address as text), for [`pick`](Balancer::pick).
    ///
    /// ```
    /// use proxy_net::core::Balancer;
    /// assert_eq!(Balancer::key(b"session-1"), Balancer::key(b"session-1"));
    /// assert_ne!(Balancer::key(b"session-1"), Balancer::key(b"session-2"));
    /// ```
    pub fn key(bytes: &[u8]) -> u64 {
        hash_key(REQUEST_KEY, bytes)
    }

    /// The hash of a request key with this balancer's key (equal to
    /// [`key`](Balancer::key) under the default all-zero key).
    ///
    /// ```
    /// use proxy_net::core::Balancer;
    /// use proxy_net::{Pool, ProxyBuilder};
    /// let config = ProxyBuilder::new().pool("p", Pool::new(["10.0.0.1:80"])).config()?;
    /// let pool = config.pool("p").expect("pool");
    /// assert_eq!(Balancer::new(pool).hash(b"k"), Balancer::key(b"k"));
    /// assert_ne!(Balancer::with_key(pool, [7; 16]).hash(b"k"), Balancer::key(b"k"));
    /// # Ok::<(), proxy_net::ConfigError>(())
    /// ```
    pub fn hash(&self, bytes: &[u8]) -> u64 {
        hash_key(self.request_key, bytes)
    }

    /// Recomputes the eligible set from `health`: the healthy endpoints of the
    /// lowest tier that has one. When the healthy share of the pool is below
    /// the panic threshold, every endpoint that is not draining in the lowest
    /// tier present is eligible instead, so a mostly failed pool degrades
    /// rather than refusing everything. Returns the number eligible.
    pub fn refresh(&self, health: &HealthTable) -> usize {
        let n = self.members.len();
        let id = |i: usize| EndpointId(i as u16);
        let healthy: Vec<bool> = (0..n).map(|i| health.is_healthy(id(i))).collect();
        let live = (0..n).filter(|&i| !health.is_draining(id(i))).count();
        let healthy_count = healthy.iter().filter(|h| **h).count();
        let panic =
            live > 0 && (healthy_count as u64) * 100 < u64::from(self.panic_below) * live as u64;
        let mut eligible = alloc::vec![false; n];
        if panic {
            let tier = (0..n)
                .filter(|&i| !health.is_draining(id(i)))
                .map(|i| self.members[i].tier)
                .min();
            for (i, (e, m)) in eligible.iter_mut().zip(self.members.iter()).enumerate() {
                *e = Some(m.tier) == tier && !health.is_draining(id(i));
            }
        } else if let Some(tier) = (0..n)
            .filter(|&i| healthy[i])
            .map(|i| self.members[i].tier)
            .min()
        {
            for ((e, m), h) in eligible.iter_mut().zip(self.members.iter()).zip(&healthy) {
                *e = *h && m.tier == tier;
            }
        }
        let mut count = 0;
        for (m, e) in self.members.iter().zip(&eligible) {
            m.eligible.store(*e, Ordering::Release);
            count += usize::from(*e);
        }
        self.eligible_count
            .store(u32::try_from(count).unwrap_or(u32::MAX), Ordering::Release);
        count
    }

    /// True when endpoint `id` may be picked now.
    pub fn is_eligible(&self, id: EndpointId) -> bool {
        self.members
            .get(id.index())
            .is_some_and(|m| m.eligible.load(Ordering::Acquire))
    }

    /// Picks an endpoint, avoiding those in `exclude` (the ones a retry
    /// already tried) when another is eligible. `key` is the request's hash
    /// key for consistent hashing ([`key`](Balancer::key)); without one, a
    /// consistent-hash pool balances round robin. `None` when nothing is
    /// eligible.
    pub fn pick(&self, key: Option<u64>, exclude: &[EndpointId]) -> Option<EndpointId> {
        if self.eligible_count.load(Ordering::Acquire) == 0 {
            return None;
        }
        self.pick_with(key, &|i| self.usable(i, exclude))
            .or_else(|| self.pick_with(key, &|i| self.usable(i, &[])))
    }

    fn usable(&self, i: usize, exclude: &[EndpointId]) -> bool {
        self.members[i].eligible.load(Ordering::Acquire) && !exclude.iter().any(|e| e.index() == i)
    }

    fn next(&self) -> u64 {
        self.counter.fetch_add(1, Ordering::Relaxed)
    }

    fn pick_with(&self, key: Option<u64>, ok: &dyn Fn(usize) -> bool) -> Option<EndpointId> {
        let n = self.members.len();
        let id = |i: usize| Some(EndpointId(i as u16));
        match &self.policy {
            Policy::RoundRobin { schedule } => self.round_robin(schedule, ok),
            Policy::LeastConnections => {
                let start = (self.next() % n as u64) as usize;
                let mut best: Option<usize> = None;
                for k in 0..n {
                    let i = (start + k) % n;
                    if !ok(i) {
                        continue;
                    }
                    best = match best {
                        Some(b) if !self.fewer(i, b) => Some(b),
                        _ => Some(i),
                    };
                }
                best.and_then(id)
            }
            Policy::PowerOfTwo { prefix } => {
                let r = mix(self.seed ^ self.next());
                let a = weighted(prefix, r);
                let b = weighted(prefix, mix(r));
                match (ok(a), ok(b)) {
                    (true, true) => {
                        let ma = &self.members[a];
                        let mb = &self.members[b];
                        let (aa, ab) = (
                            ma.active.load(Ordering::Relaxed),
                            mb.active.load(Ordering::Relaxed),
                        );
                        let pick_b = ab < aa
                            || (ab == aa
                                && mb.ewma.load(Ordering::Relaxed)
                                    < ma.ewma.load(Ordering::Relaxed));
                        id(if pick_b { b } else { a })
                    }
                    (true, false) => id(a),
                    (false, true) => id(b),
                    (false, false) => self.scan(r, ok),
                }
            }
            Policy::Random { prefix } => {
                let r = mix(self.seed ^ self.next());
                let a = weighted(prefix, r);
                if ok(a) { id(a) } else { self.scan(r, ok) }
            }
            Policy::First => (0..n).find(|&i| ok(i)).and_then(id),
            Policy::ConsistentHash {
                table,
                load_percent,
            } => match key {
                None => self.scan(self.next(), ok),
                Some(h) => self.consistent(table, *load_percent, h, ok),
            },
        }
    }

    /// True when `a` has fewer active requests for its weight than `b`.
    fn fewer(&self, a: usize, b: usize) -> bool {
        let (ma, mb) = (&self.members[a], &self.members[b]);
        u64::from(ma.active.load(Ordering::Relaxed)) * mb.weight
            < u64::from(mb.active.load(Ordering::Relaxed)) * ma.weight
    }

    fn round_robin(&self, schedule: &[u16], ok: &dyn Fn(usize) -> bool) -> Option<EndpointId> {
        let len = schedule.len() as u64;
        let start = self.next();
        (0..len)
            .map(|k| usize::from(schedule[((start + k) % len) as usize]))
            .find(|&i| ok(i))
            .map(|i| EndpointId(i as u16))
    }

    /// The first usable endpoint from a position derived from `r`.
    fn scan(&self, r: u64, ok: &dyn Fn(usize) -> bool) -> Option<EndpointId> {
        let n = self.members.len();
        let start = (r % n as u64) as usize;
        (0..n)
            .map(|k| (start + k) % n)
            .find(|&i| ok(i))
            .map(|i| EndpointId(i as u16))
    }

    fn consistent(
        &self,
        table: &[u16],
        load_percent: u16,
        h: u64,
        ok: &dyn Fn(usize) -> bool,
    ) -> Option<EndpointId> {
        let m = table.len();
        let slot = (h % m as u64) as usize;
        let capacity = if load_percent == 0 {
            u64::MAX
        } else {
            // ceil(percent x (total + 1) / (100 x eligible)), Mirrokni et al.
            let eligible = u64::from(self.eligible_count.load(Ordering::Acquire)).max(1);
            let total = self.total_active.load(Ordering::Acquire) + 1;
            (u64::from(load_percent) * total).div_ceil(100 * eligible)
        };
        let mut fallback = None;
        for k in 0..m {
            let i = usize::from(table[(slot + k) % m]);
            if !ok(i) {
                continue;
            }
            if u64::from(self.members[i].active.load(Ordering::Relaxed)) < capacity {
                return Some(EndpointId(i as u16));
            }
            let _ = fallback.get_or_insert(i);
        }
        fallback.map(|i| EndpointId(i as u16))
    }

    /// An exchange started on `id`.
    pub fn started(&self, id: EndpointId) {
        if let Some(m) = self.members.get(id.index()) {
            let _ = m.active.fetch_add(1, Ordering::AcqRel);
            let _ = self.total_active.fetch_add(1, Ordering::AcqRel);
        }
    }

    /// An exchange on `id` finished; `latency_ns` (time to the response head,
    /// or 0 when there was none) updates its latency estimate.
    pub fn finished(&self, id: EndpointId, latency_ns: u64) {
        if let Some(m) = self.members.get(id.index()) {
            let _ = update_u32(&m.active, |a| a.checked_sub(1));
            let _ = update_u64(&self.total_active, |a| a.checked_sub(1));
            if latency_ns > 0 {
                let _ = update_u64(&m.ewma, |old| {
                    Some(if old == 0 {
                        latency_ns
                    } else {
                        // old + (new - old) / 8, in signed arithmetic.
                        let delta = (latency_ns as i128 - old as i128) / 8;
                        (old as i128 + delta).clamp(0, u64::MAX as i128) as u64
                    })
                });
            }
        }
    }

    /// Active exchanges on `id`.
    pub fn active(&self, id: EndpointId) -> u32 {
        self.members
            .get(id.index())
            .map_or(0, |m| m.active.load(Ordering::Acquire))
    }

    /// The latency estimate of `id`, in nanoseconds (0 before any).
    pub fn latency(&self, id: EndpointId) -> u64 {
        self.members
            .get(id.index())
            .map_or(0, |m| m.ewma.load(Ordering::Acquire))
    }

    /// The number of eligible endpoints.
    pub fn eligible_count(&self) -> usize {
        self.eligible_count.load(Ordering::Acquire) as usize
    }
}

/// The index whose prefix-sum range holds `r % total`.
fn weighted(prefix: &[u64], r: u64) -> usize {
    let total = prefix.last().copied().unwrap_or(1).max(1);
    let x = r % total;
    prefix
        .partition_point(|&p| p <= x)
        .min(prefix.len().saturating_sub(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Outcome, Tick};
    use crate::{Endpoint, HashKey, Outlier, Pool, ProxyBuilder};

    fn pool_of(p: Pool) -> PoolConfig {
        ProxyBuilder::new()
            .pool("p", p)
            .config()
            .expect("config")
            .pool("p")
            .expect("pool")
            .clone()
    }

    fn addrs(n: usize) -> Vec<alloc::string::String> {
        (0..n)
            .map(|i| alloc::format!("10.0.{}.{}:80", i / 250, i % 250 + 1))
            .collect()
    }

    fn ready(p: &PoolConfig) -> (Balancer, HealthTable) {
        let b = Balancer::new(p);
        let h = HealthTable::new(p);
        let _ = b.refresh(&h);
        (b, h)
    }

    #[test]
    fn test_schedule_counts_and_spreads() {
        let s = schedule(&[5, 1, 1]);
        assert_eq!(s.len(), 7);
        assert_eq!(s.iter().filter(|&&i| i == 0).count(), 5);
        // Spread: across the cycle boundary too, the gap between two turns of
        // an endpoint is at most ceil(len / weight) + 1.
        for (e, w) in [(0u16, 5usize), (1, 1), (2, 1)] {
            let at: Vec<usize> = s
                .iter()
                .chain(s.iter())
                .enumerate()
                .filter(|(_, i)| **i == e)
                .map(|(p, _)| p)
                .collect();
            let gap = at.windows(2).map(|p| p[1] - p[0]).max().unwrap_or(0);
            assert!(gap <= s.len().div_ceil(w) + 1, "{e}: {s:?}");
        }
        assert_eq!(&*schedule(&[2, 2]), &[0, 1]);
        assert_eq!(schedule(&[1000; 100]).len(), 100);
    }

    #[test]
    fn test_reduced_weights_scale() {
        assert_eq!(reduced_weights(&[4, 8], 100), [1, 2]);
        let w = reduced_weights(&[1000, 1], 10);
        assert_eq!(w, [9, 1]);
    }

    #[test]
    fn test_round_robin_weights() {
        let p = pool_of(
            Pool::new(Vec::<&str>::new())
                .endpoint(Endpoint::parse("10.0.0.1:80").expect("e").weight(3))
                .endpoint(Endpoint::parse("10.0.0.2:80").expect("e").weight(1)),
        );
        let (b, _) = ready(&p);
        let picks: Vec<u16> = (0..8).map(|_| b.pick(None, &[]).expect("pick").0).collect();
        assert_eq!(picks.iter().filter(|&&i| i == 0).count(), 6);
    }

    #[test]
    fn test_least_connections_and_counters() {
        let p = pool_of(Pool::new(addrs(3)).balance(Balance::LeastConnections));
        let (b, _) = ready(&p);
        b.started(EndpointId(0));
        b.started(EndpointId(1));
        assert_eq!(b.pick(None, &[]), Some(EndpointId(2)));
        b.started(EndpointId(2));
        b.started(EndpointId(2));
        let pick = b.pick(None, &[]).expect("pick");
        assert!(pick == EndpointId(0) || pick == EndpointId(1));
        assert_eq!(b.active(EndpointId(2)), 2);
        b.finished(EndpointId(2), 1_000);
        b.finished(EndpointId(2), 0);
        b.finished(EndpointId(2), 0);
        assert_eq!(b.active(EndpointId(2)), 0);
        assert_eq!(b.latency(EndpointId(2)), 1_000);
        b.finished(EndpointId(1), 9_000);
        assert_eq!(b.latency(EndpointId(1)), 9_000);
        b.finished(EndpointId(1), 1_000);
        assert_eq!(b.latency(EndpointId(1)), 8_000);
        assert_eq!(b.active(EndpointId(9)), 0);
        b.started(EndpointId(9));
        b.finished(EndpointId(9), 5);
    }

    #[test]
    fn test_exclude_falls_back_to_the_only_endpoint() {
        let p = pool_of(Pool::new(addrs(2)));
        let (b, _) = ready(&p);
        for _ in 0..4 {
            assert_eq!(b.pick(None, &[EndpointId(0)]), Some(EndpointId(1)));
        }
        let one = pool_of(Pool::new(addrs(1)));
        let (b, _) = ready(&one);
        assert_eq!(b.pick(None, &[EndpointId(0)]), Some(EndpointId(0)));
    }

    #[test]
    fn test_tiers_and_panic_threshold() {
        let p = pool_of(
            Pool::new(addrs(2))
                .endpoint(Endpoint::parse("10.9.0.1:80").expect("e").tier(1))
                .outlier(
                    Outlier::default()
                        .consecutive_failures(1)
                        .max_ejected(100)
                        .panic_below(0),
                ),
        );
        let (b, h) = ready(&p);
        assert_eq!(b.eligible_count(), 2);
        assert!(!b.is_eligible(EndpointId(2)));
        for i in 0..2 {
            let _ = h.record(EndpointId(i), Outcome::Failure, Tick(0));
        }
        assert_eq!(b.refresh(&h), 1);
        assert_eq!(b.pick(None, &[]), Some(EndpointId(2)));
        let _ = h.record(EndpointId(2), Outcome::Failure, Tick(0));
        assert_eq!(b.refresh(&h), 0);
        assert_eq!(b.pick(None, &[]), None);
        // With the panic threshold on, a dead pool balances over tier 0.
        let p = pool_of(
            Pool::new(addrs(2))
                .outlier(Outlier::default().consecutive_failures(1).max_ejected(100)),
        );
        let (b, h) = ready(&p);
        for i in 0..2 {
            let _ = h.record(EndpointId(i), Outcome::Failure, Tick(0));
        }
        assert_eq!(b.refresh(&h), 2);
        assert!(b.pick(None, &[]).is_some());
    }

    #[test]
    fn test_power_of_two_prefers_less_loaded() {
        let p = pool_of(Pool::new(addrs(2)).balance(Balance::PowerOfTwo));
        let (b, _) = ready(&p);
        for _ in 0..10 {
            b.started(EndpointId(0));
        }
        let picks = (0..200)
            .filter(|_| b.pick(None, &[]) == Some(EndpointId(1)))
            .count();
        // Whenever both are sampled, 1 wins; it is picked far more often.
        assert!(picks > 120, "{picks}");
    }

    #[test]
    fn test_random_and_first() {
        let p = pool_of(Pool::new(addrs(3)).balance(Balance::Random));
        let (b, _) = ready(&p);
        let mut seen = [0usize; 3];
        for _ in 0..3000 {
            seen[b.pick(None, &[]).expect("pick").index()] += 1;
        }
        assert!(seen.iter().all(|&c| c > 800), "{seen:?}");
        let p = pool_of(Pool::new(addrs(3)).balance(Balance::First));
        let (b, _) = ready(&p);
        assert_eq!(b.pick(None, &[]), Some(EndpointId(0)));
        assert_eq!(b.pick(None, &[EndpointId(0)]), Some(EndpointId(1)));
    }

    #[test]
    fn test_consistent_hash_is_stable_and_bounded() {
        let p = pool_of(Pool::new(addrs(5)).balance(Balance::consistent_hash(HashKey::Path)));
        let (b, h) = ready(&p);
        let k = Balancer::key(b"/users/42");
        let first = b.pick(Some(k), &[]).expect("pick");
        for _ in 0..10 {
            assert_eq!(b.pick(Some(k), &[]), Some(first));
        }
        // The same configuration gives the same answer on another node.
        let (b2, _) = ready(&p);
        assert_eq!(b2.pick(Some(k), &[]), Some(first));
        // Without a key: round robin over the eligible set.
        assert!(b.pick(None, &[]).is_some());
        // Over the bound, the key moves on; under it, it stays.
        for _ in 0..50 {
            b.started(first);
        }
        assert_ne!(b.pick(Some(k), &[]), Some(first));
        let _ = h;
    }

    #[test]
    fn test_maglev_minimal_disruption() {
        // E-154: removing one of n endpoints moves about 1/n of the keys.
        let n = 10;
        let names: Vec<Vec<u8>> = addrs(n).into_iter().map(|s| s.into_bytes()).collect();
        let full = maglev(&names, &alloc::vec![1; n]);
        assert_eq!(full.len(), 65_537);
        let mut counts = alloc::vec![0usize; n];
        for &e in full.iter() {
            counts[usize::from(e)] += 1;
        }
        let even = full.len() / n;
        assert!(
            counts.iter().all(|&c| c.abs_diff(even) <= even / 50),
            "{counts:?}"
        );
        let fewer = maglev(&names[1..], &alloc::vec![1; n - 1]);
        let moved = full
            .iter()
            .zip(fewer.iter())
            .filter(|(a, b)| **a != 0 && usize::from(**a) != usize::from(**b) + 1)
            .count();
        // Maglev's bound is small; allow 1/n plus 10% of the table.
        assert!(moved < full.len() / 10, "{moved}");
        assert!(is_prime(65_537));
        assert!(!is_prime(1));
        assert!(!is_prime(65_536));
    }

    #[test]
    fn test_weighted_index() {
        let prefix = prefix_sums(&[1, 3]);
        assert_eq!(weighted(&prefix, 0), 0);
        assert_eq!(weighted(&prefix, 1), 1);
        assert_eq!(weighted(&prefix, 3), 1);
        assert_eq!(weighted(&prefix, 4), 0);
    }

    #[test]
    fn test_nothing_eligible_before_refresh() {
        let p = pool_of(Pool::new(addrs(2)));
        let b = Balancer::new(&p);
        assert_eq!(b.pick(None, &[]), None);
        assert!(alloc::format!("{b:?}").contains("Balancer"));
    }
}
