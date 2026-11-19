//! The running proxy: `Proxy`, its handle, and the health snapshot.

use std::boxed::Box;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::vec::Vec;

use crate::client::checker::{self, Stop};
use crate::client::forward::{Forward, RequestParts};
use crate::client::runtime::Runtime;
use crate::config::{Config, ProxyBuilder};
use crate::core::{Balancer, EndpointId, EndpointState};
use crate::endpoint::EndpointAddr;
use crate::error::ConfigError;
use crate::observe::ProxyStats;

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
}

impl std::fmt::Display for ProxyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProxyError::Config(e) => write!(f, "configuration: {e}"),
            #[cfg(feature = "serve")]
            ProxyError::Serve(e) => write!(f, "server: {e}"),
            ProxyError::Io(e) => write!(f, "health checker: {e}"),
        }
    }
}

impl std::error::Error for ProxyError {}

impl From<ConfigError> for ProxyError {
    fn from(e: ConfigError) -> ProxyError {
        ProxyError::Config(e)
    }
}

impl ProxyBuilder {
    /// Builds the proxy: checks the configuration as
    /// [`config`](ProxyBuilder::config) does, then starts its running state
    /// (balancers, health, connection pools, and the health checker thread).
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
    /// `Config` as `config` does; `Io` when the health checker thread
    /// cannot start.
    pub fn build(self) -> Result<Proxy, ProxyError> {
        let (observer, connector) = self.parts();
        let config = self.config()?;
        Proxy::start(Runtime::new(config, connector, observer)).map_err(ProxyError::Io)
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
    pub(crate) rt: Arc<Runtime>,
    pub(crate) stop: Arc<Stop>,
    pub(crate) checker: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for Proxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Proxy")
            .field("pools", &self.rt.pools.len())
            .finish_non_exhaustive()
    }
}

impl Proxy {
    pub(crate) fn start(rt: Runtime) -> std::io::Result<Proxy> {
        let rt = Arc::new(rt);
        let stop = Arc::new(Stop::default());
        let checker = checker::spawn(Arc::clone(&rt), Arc::clone(&stop))?;
        Ok(Proxy {
            rt,
            stop,
            checker: Some(checker),
        })
    }

    /// A handle for snapshots and control, cheap to clone.
    pub fn handle(&self) -> ProxyHandle {
        ProxyHandle {
            rt: Arc::clone(&self.rt),
        }
    }

    /// The configuration.
    pub fn config(&self) -> &Config {
        &self.rt.config
    }

    /// Starts forwarding one request that matched route `route`: the
    /// transport-neutral entry the serve-net service uses, for other inbound
    /// engines. `None` when the route does not exist.
    pub fn forward(&self, route: usize, parts: RequestParts) -> Option<Forward> {
        let pool = self.rt.config.routes().get(route)?.pool_index();
        Some(Forward::new(Arc::clone(&self.rt), route, pool, parts))
    }

    /// The hash key of `bytes` for pool `pool`'s consistent hashing.
    pub fn hash(&self, pool: usize, bytes: &[u8]) -> Option<u64> {
        self.rt.pools.get(pool).map(|p| p.balancer.hash(bytes))
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.stop.stop();
        if let Some(t) = self.checker.take() {
            let _ = t.join();
        }
        for pool in self.rt.pools.iter() {
            for conns in pool.conns.iter() {
                conns.close_idle();
            }
        }
    }
}

/// A handle to a running proxy: snapshots of health and counters.
#[derive(Clone)]
pub struct ProxyHandle {
    rt: Arc<Runtime>,
}

impl std::fmt::Debug for ProxyHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProxyHandle").finish_non_exhaustive()
    }
}

impl ProxyHandle {
    /// The health of every endpoint, read without locks.
    pub fn health(&self) -> HealthSnapshot {
        let pools = self
            .rt
            .config
            .pools()
            .iter()
            .zip(self.rt.pools.iter())
            .map(|(cfg, rt)| PoolHealth {
                name: cfg.name().into(),
                endpoints: cfg
                    .endpoints()
                    .iter()
                    .enumerate()
                    .map(|(i, e)| {
                        let id = EndpointId(i as u16);
                        let (open, idle) = rt.conns.get(i).map_or((0, 0), |c| c.counts());
                        EndpointHealth {
                            addr: e.addr().clone(),
                            state: rt.health.state(id),
                            eligible: rt.balancer.is_eligible(id),
                            active: rt.balancer.active(id),
                            open,
                            idle,
                            latency_ns: rt.balancer.latency(id),
                        }
                    })
                    .collect(),
            })
            .collect();
        HealthSnapshot { pools }
    }

    /// The proxy's counters.
    pub fn stats(&self) -> ProxyStats {
        self.rt.counters.snapshot()
    }

    /// The balancer of pool `pool`, for callers that drive the core
    /// themselves.
    pub fn balancer(&self, pool: usize) -> Option<&Balancer> {
        self.rt.pools.get(pool).map(|p| &p.balancer)
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
}
