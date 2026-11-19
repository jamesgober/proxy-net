//! The running state behind a `Proxy`: per pool, its balancer, health, retry
//! budget, and connections.

use std::boxed::Box;
use std::sync::Arc;
use std::time::Instant;
use std::vec::Vec;

use super::connector::Connector;
use super::pool::EndpointConns;
use crate::config::Config;
use crate::core::{Balancer, EndpointId, HealthTable, RetryBudget, Tick, Transition};
use crate::observe::{Counters, Observer};

/// One pool's running state.
pub(crate) struct PoolRuntime {
    pub balancer: Balancer,
    pub health: HealthTable,
    pub budget: RetryBudget,
    pub conns: Box<[EndpointConns]>,
}

/// Everything a proxy holds while it runs.
pub(crate) struct Runtime {
    pub config: Config,
    pub pools: Box<[PoolRuntime]>,
    pub connector: Arc<dyn Connector>,
    pub observer: Arc<dyn Observer>,
    pub origin: Instant,
    pub counters: Counters,
}

impl Runtime {
    pub(crate) fn new(
        config: Config,
        connector: Arc<dyn Connector>,
        observer: Arc<dyn Observer>,
    ) -> Runtime {
        let pools: Vec<PoolRuntime> = config
            .pools()
            .iter()
            .map(|p| {
                let balancer = Balancer::with_key(p, config.hash_key());
                let health = HealthTable::new(p);
                let _ = balancer.refresh(&health);
                PoolRuntime {
                    balancer,
                    health,
                    budget: RetryBudget::new(p.retry()),
                    conns: p
                        .endpoints()
                        .iter()
                        .map(|e| EndpointConns::new(e.max_connections_value()))
                        .collect(),
                }
            })
            .collect();
        Runtime {
            config,
            pools: pools.into_boxed_slice(),
            connector,
            observer,
            origin: Instant::now(),
            counters: Counters::default(),
        }
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

    /// Reports a health change and refreshes the pool's eligible set.
    pub(crate) fn changed(&self, pool: usize, t: Transition) {
        let (Some(rt), Some(cfg)) = (self.pools.get(pool), self.config.pool_at(pool)) else {
            return;
        };
        let _ = rt.balancer.refresh(&rt.health);
        if let Some(e) = cfg.endpoints().get(t.endpoint.index()) {
            self.observer
                .health_changed(cfg.name(), e.addr(), t.from, t.to);
        }
    }

    /// The endpoint's connections.
    pub(crate) fn conns(&self, pool: usize, id: EndpointId) -> Option<&EndpointConns> {
        self.pools.get(pool)?.conns.get(id.index())
    }
}
