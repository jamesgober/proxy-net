//! Upstream pools and their policies: balancing, health, retries, timeouts,
//! and connection limits.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::ops::RangeInclusive;
use core::time::Duration;

use crate::endpoint::{self, Endpoint};
use crate::error::{ConfigError, ConfigItem, ConfigReason};
use crate::syntax;

/// The most endpoints one pool may hold.
pub const MAX_ENDPOINTS: usize = 4096;

/// A set of upstream endpoints and the policies for sending them requests.
///
/// Endpoint text is parsed when the configuration is built, so a mistake is a
/// [`ConfigError`] naming the pool and the endpoint's position.
///
/// ```
/// use std::time::Duration;
/// use proxy_net::{Balance, HealthCheck, Pool, Timeouts};
/// let pool = Pool::new(["10.0.0.1:8080", "10.0.0.2:8080"])
///     .balance(Balance::LeastConnections)
///     .health(HealthCheck::http("/health").interval(Duration::from_secs(2)))
///     .timeouts(Timeouts::default().connect(Duration::from_secs(1)));
/// # let _ = pool;
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
#[must_use]
pub struct Pool {
    endpoints: Vec<PendingEndpoint>,
    balance: Balance,
    health: Option<HealthCheck>,
    outlier: Outlier,
    retry: Retry,
    timeouts: Timeouts,
    limits: PoolLimits,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum PendingEndpoint {
    Text(Box<str>),
    Parsed(Endpoint),
}

impl Pool {
    /// A pool of the endpoints written as `host:port`, `ip:port`, or
    /// `[ipv6]:port`, with the default policies: round robin, passive health,
    /// two attempts for idempotent requests, and the default timeouts and
    /// limits.
    pub fn new<I, S>(endpoints: I) -> Pool
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Pool {
            endpoints: endpoints
                .into_iter()
                .map(|s| PendingEndpoint::Text(s.as_ref().into()))
                .collect(),
            balance: Balance::RoundRobin,
            health: None,
            outlier: Outlier::default(),
            retry: Retry::default(),
            timeouts: Timeouts::default(),
            limits: PoolLimits::default(),
        }
    }

    /// Adds an endpoint built with its own weight, tier, or cap.
    ///
    /// ```
    /// use proxy_net::{Endpoint, Pool};
    /// let pool = Pool::new(["10.0.0.1:80"]).endpoint(Endpoint::parse("10.0.0.9:80")?.tier(1));
    /// # let _ = pool;
    /// # Ok::<(), proxy_net::ConfigError>(())
    /// ```
    pub fn endpoint(mut self, endpoint: Endpoint) -> Pool {
        self.endpoints.push(PendingEndpoint::Parsed(endpoint));
        self
    }

    /// How requests are spread over the endpoints. Default
    /// [`Balance::RoundRobin`].
    pub fn balance(mut self, policy: Balance) -> Pool {
        self.balance = policy;
        self
    }

    /// An active health check. Default: none (passive health only).
    pub fn health(mut self, check: HealthCheck) -> Pool {
        self.health = Some(check);
        self
    }

    /// Passive health from real traffic.
    pub fn outlier(mut self, outlier: Outlier) -> Pool {
        self.outlier = outlier;
        self
    }

    /// Retries of failed requests.
    pub fn retry(mut self, retry: Retry) -> Pool {
        self.retry = retry;
        self
    }

    /// Timeouts toward the endpoints.
    pub fn timeouts(mut self, timeouts: Timeouts) -> Pool {
        self.timeouts = timeouts;
        self
    }

    /// Connection limits toward the endpoints.
    pub fn limits(mut self, limits: PoolLimits) -> Pool {
        self.limits = limits;
        self
    }

    /// Checks everything and returns the pool's validated form.
    pub(crate) fn compile(self, name: &str) -> Result<PoolConfig, ConfigError> {
        let pool_err = |reason| ConfigError::new(ConfigItem::Pool(name.into()), reason);
        if !valid_pool_name(name) {
            return Err(pool_err(ConfigReason::PoolName));
        }
        if self.endpoints.is_empty() {
            return Err(pool_err(ConfigReason::NoEndpoints));
        }
        if self.endpoints.len() > MAX_ENDPOINTS {
            return Err(pool_err(ConfigReason::Limit { field: "endpoints" }));
        }
        let mut endpoints: Vec<Endpoint> = Vec::with_capacity(self.endpoints.len());
        for (index, pending) in self.endpoints.into_iter().enumerate() {
            let at = |reason| {
                ConfigError::new(
                    ConfigItem::Endpoint {
                        pool: Some(name.into()),
                        index: Some(index),
                    },
                    reason,
                )
            };
            let e = match pending {
                PendingEndpoint::Text(t) => endpoint::parse_addr(t.as_bytes())
                    .map(Endpoint::new)
                    .map_err(|k| at(ConfigReason::EndpointSyntax(k)))?,
                PendingEndpoint::Parsed(e) => e,
            };
            e.validate().map_err(at)?;
            if endpoints.iter().any(|x| x.addr() == e.addr()) {
                return Err(at(ConfigReason::DuplicateEndpoint));
            }
            endpoints.push(e);
        }
        self.balance.validate().map_err(pool_err)?;
        if let Some(h) = &self.health {
            h.validate().map_err(pool_err)?;
        }
        self.outlier.validate().map_err(pool_err)?;
        self.retry.validate().map_err(pool_err)?;
        self.timeouts.validate().map_err(pool_err)?;
        self.limits.validate().map_err(pool_err)?;
        Ok(PoolConfig {
            name: name.into(),
            endpoints: endpoints.into_boxed_slice(),
            balance: self.balance,
            health: self.health,
            outlier: self.outlier,
            retry: self.retry,
            timeouts: self.timeouts,
            limits: self.limits,
        })
    }
}

fn valid_pool_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// A pool after every endpoint and policy was checked.
///
/// ```
/// use proxy_net::{Balance, Pool, ProxyBuilder};
/// let config = ProxyBuilder::new()
///     .pool("app", Pool::new(["10.0.0.1:80", "10.0.0.2:80"]).balance(Balance::PowerOfTwo))
///     .config()?;
/// let pool = config.pool("app").expect("pool");
/// assert_eq!(pool.endpoints().len(), 2);
/// assert_eq!(pool.balance(), &Balance::PowerOfTwo);
/// # Ok::<(), proxy_net::ConfigError>(())
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PoolConfig {
    name: Box<str>,
    endpoints: Box<[Endpoint]>,
    balance: Balance,
    health: Option<HealthCheck>,
    outlier: Outlier,
    retry: Retry,
    timeouts: Timeouts,
    limits: PoolLimits,
}

impl PoolConfig {
    /// The pool's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The endpoints, in the order they were given.
    pub fn endpoints(&self) -> &[Endpoint] {
        &self.endpoints
    }

    /// The balancing policy.
    pub fn balance(&self) -> &Balance {
        &self.balance
    }

    /// The active health check, if any.
    pub fn health(&self) -> Option<&HealthCheck> {
        self.health.as_ref()
    }

    /// The passive health settings.
    pub fn outlier(&self) -> &Outlier {
        &self.outlier
    }

    /// The retry settings.
    pub fn retry(&self) -> &Retry {
        &self.retry
    }

    /// The timeouts.
    pub fn timeouts(&self) -> &Timeouts {
        &self.timeouts
    }

    /// The connection limits.
    pub fn limits(&self) -> &PoolLimits {
        &self.limits
    }
}

/// How a pool spreads requests over its endpoints.
///
/// ```
/// use proxy_net::{Balance, HashKey};
/// let sticky = Balance::consistent_hash(HashKey::Cookie("session".into())).bounded_load(150);
/// # let _ = sticky;
/// ```
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum Balance {
    /// Smooth weighted round robin (the default).
    #[default]
    RoundRobin,
    /// The endpoint with the fewest active requests for its weight.
    LeastConnections,
    /// Two endpoints picked at random by weight; the one with fewer
    /// outstanding requests wins, then the faster one.
    PowerOfTwo,
    /// Maglev consistent hashing on a key, with bounded load.
    ConsistentHash {
        /// What is hashed.
        key: HashKey,
        /// An endpoint above this percentage of the mean load passes the key
        /// on: 100 to 1,000, or 0 for no bound. Default 125.
        load_percent: u16,
    },
    /// An endpoint at random, by weight.
    Random,
    /// The first healthy endpoint in the order given (active and standby).
    First,
}

impl Balance {
    /// Consistent hashing on `key`, with the default load bound (125% of the
    /// mean).
    pub fn consistent_hash(key: HashKey) -> Balance {
        Balance::ConsistentHash {
            key,
            load_percent: 125,
        }
    }

    /// Sets the load bound of a consistent-hash policy, in percent of the mean
    /// (100 to 1,000), or 0 for none. Other policies are returned unchanged and
    /// a bound on them is refused when the configuration is built.
    #[must_use]
    pub fn bounded_load(self, percent: u16) -> Balance {
        match self {
            Balance::ConsistentHash { key, .. } => Balance::ConsistentHash {
                key,
                load_percent: percent,
            },
            other => other,
        }
    }

    fn validate(&self) -> Result<(), ConfigReason> {
        match self {
            Balance::ConsistentHash { key, load_percent } => {
                if *load_percent != 0 && !(100..=1000).contains(load_percent) {
                    return Err(ConfigReason::Limit {
                        field: "balance.load_percent",
                    });
                }
                key.validate()
            }
            _ => Ok(()),
        }
    }
}

/// What consistent hashing hashes.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HashKey {
    /// The client address serve-net resolved.
    ClientAddress,
    /// A request header (when absent, the request is balanced round robin).
    Header(Box<str>),
    /// A cookie (when absent, as for a header).
    Cookie(Box<str>),
    /// The path without the query.
    Path,
    /// The path and the query.
    PathAndQuery,
}

impl HashKey {
    fn validate(&self) -> Result<(), ConfigReason> {
        match self {
            HashKey::Header(n) | HashKey::Cookie(n) if !syntax::is_token(n.as_bytes()) => {
                Err(ConfigReason::HeaderName)
            }
            _ => Ok(()),
        }
    }
}

/// An active health check (FR-17): a request or a connect sent to every
/// endpoint on a schedule.
///
/// ```
/// use std::time::Duration;
/// use proxy_net::HealthCheck;
/// let check = HealthCheck::http("/ready")
///     .interval(Duration::from_secs(3))
///     .timeout(Duration::from_secs(1))
///     .rise(2)
///     .fall(2)
///     .expect_status(200..=204);
/// # let _ = check;
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
#[must_use]
pub struct HealthCheck {
    kind: CheckKind,
    interval: Duration,
    timeout: Duration,
    jitter_percent: u8,
    rise: u8,
    fall: u8,
}

/// What an active health check sends.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckKind {
    /// An HTTP request.
    Http {
        /// The request path.
        path: Box<str>,
        /// The `Host`, or the endpoint's server name when `None`.
        host: Option<Box<str>>,
        /// Status codes that count as healthy.
        expect: RangeInclusive<u16>,
        /// Text that must appear in the first 4 KiB of the body.
        body: Option<Box<str>>,
    },
    /// A TCP connect (and the TLS handshake when the pool uses TLS).
    Tcp,
}

impl HealthCheck {
    /// `GET path`, healthy on 200 to 399.
    pub fn http(path: &str) -> HealthCheck {
        HealthCheck::with_kind(CheckKind::Http {
            path: path.into(),
            host: None,
            expect: 200..=399,
            body: None,
        })
    }

    /// A connect only.
    pub fn tcp() -> HealthCheck {
        HealthCheck::with_kind(CheckKind::Tcp)
    }

    fn with_kind(kind: CheckKind) -> HealthCheck {
        HealthCheck {
            kind,
            interval: Duration::from_secs(5),
            timeout: Duration::from_secs(2),
            jitter_percent: 10,
            rise: 2,
            fall: 3,
        }
    }

    /// Time between checks of one endpoint, at least 100 ms. Default 5 s.
    pub fn interval(mut self, every: Duration) -> HealthCheck {
        self.interval = every;
        self
    }

    /// How long one check may take, below the interval. Default 2 s.
    pub fn timeout(mut self, within: Duration) -> HealthCheck {
        self.timeout = within;
        self
    }

    /// Random spread of each interval, 0 to 50 percent. Default 10.
    pub fn jitter(mut self, percent: u8) -> HealthCheck {
        self.jitter_percent = percent;
        self
    }

    /// Successes in a row that mark an endpoint healthy, 1 to 100. Default 2.
    pub fn rise(mut self, n: u8) -> HealthCheck {
        self.rise = n;
        self
    }

    /// Failures in a row that mark an endpoint unhealthy, 1 to 100. Default 3.
    pub fn fall(mut self, n: u8) -> HealthCheck {
        self.fall = n;
        self
    }

    /// Status codes that count as healthy (HTTP checks).
    pub fn expect_status(mut self, range: RangeInclusive<u16>) -> HealthCheck {
        if let CheckKind::Http { expect, .. } = &mut self.kind {
            *expect = range;
        }
        self
    }

    /// Text that must appear in the first 4 KiB of the body (HTTP checks).
    pub fn expect_body(mut self, contains: &str) -> HealthCheck {
        if let CheckKind::Http { body, .. } = &mut self.kind {
            *body = Some(contains.into());
        }
        self
    }

    /// The `Host` of the check request (HTTP checks).
    pub fn host(mut self, name: &str) -> HealthCheck {
        if let CheckKind::Http { host, .. } = &mut self.kind {
            *host = Some(name.into());
        }
        self
    }

    /// What the check sends.
    pub fn kind(&self) -> &CheckKind {
        &self.kind
    }

    /// The interval.
    pub fn interval_value(&self) -> Duration {
        self.interval
    }

    /// The timeout.
    pub fn timeout_value(&self) -> Duration {
        self.timeout
    }

    /// The jitter, in percent.
    pub fn jitter_value(&self) -> u8 {
        self.jitter_percent
    }

    /// The rise count.
    pub fn rise_value(&self) -> u8 {
        self.rise
    }

    /// The fall count.
    pub fn fall_value(&self) -> u8 {
        self.fall
    }

    fn validate(&self) -> Result<(), ConfigReason> {
        let limit = |field| Err(ConfigReason::Limit { field });
        if self.interval < Duration::from_millis(100) || self.interval > Duration::from_secs(3600) {
            return limit("health.interval");
        }
        if self.timeout.is_zero() || self.timeout >= self.interval {
            return limit("health.timeout");
        }
        if self.jitter_percent > 50 {
            return limit("health.jitter");
        }
        if !(1..=100).contains(&self.rise) {
            return limit("health.rise");
        }
        if !(1..=100).contains(&self.fall) {
            return limit("health.fall");
        }
        if let CheckKind::Http {
            path,
            host,
            expect,
            body,
        } = &self.kind
        {
            if !syntax::check_path(path.as_bytes()) {
                return Err(ConfigReason::PathPattern);
            }
            if let Some(h) = host {
                let ok = endpoint::parse_addr(h.as_bytes()).is_ok()
                    || syntax::host_name(h.as_bytes()).is_some()
                    || syntax::ipv4(h.as_bytes()).is_some();
                if !ok {
                    return Err(ConfigReason::HeaderValue);
                }
            }
            if *expect.start() < 100 || *expect.end() > 599 || expect.start() > expect.end() {
                return limit("health.expect_status");
            }
            if let Some(b) = body {
                if b.is_empty() || b.len() > 4096 {
                    return limit("health.expect_body");
                }
            }
        }
        Ok(())
    }
}

/// Passive health: endpoints that keep failing real requests are taken out
/// for a while (FR-16), and a pool that has lost most of its endpoints sends
/// to all of them rather than to none (FR-18).
///
/// ```
/// use std::time::Duration;
/// let outlier = proxy_net::Outlier::default()
///     .consecutive_failures(3)
///     .base_ejection(Duration::from_secs(10));
/// # let _ = outlier;
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
#[must_use]
pub struct Outlier {
    consecutive: u16,
    base_ejection: Duration,
    max_ejection: Duration,
    max_ejected_percent: u8,
    panic_below_percent: u8,
}

impl Default for Outlier {
    fn default() -> Outlier {
        Outlier {
            consecutive: 5,
            base_ejection: Duration::from_secs(30),
            max_ejection: Duration::from_secs(300),
            max_ejected_percent: 50,
            panic_below_percent: 50,
        }
    }
}

impl Outlier {
    /// Connect failures or gateway errors in a row that eject an endpoint; 0
    /// turns passive ejection off. Default 5.
    pub fn consecutive_failures(mut self, n: u16) -> Outlier {
        self.consecutive = n;
        self
    }

    /// How long the first ejection lasts; each later one lasts this times the
    /// number of ejections, up to [`max_ejection`](Outlier::max_ejection).
    /// Default 30 s.
    pub fn base_ejection(mut self, d: Duration) -> Outlier {
        self.base_ejection = d;
        self
    }

    /// The longest an ejection lasts. Default 300 s.
    pub fn max_ejection(mut self, d: Duration) -> Outlier {
        self.max_ejection = d;
        self
    }

    /// The most of a pool that may be ejected at once, in percent. Default 50.
    pub fn max_ejected(mut self, percent: u8) -> Outlier {
        self.max_ejected_percent = percent;
        self
    }

    /// Below this percentage of healthy endpoints, balance across all of them.
    /// 0 turns it off. Default 50.
    pub fn panic_below(mut self, percent: u8) -> Outlier {
        self.panic_below_percent = percent;
        self
    }

    /// The failure count.
    pub fn consecutive_value(&self) -> u16 {
        self.consecutive
    }

    /// The first ejection's length.
    pub fn base_ejection_value(&self) -> Duration {
        self.base_ejection
    }

    /// The longest ejection.
    pub fn max_ejection_value(&self) -> Duration {
        self.max_ejection
    }

    /// The ejection cap, in percent.
    pub fn max_ejected_value(&self) -> u8 {
        self.max_ejected_percent
    }

    /// The panic threshold, in percent.
    pub fn panic_below_value(&self) -> u8 {
        self.panic_below_percent
    }

    fn validate(&self) -> Result<(), ConfigReason> {
        let limit = |field| Err(ConfigReason::Limit { field });
        if self.consecutive > 0 && self.base_ejection.is_zero() {
            return limit("outlier.base_ejection");
        }
        if self.max_ejection < self.base_ejection {
            return limit("outlier.max_ejection");
        }
        if self.max_ejected_percent > 100 {
            return limit("outlier.max_ejected");
        }
        if self.panic_below_percent > 100 {
            return limit("outlier.panic_below");
        }
        Ok(())
    }
}

/// Retries of failed requests (FR-38, FR-39). Only idempotent methods are
/// retried, only when the request body is held in full, and only before any
/// byte of a response reached the client.
///
/// ```
/// let retry = proxy_net::Retry::default().attempts(3).on_status(&[503]);
/// # let _ = retry;
/// let none = proxy_net::Retry::off();
/// # let _ = none;
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
#[must_use]
pub struct Retry {
    attempts: u8,
    on_status: Vec<u16>,
    backoff: Duration,
    budget_percent: u8,
    budget_min: u16,
}

impl Default for Retry {
    fn default() -> Retry {
        Retry {
            attempts: 2,
            on_status: Vec::new(),
            backoff: Duration::from_millis(25),
            budget_percent: 20,
            budget_min: 3,
        }
    }
}

impl Retry {
    /// No retries: one attempt per request.
    pub fn off() -> Retry {
        Retry::default().attempts(1)
    }

    /// Attempts in total, 1 to 5. Default 2.
    pub fn attempts(mut self, n: u8) -> Retry {
        self.attempts = n;
        self
    }

    /// Upstream statuses (500 to 599) that are retried like a failure. Default:
    /// none.
    pub fn on_status(mut self, statuses: &[u16]) -> Retry {
        self.on_status = statuses.to_vec();
        self
    }

    /// The pause before a retry to the same endpoint (a retry to another
    /// endpoint does not wait), with full jitter. Default 25 ms.
    pub fn backoff(mut self, d: Duration) -> Retry {
        self.backoff = d;
        self
    }

    /// Retries in flight are kept under this percentage of active requests,
    /// or `min`, whichever is larger. Default 20 percent and 3.
    pub fn budget(mut self, percent: u8, min: u16) -> Retry {
        self.budget_percent = percent;
        self.budget_min = min;
        self
    }

    /// Attempts in total.
    pub fn attempts_value(&self) -> u8 {
        self.attempts
    }

    /// Statuses retried.
    pub fn on_status_value(&self) -> &[u16] {
        &self.on_status
    }

    /// The backoff.
    pub fn backoff_value(&self) -> Duration {
        self.backoff
    }

    /// The budget: percent and minimum.
    pub fn budget_value(&self) -> (u8, u16) {
        (self.budget_percent, self.budget_min)
    }

    fn validate(&self) -> Result<(), ConfigReason> {
        let limit = |field| Err(ConfigReason::Limit { field });
        if !(1..=5).contains(&self.attempts) {
            return limit("retry.attempts");
        }
        if self.on_status.iter().any(|s| !(500..=599).contains(s)) {
            return limit("retry.on_status");
        }
        if self.backoff > Duration::from_secs(10) {
            return limit("retry.backoff");
        }
        if self.budget_percent > 100 {
            return limit("retry.budget");
        }
        Ok(())
    }
}

/// Timeouts toward a pool's endpoints (FR-37).
///
/// ```
/// use std::time::Duration;
/// let t = proxy_net::Timeouts::default()
///     .connect(Duration::from_secs(1))
///     .response_head(Duration::from_secs(10))
///     .total(Duration::from_secs(120));
/// assert_eq!(t.connect_value(), Duration::from_secs(1));
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
#[must_use]
pub struct Timeouts {
    connect: Duration,
    response_head: Duration,
    read_idle: Duration,
    write_idle: Duration,
    total: Option<Duration>,
    tunnel_idle: Duration,
}

impl Default for Timeouts {
    fn default() -> Timeouts {
        Timeouts {
            connect: Duration::from_secs(5),
            response_head: Duration::from_secs(60),
            read_idle: Duration::from_secs(60),
            write_idle: Duration::from_secs(60),
            total: None,
            tunnel_idle: Duration::from_secs(300),
        }
    }
}

impl Timeouts {
    /// Connecting, TLS included. Default 5 s.
    pub fn connect(mut self, d: Duration) -> Timeouts {
        self.connect = d;
        self
    }

    /// From the request sent to the response head. Default 60 s.
    pub fn response_head(mut self, d: Duration) -> Timeouts {
        self.response_head = d;
        self
    }

    /// Between two reads from the endpoint. Default 60 s.
    pub fn read_idle(mut self, d: Duration) -> Timeouts {
        self.read_idle = d;
        self
    }

    /// A write to the endpoint that makes no progress. Default 60 s.
    pub fn write_idle(mut self, d: Duration) -> Timeouts {
        self.write_idle = d;
        self
    }

    /// The whole exchange. Default: no bound.
    pub fn total(mut self, d: Duration) -> Timeouts {
        self.total = Some(d);
        self
    }

    /// A bridged WebSocket with no message either way. Default 300 s.
    pub fn tunnel_idle(mut self, d: Duration) -> Timeouts {
        self.tunnel_idle = d;
        self
    }

    /// The connect timeout.
    pub fn connect_value(&self) -> Duration {
        self.connect
    }

    /// The response head timeout.
    pub fn response_head_value(&self) -> Duration {
        self.response_head
    }

    /// The read idle timeout.
    pub fn read_idle_value(&self) -> Duration {
        self.read_idle
    }

    /// The write idle timeout.
    pub fn write_idle_value(&self) -> Duration {
        self.write_idle
    }

    /// The total timeout.
    pub fn total_value(&self) -> Option<Duration> {
        self.total
    }

    /// The tunnel idle timeout.
    pub fn tunnel_idle_value(&self) -> Duration {
        self.tunnel_idle
    }

    fn validate(&self) -> Result<(), ConfigReason> {
        let limit = |field| Err(ConfigReason::Limit { field });
        let all = [
            (self.connect, "timeouts.connect"),
            (self.response_head, "timeouts.response_head"),
            (self.read_idle, "timeouts.read_idle"),
            (self.write_idle, "timeouts.write_idle"),
            (self.tunnel_idle, "timeouts.tunnel_idle"),
        ];
        for (d, field) in all {
            if d.is_zero() {
                return limit(field);
            }
        }
        if let Some(t) = self.total {
            if t < self.connect {
                return limit("timeouts.total");
            }
        }
        Ok(())
    }
}

/// Connection limits toward a pool's endpoints (FR-20, FR-35, FR-36).
///
/// ```
/// use std::time::Duration;
/// let l = proxy_net::PoolLimits::default().max_idle(8).idle_timeout(Duration::from_secs(2));
/// assert_eq!(l.max_idle_value(), 8);
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
#[must_use]
pub struct PoolLimits {
    max_pending: u32,
    pending_timeout: Duration,
    max_idle: u32,
    idle_timeout: Duration,
    max_exchanges_per_connection: u32,
    fresh_for: Duration,
}

impl Default for PoolLimits {
    fn default() -> PoolLimits {
        PoolLimits {
            max_pending: 1024,
            pending_timeout: Duration::from_secs(1),
            max_idle: 32,
            idle_timeout: Duration::from_secs(4),
            max_exchanges_per_connection: 1000,
            fresh_for: Duration::from_secs(1),
        }
    }
}

impl PoolLimits {
    /// Requests that may wait for a connection when every endpoint is at its
    /// cap; past it the answer is 503. Default 1,024.
    pub fn max_pending(mut self, n: u32) -> PoolLimits {
        self.max_pending = n;
        self
    }

    /// How long a request waits for a connection. Default 1 s.
    pub fn pending_timeout(mut self, d: Duration) -> PoolLimits {
        self.pending_timeout = d;
        self
    }

    /// Idle connections kept per endpoint; 0 turns reuse off. Default 32.
    pub fn max_idle(mut self, n: u32) -> PoolLimits {
        self.max_idle = n;
        self
    }

    /// How long an idle connection is kept; set it below the endpoint's own
    /// keep-alive timeout. Default 4 s.
    pub fn idle_timeout(mut self, d: Duration) -> PoolLimits {
        self.idle_timeout = d;
        self
    }

    /// Exchanges on one connection before it is closed. Default 1,000.
    pub fn max_exchanges_per_connection(mut self, n: u32) -> PoolLimits {
        self.max_exchanges_per_connection = n;
        self
    }

    /// A non-idempotent request only takes an idle connection used within this
    /// long (T-14). Default 1 s; at most the idle timeout.
    pub fn fresh_for(mut self, d: Duration) -> PoolLimits {
        self.fresh_for = d;
        self
    }

    /// The pending cap.
    pub fn max_pending_value(&self) -> u32 {
        self.max_pending
    }

    /// The pending timeout.
    pub fn pending_timeout_value(&self) -> Duration {
        self.pending_timeout
    }

    /// The idle cap.
    pub fn max_idle_value(&self) -> u32 {
        self.max_idle
    }

    /// The idle timeout.
    pub fn idle_timeout_value(&self) -> Duration {
        self.idle_timeout
    }

    /// Exchanges per connection.
    pub fn max_exchanges_per_connection_value(&self) -> u32 {
        self.max_exchanges_per_connection
    }

    /// The freshness window for non-idempotent requests.
    pub fn fresh_for_value(&self) -> Duration {
        self.fresh_for
    }

    fn validate(&self) -> Result<(), ConfigReason> {
        let limit = |field| Err(ConfigReason::Limit { field });
        if self.pending_timeout.is_zero() && self.max_pending > 0 {
            return limit("limits.pending_timeout");
        }
        if self.max_idle > 0 && self.idle_timeout.is_zero() {
            return limit("limits.idle_timeout");
        }
        if self.max_exchanges_per_connection == 0 {
            return limit("limits.max_exchanges_per_connection");
        }
        if self.fresh_for > self.idle_timeout {
            return limit("limits.fresh_for");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::EndpointErrorKind;

    fn reason(pool: Pool) -> ConfigReason {
        pool.compile("p").expect_err("refused").reason().clone()
    }

    #[test]
    fn test_compile_parses_endpoints_with_their_position() {
        let err = Pool::new(["10.0.0.1:80", "bad"])
            .compile("app")
            .expect_err("bad");
        assert_eq!(
            err.item(),
            &ConfigItem::Endpoint {
                pool: Some("app".into()),
                index: Some(1)
            }
        );
        assert_eq!(
            err.reason(),
            &ConfigReason::EndpointSyntax(EndpointErrorKind::MissingPort)
        );
        let ok = Pool::new(["10.0.0.1:80"])
            .endpoint(Endpoint::parse("h.internal:81").expect("e").weight(2))
            .compile("app")
            .expect("ok");
        assert_eq!(ok.name(), "app");
        assert_eq!(ok.endpoints().len(), 2);
        assert_eq!(ok.endpoints()[1].weight_value(), 2);
    }

    #[test]
    fn test_compile_refuses_pool_problems() {
        assert_eq!(
            reason(Pool::new(Vec::<&str>::new())),
            ConfigReason::NoEndpoints
        );
        assert_eq!(
            reason(Pool::new(["10.0.0.1:80", "10.0.0.1:80"])),
            ConfigReason::DuplicateEndpoint
        );
        assert_eq!(
            reason(
                Pool::new(["10.0.0.1:80"])
                    .endpoint(Endpoint::parse("10.0.0.2:80").expect("e").weight(0))
            ),
            ConfigReason::Weight
        );
        for bad in ["", "a b", "a/b", &"x".repeat(65)] {
            assert_eq!(
                Pool::new(["10.0.0.1:80"])
                    .compile(bad)
                    .expect_err("name")
                    .reason(),
                &ConfigReason::PoolName,
                "{bad}"
            );
        }
        let many: Vec<alloc::string::String> = (0..=MAX_ENDPOINTS)
            .map(|i| alloc::format!("10.{}.{}.1:80", i / 256, i % 256))
            .collect();
        assert_eq!(
            reason(Pool::new(many)),
            ConfigReason::Limit { field: "endpoints" }
        );
    }

    #[test]
    fn test_balance_validation() {
        let ok = |b: Balance| Pool::new(["10.0.0.1:80"]).balance(b).compile("p").is_ok();
        assert!(ok(Balance::RoundRobin));
        assert!(ok(Balance::consistent_hash(HashKey::ClientAddress)));
        assert!(ok(Balance::consistent_hash(HashKey::Path).bounded_load(0)));
        assert!(ok(
            Balance::consistent_hash(HashKey::Path).bounded_load(1000)
        ));
        assert!(!ok(Balance::consistent_hash(HashKey::Path).bounded_load(99)));
        assert!(!ok(Balance::consistent_hash(HashKey::Header("a b".into()))));
        assert!(!ok(Balance::consistent_hash(HashKey::Cookie("".into()))));
        assert_eq!(Balance::Random.bounded_load(150), Balance::Random);
        assert_eq!(Balance::default(), Balance::RoundRobin);
    }

    #[test]
    fn test_health_check_validation() {
        let check = |h: HealthCheck| {
            Pool::new(["10.0.0.1:80"])
                .health(h)
                .compile("p")
                .map(|_| ())
        };
        assert!(check(HealthCheck::http("/h")).is_ok());
        assert!(check(HealthCheck::tcp()).is_ok());
        let bad = [
            HealthCheck::http("/h").interval(Duration::from_millis(99)),
            HealthCheck::http("/h").timeout(Duration::from_secs(5)),
            HealthCheck::http("/h").timeout(Duration::ZERO),
            HealthCheck::http("/h").jitter(51),
            HealthCheck::http("/h").rise(0),
            HealthCheck::http("/h").fall(101),
            HealthCheck::http("h"),
            HealthCheck::http("/h").expect_status(99..=200),
            HealthCheck::http("/h").expect_status(RangeInclusive::new(300, 200)),
            HealthCheck::http("/h").expect_body(""),
            HealthCheck::http("/h").host("bad host"),
        ];
        for h in bad {
            assert!(check(h.clone()).is_err(), "{h:?}");
        }
        let h = HealthCheck::http("/h")
            .host("app.internal")
            .expect_body("ok");
        assert!(check(h.clone()).is_ok());
        assert_eq!(h.rise_value(), 2);
        assert_eq!(h.fall_value(), 3);
        assert_eq!(h.jitter_value(), 10);
        assert_eq!(h.interval_value(), Duration::from_secs(5));
        assert_eq!(h.timeout_value(), Duration::from_secs(2));
        assert!(matches!(h.kind(), CheckKind::Http { .. }));
        // Setters for HTTP checks leave a TCP check unchanged.
        assert_eq!(
            HealthCheck::tcp()
                .expect_body("x")
                .host("h")
                .expect_status(200..=200),
            HealthCheck::tcp()
        );
    }

    #[test]
    fn test_outlier_retry_timeouts_limits_validation() {
        let base = || Pool::new(["10.0.0.1:80"]);
        assert_eq!(
            reason(base().outlier(Outlier::default().base_ejection(Duration::ZERO))),
            ConfigReason::Limit {
                field: "outlier.base_ejection"
            }
        );
        assert!(
            base()
                .outlier(
                    Outlier::default()
                        .consecutive_failures(0)
                        .base_ejection(Duration::ZERO)
                )
                .compile("p")
                .is_ok()
        );
        assert_eq!(
            reason(base().outlier(Outlier::default().max_ejection(Duration::from_secs(1)))),
            ConfigReason::Limit {
                field: "outlier.max_ejection"
            }
        );
        assert_eq!(
            reason(base().outlier(Outlier::default().max_ejected(101))),
            ConfigReason::Limit {
                field: "outlier.max_ejected"
            }
        );
        assert_eq!(
            reason(base().outlier(Outlier::default().panic_below(101))),
            ConfigReason::Limit {
                field: "outlier.panic_below"
            }
        );
        assert_eq!(
            reason(base().retry(Retry::default().attempts(0))),
            ConfigReason::Limit {
                field: "retry.attempts"
            }
        );
        assert_eq!(
            reason(base().retry(Retry::default().attempts(6))),
            ConfigReason::Limit {
                field: "retry.attempts"
            }
        );
        assert_eq!(
            reason(base().retry(Retry::default().on_status(&[404]))),
            ConfigReason::Limit {
                field: "retry.on_status"
            }
        );
        assert_eq!(
            reason(base().retry(Retry::default().backoff(Duration::from_secs(11)))),
            ConfigReason::Limit {
                field: "retry.backoff"
            }
        );
        assert_eq!(
            reason(base().retry(Retry::default().budget(101, 3))),
            ConfigReason::Limit {
                field: "retry.budget"
            }
        );
        assert_eq!(Retry::off().attempts_value(), 1);
        assert_eq!(
            reason(base().timeouts(Timeouts::default().connect(Duration::ZERO))),
            ConfigReason::Limit {
                field: "timeouts.connect"
            }
        );
        assert_eq!(
            reason(base().timeouts(Timeouts::default().total(Duration::from_secs(1)))),
            ConfigReason::Limit {
                field: "timeouts.total"
            }
        );
        assert_eq!(
            reason(base().limits(PoolLimits::default().max_exchanges_per_connection(0))),
            ConfigReason::Limit {
                field: "limits.max_exchanges_per_connection"
            }
        );
        assert_eq!(
            reason(base().limits(PoolLimits::default().fresh_for(Duration::from_secs(5)))),
            ConfigReason::Limit {
                field: "limits.fresh_for"
            }
        );
        assert_eq!(
            reason(base().limits(PoolLimits::default().pending_timeout(Duration::ZERO))),
            ConfigReason::Limit {
                field: "limits.pending_timeout"
            }
        );
        assert_eq!(
            reason(
                base().limits(
                    PoolLimits::default()
                        .idle_timeout(Duration::ZERO)
                        .fresh_for(Duration::ZERO)
                )
            ),
            ConfigReason::Limit {
                field: "limits.idle_timeout"
            }
        );
        assert!(
            base()
                .limits(
                    PoolLimits::default()
                        .max_idle(0)
                        .idle_timeout(Duration::ZERO)
                        .fresh_for(Duration::ZERO)
                )
                .compile("p")
                .is_ok()
        );
    }

    #[test]
    fn test_defaults_and_accessors() {
        let p = Pool::new(["10.0.0.1:80"]).compile("p").expect("ok");
        assert_eq!(p.balance(), &Balance::RoundRobin);
        assert!(p.health().is_none());
        assert_eq!(p.outlier().consecutive_value(), 5);
        assert_eq!(p.outlier().base_ejection_value(), Duration::from_secs(30));
        assert_eq!(p.outlier().max_ejection_value(), Duration::from_secs(300));
        assert_eq!(p.outlier().max_ejected_value(), 50);
        assert_eq!(p.outlier().panic_below_value(), 50);
        assert_eq!(p.retry().attempts_value(), 2);
        assert!(p.retry().on_status_value().is_empty());
        assert_eq!(p.retry().backoff_value(), Duration::from_millis(25));
        assert_eq!(p.retry().budget_value(), (20, 3));
        let t = p.timeouts();
        assert_eq!(t.connect_value(), Duration::from_secs(5));
        assert_eq!(t.response_head_value(), Duration::from_secs(60));
        assert_eq!(t.read_idle_value(), Duration::from_secs(60));
        assert_eq!(t.write_idle_value(), Duration::from_secs(60));
        assert_eq!(t.total_value(), None);
        assert_eq!(t.tunnel_idle_value(), Duration::from_secs(300));
        let l = p.limits();
        assert_eq!(l.max_pending_value(), 1024);
        assert_eq!(l.pending_timeout_value(), Duration::from_secs(1));
        assert_eq!(l.max_idle_value(), 32);
        assert_eq!(l.idle_timeout_value(), Duration::from_secs(4));
        assert_eq!(l.max_exchanges_per_connection_value(), 1000);
        assert_eq!(l.fresh_for_value(), Duration::from_secs(1));
    }
}
