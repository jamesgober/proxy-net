//! The running proxy: `Proxy`, its handle, live changes, and the health
//! snapshot.

use std::boxed::Box;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::vec::Vec;

use crate::client::checker::{self, Stop};
use crate::client::discovery::allowed;
use crate::client::forward::{Forward, RequestParts};
use crate::client::runtime::{Runtime, Shared};
use crate::config::{Config, ProxyBuilder};
use crate::core::{EndpointId, EndpointState};
use crate::endpoint::{Endpoint, EndpointAddr};
use crate::error::ConfigError;
use crate::observe::{Counters, Observer, ProxyStats};

/// Why a proxy could not start.
#[non_exhaustive]
#[derive(Debug)]
pub enum ProxyError {
    /// The configuration was refused.
    Config(ConfigError),
    /// serve-net could not start (the listener could not be bound, or its
    /// configuration was refused).
    #[cfg(feature = "serve")]
    Serve(serve_net::ServeError),
    /// The health checker thread could not be started.
    Io(std::io::Error),
    /// An [`Inbound`](crate::Inbound) setting was refused; the text names it.
    #[cfg(feature = "serve")]
    Inbound(&'static str),
}

impl std::fmt::Display for ProxyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProxyError::Config(e) => write!(f, "configuration: {e}"),
            #[cfg(feature = "serve")]
            ProxyError::Serve(e) => write!(f, "server: {e}"),
            ProxyError::Io(e) => write!(f, "health checker: {e}"),
            #[cfg(feature = "serve")]
            ProxyError::Inbound(what) => write!(f, "inbound: {what}"),
        }
    }
}

impl std::error::Error for ProxyError {}

impl From<ConfigError> for ProxyError {
    fn from(e: ConfigError) -> ProxyError {
        ProxyError::Config(e)
    }
}

/// Asks every pool's discovery source for its endpoints and puts them in the
/// builder; a source that fails or gives nothing inside its allowlist leaves
/// the pool as it was, and the observer hears about it.
pub(crate) fn discover(builder: ProxyBuilder, observer: &dyn Observer) -> ProxyBuilder {
    builder.map_pools(|name, pool| {
        let Some(slot) = pool.discovery_slot().cloned() else {
            return pool;
        };
        match slot.0.endpoints() {
            Ok(found) => {
                let kept = allowed(found, pool.allowlist());
                if kept.is_empty() {
                    observer.discovery_failed(name, "no endpoint inside the allowlist");
                    pool
                } else {
                    pool.with_endpoints(&kept)
                }
            }
            Err(e) => {
                observer.discovery_failed(name, e.message());
                pool
            }
        }
    })
}

impl ProxyBuilder {
    /// Builds the proxy: asks discovery sources for their endpoints, checks
    /// the configuration as [`config`](ProxyBuilder::config) does, then
    /// starts its running state (balancers, health, connection pools, and
    /// the health checker thread).
    ///
    /// ```no_run
    /// let proxy = proxy_net::ProxyBuilder::new()
    ///     .pool("app", proxy_net::Pool::new(["127.0.0.1:3000"]))
    ///     .build()?;
    /// # let _ = proxy;
    /// # Ok::<(), proxy_net::ProxyError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// `Config` as `config` does (and for TLS settings that cannot be
    /// loaded); `Io` when the health checker thread cannot start.
    pub fn build(self) -> Result<Proxy, ProxyError> {
        let (observer, connector) = self.parts();
        let builder = discover(self, &*observer);
        let config = builder.clone().config()?;
        let counters = Arc::new(Counters::default());
        let rt = Runtime::new(config, connector.as_ref(), observer, counters, None, None)?;
        let shared = Arc::new(Shared::new(builder, rt));
        let stop = Arc::new(Stop::default());
        let checker =
            checker::spawn(Arc::clone(&shared), Arc::clone(&stop)).map_err(ProxyError::Io)?;
        Ok(Proxy {
            shared,
            stop,
            checker: Some(checker),
        })
    }
}

/// A proxy: the validated configuration and the running state behind it
/// (balancers, health, retry budgets, connection pools, and the health
/// checker thread). Built by [`ProxyBuilder::build`](crate::ProxyBuilder::build).
///
/// ```no_run
/// let proxy = proxy_net::ProxyBuilder::new()
///     .pool("app", proxy_net::Pool::new(["127.0.0.1:3000", "127.0.0.1:3001"]))
///     .build()?;
/// // With the `serve` feature: `let running = proxy.serve("127.0.0.1:8080")?;`
/// assert_eq!(proxy.handle().health().pools[0].endpoints.len(), 2);
/// # Ok::<(), proxy_net::ProxyError>(())
/// ```
pub struct Proxy {
    pub(crate) shared: Arc<Shared>,
    pub(crate) stop: Arc<Stop>,
    pub(crate) checker: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for Proxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Proxy").finish_non_exhaustive()
    }
}

impl Proxy {
    /// A handle for snapshots and live changes, cheap to clone.
    pub fn handle(&self) -> ProxyHandle {
        ProxyHandle {
            shared: Arc::clone(&self.shared),
        }
    }

    /// The current configuration.
    pub fn config(&self) -> Config {
        self.shared.runtime().config.clone()
    }

    /// Starts forwarding one request that matched route `route` of the
    /// current configuration: the transport-neutral entry the serve-net
    /// service uses, for other inbound engines. `None` when the route does
    /// not exist.
    pub fn forward(&self, route: usize, parts: RequestParts) -> Option<Forward> {
        let rt = self.shared.runtime();
        let pool = rt.config.routes().get(route)?.pool_index();
        Some(Forward::new(rt, route, pool, parts))
    }

    /// The hash key of `bytes` for pool `pool`'s consistent hashing.
    pub fn hash(&self, pool: usize, bytes: &[u8]) -> Option<u64> {
        self.shared
            .runtime()
            .pools
            .get(pool)
            .map(|p| p.balancer.hash(bytes))
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.stop.stop();
        if let Some(t) = self.checker.take() {
            let _ = t.join();
        }
        for pool in self.shared.runtime().pools.iter() {
            for conns in pool.conns.iter() {
                conns.close_idle();
            }
        }
    }
}

/// A handle to a running proxy: snapshots of health and counters, and live
/// changes.
#[derive(Clone)]
pub struct ProxyHandle {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for ProxyHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProxyHandle").finish_non_exhaustive()
    }
}

impl ProxyHandle {
    /// The health of every endpoint, read without locks.
    pub fn health(&self) -> HealthSnapshot {
        let rt = self.shared.runtime();
        let pools = rt
            .config
            .pools()
            .iter()
            .zip(rt.pools.iter())
            .map(|(cfg, p)| PoolHealth {
                name: cfg.name().into(),
                endpoints: cfg
                    .endpoints()
                    .iter()
                    .enumerate()
                    .map(|(i, e)| {
                        let id = EndpointId(i as u16);
                        let (open, idle) = p.conns.get(i).map_or((0, 0), |c| c.counts());
                        EndpointHealth {
                            addr: e.addr().clone(),
                            state: p.health.state(id),
                            eligible: p.balancer.is_eligible(id),
                            active: p.balancer.active(id),
                            open,
                            idle,
                            latency_ns: p.balancer.latency(id),
                            ramp: p.balancer.ramp(id),
                            adaptive_limit: p
                                .adaptive
                                .as_ref()
                                .and_then(|a| a.get(i))
                                .map(crate::core::adaptive::GradientLimit::limit),
                        }
                    })
                    .collect(),
            })
            .collect();
        HealthSnapshot { pools }
    }

    /// The proxy's counters (they run across reloads).
    pub fn stats(&self) -> ProxyStats {
        self.shared.counters.snapshot()
    }

    /// Whether endpoint `id` of pool `pool` may be picked now.
    pub fn is_eligible(&self, pool: usize, id: EndpointId) -> bool {
        self.shared
            .runtime()
            .pools
            .get(pool)
            .is_some_and(|p| p.balancer.is_eligible(id))
    }

    /// Replaces the whole configuration while traffic flows. Exchanges in
    /// flight finish on the configuration they started with; an endpoint in
    /// both (same pool name and address) keeps its health and its pooled
    /// connections; one that is gone takes nothing new.
    ///
    /// ```no_run
    /// let running = proxy_net::proxy("127.0.0.1:8080", ["127.0.0.1:3000"])?;
    /// running.handle().reload(
    ///     proxy_net::ProxyBuilder::new().pool("default", proxy_net::Pool::new(["127.0.0.1:3001"])),
    /// )?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    ///
    /// # Errors
    ///
    /// The new configuration's `ConfigError`; the running one is unchanged.
    pub fn reload(&self, builder: ProxyBuilder) -> Result<(), ConfigError> {
        self.shared.reload(builder)
    }

    /// Replaces the endpoints of pool `pool` (what discovery does).
    ///
    /// # Errors
    ///
    /// `UnknownPool` when there is no such pool, or the new configuration's
    /// `ConfigError`.
    pub fn set_endpoints(&self, pool: &str, endpoints: &[Endpoint]) -> Result<(), ConfigError> {
        let builder = self.shared.builder();
        if !builder.has_pool(pool) {
            return Err(ConfigError::new(
                crate::ConfigItem::Pool(pool.into()),
                crate::ConfigReason::UnknownPool,
            ));
        }
        let builder = builder.map_pools(|name, p| {
            if name == pool {
                p.with_endpoints(endpoints)
            } else {
                p
            }
        });
        self.shared.swap(builder)
    }

    /// Drains one endpoint: it takes no new exchanges, finishes the ones in
    /// flight, and its idle connections close; it stays draining across
    /// reloads. `false` when there is no such endpoint.
    pub fn drain_endpoint(&self, pool: &str, endpoint: &EndpointAddr) -> bool {
        self.shared.drain(pool, endpoint)
    }
}

/// The health of every pool.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HealthSnapshot {
    /// The pools, in configuration order.
    pub pools: Vec<PoolHealth>,
}

/// The health of one pool.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PoolHealth {
    /// The pool's name.
    pub name: Box<str>,
    /// Its endpoints, in configuration order.
    pub endpoints: Vec<EndpointHealth>,
}

/// The health of one endpoint.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EndpointHealth {
    /// Its address.
    pub addr: EndpointAddr,
    /// Its health state.
    pub state: EndpointState,
    /// Whether the balancer may pick it now.
    pub eligible: bool,
    /// Exchanges in flight on it.
    pub active: u32,
    /// Open connections (in use and idle).
    pub open: u32,
    /// Idle pooled connections.
    pub idle: u32,
    /// Its latency estimate, in nanoseconds.
    pub latency_ns: u64,
    /// Its slow-start share, in percent (100 when it is not ramping).
    pub ramp: u32,
    /// Its adaptive concurrency limit, when the pool has one (0.7).
    pub adaptive_limit: Option<u32>,
}
