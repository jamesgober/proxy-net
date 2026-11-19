//! The running state behind a `Proxy`: per pool, its balancer, health, retry
//! budget, and connections; and the slot that swaps it on a reload.

use std::boxed::Box;
use std::net::SocketAddr;
use std::string::String;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, RwLock};
use std::time::Instant;
use std::vec::Vec;

use super::connector::Connector;
use super::pool::EndpointConns;
use crate::config::{Config, ProxyBuilder};
use crate::core::{
    Balancer, EndpointId, EndpointState, HealthTable, RetryBudget, Tick, Transition,
};
use crate::endpoint::EndpointAddr;
use crate::observe::{Counters, Observer};

/// One pool's running state.
pub(crate) struct PoolRuntime {
    pub balancer: Balancer,
    pub health: HealthTable,
    pub budget: RetryBudget,
    pub conns: Box<[Arc<EndpointConns>]>,
    pub connector: Arc<dyn Connector>,
    /// The pool's TLS settings as a connector, for the WebSocket bridge,
    /// which drives its own sockets.
    #[cfg(all(feature = "tls", feature = "ws"))]
    pub tls: Option<Arc<super::tls::TlsConnector>>,
}

/// Response size statistics of one route (idea I-4).
#[derive(Default)]
pub(crate) struct RouteSizes {
    pub mean: AtomicU64,
    pub count: AtomicU64,
}

/// Everything one configuration holds while it runs.
pub(crate) struct Runtime {
    pub config: Config,
    pub pools: Box<[PoolRuntime]>,
    pub observer: Arc<dyn Observer>,
    pub origin: Instant,
    pub counters: Arc<Counters>,
    pub routes: Box<[RouteSizes]>,
    pub listener: Option<SocketAddr>,
    /// Counts reloads, so a request matched under one configuration is
    /// matched again under the next.
    pub generation: u32,
}

/// A pool's TLS connector, when it has TLS settings.
#[cfg(feature = "tls")]
type TlsPart = Option<Arc<super::tls::TlsConnector>>;

/// The connector for one pool: the configured one, or TCP (with TLS when the
/// pool has it and the `tls` feature is on). With TLS, also the TLS
/// connector itself, built once for both uses.
#[cfg(feature = "tls")]
fn connector_for(
    pool: &crate::pool::PoolConfig,
    custom: Option<&Arc<dyn Connector>>,
) -> Result<(Arc<dyn Connector>, TlsPart), crate::ConfigError> {
    let tls = match pool.tls() {
        Some(t) => Some(Arc::new(super::tls::TlsConnector::new(t).map_err(
            |reason| crate::ConfigError::new(crate::ConfigItem::Pool(pool.name().into()), reason),
        )?)),
        None => None,
    };
    let connector: Arc<dyn Connector> = match (custom, &tls) {
        (Some(c), _) => Arc::clone(c),
        (None, Some(t)) => Arc::clone(t) as Arc<dyn Connector>,
        (None, None) => Arc::new(super::connector::TcpConnector::new()),
    };
    Ok((connector, tls))
}

/// The connector for one pool: the configured one, or TCP.
#[cfg(not(feature = "tls"))]
fn connector_for(
    pool: &crate::pool::PoolConfig,
    custom: Option<&Arc<dyn Connector>>,
) -> Result<(Arc<dyn Connector>, ()), crate::ConfigError> {
    if let Some(c) = custom {
        return Ok((Arc::clone(c), ()));
    }
    if pool.tls().is_some() {
        return Err(crate::ConfigError::new(
            crate::ConfigItem::Pool(pool.name().into()),
            crate::ConfigReason::Inconsistent {
                what: "tls: build with the `tls` feature to connect with TLS",
            },
        ));
    }
    Ok((Arc::new(super::connector::TcpConnector::new()), ()))
}

impl Runtime {
    /// The running state of `config`. Endpoints also in `previous` (same pool
    /// name, same address) keep their health and their pooled connections.
    pub(crate) fn new(
        config: Config,
        custom: Option<&Arc<dyn Connector>>,
        observer: Arc<dyn Observer>,
        counters: Arc<Counters>,
        previous: Option<&Runtime>,
        listener: Option<SocketAddr>,
    ) -> Result<Runtime, crate::ConfigError> {
        let mut pools = Vec::with_capacity(config.pools().len());
        let origin = previous.map_or_else(Instant::now, |r| r.origin);
        let now = Tick(
            u64::try_from(Instant::now().saturating_duration_since(origin).as_nanos())
                .unwrap_or(u64::MAX),
        );
        for p in config.pools() {
            let balancer = Balancer::with_key(p, config.hash_key());
            let health = HealthTable::new(p);
            let old = previous.and_then(|r| {
                r.config
                    .pools()
                    .iter()
                    .position(|q| q.name() == p.name())
                    .map(|i| (r, i))
            });
            let mut conns = Vec::with_capacity(p.endpoints().len());
            for (j, e) in p.endpoints().iter().enumerate() {
                let carried = old.and_then(|(r, i)| {
                    let k = r.config.pools()[i]
                        .endpoints()
                        .iter()
                        .position(|x| x.addr() == e.addr())?;
                    health.adopt(
                        EndpointId(j as u16),
                        &r.pools[i].health,
                        EndpointId(k as u16),
                    );
                    balancer.adopt_ramp(
                        EndpointId(j as u16),
                        &r.pools[i].balancer,
                        EndpointId(k as u16),
                    );
                    Some(Arc::clone(&r.pools[i].conns[k]))
                });
                // An endpoint a live change adds to a running pool ramps up
                // like one that comes back.
                if carried.is_none() && old.is_some() && !p.slow_start().is_zero() {
                    balancer.start_ramp(EndpointId(j as u16), now);
                }
                conns
                    .push(carried.unwrap_or_else(|| {
                        Arc::new(EndpointConns::new(e.max_connections_value()))
                    }));
            }
            let _ = balancer.refresh(&health);
            #[cfg(all(feature = "tls", feature = "ws"))]
            let (connector, tls) = connector_for(p, custom)?;
            #[cfg(not(all(feature = "tls", feature = "ws")))]
            let (connector, _) = connector_for(p, custom)?;
            pools.push(PoolRuntime {
                balancer,
                health,
                budget: RetryBudget::new(p.retry()),
                conns: conns.into_boxed_slice(),
                connector,
                #[cfg(all(feature = "tls", feature = "ws"))]
                tls,
            });
        }
        let routes = (0..config.routes().len())
            .map(|_| RouteSizes::default())
            .collect();
        Ok(Runtime {
            config,
            pools: pools.into_boxed_slice(),
            observer,
            origin,
            counters,
            routes,
            listener,
            generation: previous.map_or(1, |r| r.generation.wrapping_add(1).max(1)),
        })
    }

    /// Now as the core's time.
    pub(crate) fn tick(&self) -> Tick {
        self.tick_at(Instant::now())
    }

    pub(crate) fn tick_at(&self, at: Instant) -> Tick {
        Tick(
            u64::try_from(at.saturating_duration_since(self.origin).as_nanos()).unwrap_or(u64::MAX),
        )
    }

    /// The instant of a core deadline.
    pub(crate) fn instant(&self, t: Tick) -> Instant {
        self.origin + std::time::Duration::from_nanos(t.0)
    }

    /// Reports a health change, starts a slow-start ramp when an endpoint
    /// comes back, and refreshes the pool's eligible set.
    pub(crate) fn changed(&self, pool: usize, t: Transition) {
        let (Some(rt), Some(cfg)) = (self.pools.get(pool), self.config.pool_at(pool)) else {
            return;
        };
        let back = t.to == EndpointState::Healthy
            && matches!(
                t.from,
                EndpointState::Ejected { .. } | EndpointState::Unhealthy
            );
        if back && !cfg.slow_start().is_zero() {
            rt.balancer.start_ramp(t.endpoint, self.tick());
        }
        let _ = rt.balancer.refresh(&rt.health);
        if let Some(e) = cfg.endpoints().get(t.endpoint.index()) {
            self.observer
                .health_changed(cfg.name(), e.addr(), t.from, t.to);
        }
    }

    /// The endpoint's connections.
    pub(crate) fn conns(&self, pool: usize, id: EndpointId) -> Option<&EndpointConns> {
        self.pools.get(pool)?.conns.get(id.index()).map(|c| &**c)
    }

    /// Records a response's size for its route and reports one far above the
    /// route's mean (idea I-4).
    pub(crate) fn response_size(&self, route: usize, bytes: u64) {
        let Some(r) = self.routes.get(route) else {
            return;
        };
        let n = r.count.fetch_add(1, Ordering::Relaxed);
        let mean = r.mean.load(Ordering::Relaxed);
        // A mean over at least 100 responses, and a response over 8 times it
        // and over 64 KiB, is reported (never blocked).
        if n >= 100 && mean > 0 && bytes > mean.saturating_mul(8) && bytes > 64 * 1024 {
            self.observer.response_outlier(route, bytes, mean);
        }
        let next = if n == 0 {
            bytes
        } else {
            let delta = (i128::from(bytes) - i128::from(mean)) / 32;
            (i128::from(mean) + delta).clamp(0, i128::from(u64::MAX)) as u64
        };
        r.mean.store(next, Ordering::Relaxed);
    }
}

/// The swappable state of a proxy: the current runtime, the builder it came
/// from (for live changes), and the drained endpoints.
pub(crate) struct Shared {
    current: RwLock<Arc<Runtime>>,
    builder: Mutex<ProxyBuilder>,
    drained: Mutex<Vec<(Box<str>, EndpointAddr)>>,
    pub listener: OnceLock<SocketAddr>,
    pub counters: Arc<Counters>,
    pub observer: Arc<dyn Observer>,
    pub connector: Option<Arc<dyn Connector>>,
    #[cfg_attr(not(feature = "serve"), allow(dead_code))]
    ids: AtomicU64,
    #[cfg_attr(not(feature = "serve"), allow(dead_code))]
    seed: u64,
}

impl Shared {
    /// A new request id: 32 hex digits, unique within the process and
    /// unpredictable from outside (two mixes of a counter and a start-up
    /// seed).
    #[cfg_attr(not(feature = "serve"), allow(dead_code))]
    pub(crate) fn next_request_id(&self) -> String {
        let n = self.ids.fetch_add(1, Ordering::Relaxed);
        let a = crate::core::jitter(self.seed ^ n);
        let b = crate::core::jitter(a ^ self.seed.rotate_left(17));
        format!("{a:016x}{b:016x}")
    }

    pub(crate) fn new(builder: ProxyBuilder, runtime: Runtime) -> Shared {
        let (observer, connector) = builder.parts();
        Shared {
            counters: Arc::clone(&runtime.counters),
            current: RwLock::new(Arc::new(runtime)),
            builder: Mutex::new(builder),
            drained: Mutex::new(Vec::new()),
            listener: OnceLock::new(),
            observer,
            connector,
            ids: AtomicU64::new(0),
            seed: {
                let t = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_nanos() as u64);
                crate::core::jitter(t ^ (std::process::id() as u64).rotate_left(32))
            },
        }
    }

    /// The runtime new exchanges use (one uncontended read lock).
    pub(crate) fn runtime(&self) -> Arc<Runtime> {
        Arc::clone(&self.current.read().unwrap_or_else(PoisonError::into_inner))
    }

    /// Builds `builder` into a new runtime that carries over the matching
    /// endpoints, and swaps it in. Exchanges in flight keep the runtime they
    /// started with.
    pub(crate) fn reload(&self, builder: ProxyBuilder) -> Result<(), crate::ConfigError> {
        let builder = super::super::proxy::discover(builder, &*self.observer);
        self.swap(builder)
    }

    /// As `reload`, keeping the endpoints the builder holds (no discovery).
    pub(crate) fn swap(&self, builder: ProxyBuilder) -> Result<(), crate::ConfigError> {
        let config = builder.clone().config()?;
        let old = self.runtime();
        let next = Runtime::new(
            config,
            self.connector.as_ref(),
            Arc::clone(&self.observer),
            Arc::clone(&self.counters),
            Some(&old),
            self.listener.get().copied(),
        )?;
        let drained = self
            .drained
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        for (pool, addr) in &drained {
            let _ = mark_drained(&next, pool, addr);
        }
        *self.builder.lock().unwrap_or_else(PoisonError::into_inner) = builder;
        *self.current.write().unwrap_or_else(PoisonError::into_inner) = Arc::new(next);
        Ok(())
    }

    /// A copy of the current builder.
    pub(crate) fn builder(&self) -> ProxyBuilder {
        self.builder
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Marks an endpoint draining now and after later reloads.
    pub(crate) fn drain(&self, pool: &str, addr: &EndpointAddr) -> bool {
        let rt = self.runtime();
        let found = mark_drained(&rt, pool, addr);
        if found {
            self.drained
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((pool.into(), addr.clone()));
        }
        found
    }

    /// Re-applies the listener address to the current runtime (after
    /// `Proxy::serve` binds).
    #[cfg_attr(not(feature = "serve"), allow(dead_code))]
    pub(crate) fn set_listener(&self, addr: SocketAddr) {
        let _ = self.listener.set(addr);
        let builder = self.builder();
        let _ = self.swap(builder);
    }
}

fn mark_drained(rt: &Runtime, pool: &str, addr: &EndpointAddr) -> bool {
    let Some(p) = rt.config.pools().iter().position(|p| p.name() == pool) else {
        return false;
    };
    let Some(i) = rt.config.pools()[p]
        .endpoints()
        .iter()
        .position(|e| e.addr() == addr)
    else {
        return false;
    };
    let id = EndpointId(i as u16);
    if let Some(t) = rt.pools[p].health.drain(id) {
        rt.changed(p, t);
    }
    if let Some(c) = rt.pools[p].conns.get(i) {
        c.close_idle();
    }
    true
}
