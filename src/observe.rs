//! Observation: an observer trait with a method per event, and counters.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::core::{EndpointState, ErrorClass};
use crate::endpoint::EndpointAddr;

/// Receives the proxy's events. Every method has an empty default, so an
/// observer implements only what it needs, and [`NoObserver`] costs nothing.
/// Methods are called on the thread that runs the exchange: keep them short
/// (count, enqueue), never block.
///
/// ```
/// use std::sync::atomic::{AtomicU64, Ordering};
/// use proxy_net::{Observer, Finished};
///
/// #[derive(Default)]
/// struct Count(AtomicU64);
/// impl Observer for Count {
///     fn finished(&self, _exchange: &Finished) {
///         self.0.fetch_add(1, Ordering::Relaxed);
///     }
/// }
/// let _builder = proxy_net::ProxyBuilder::new().observer(Count::default());
/// ```
pub trait Observer: Send + Sync + 'static {
    /// A request matched route `route`, which goes to `pool`.
    fn route_matched(&self, route: usize, pool: &str) {
        let _ = (route, pool);
    }

    /// The balancer picked `endpoint` for attempt `attempt` (from 1).
    fn endpoint_picked(&self, pool: &str, endpoint: &EndpointAddr, attempt: u8) {
        let _ = (pool, endpoint, attempt);
    }

    /// A connection to `endpoint` is ready: `reused` from the pool, or new
    /// after `took`.
    fn connected(&self, endpoint: &EndpointAddr, took: Duration, reused: bool) {
        let _ = (endpoint, took, reused);
    }

    /// An attempt on `endpoint` failed with `class` and the exchange retries.
    fn retry(&self, endpoint: &EndpointAddr, class: ErrorClass, attempt: u8) {
        let _ = (endpoint, class, attempt);
    }

    /// The response head from `endpoint` arrived `waited` after the request
    /// was sent.
    fn response_head(&self, endpoint: &EndpointAddr, status: u16, waited: Duration) {
        let _ = (endpoint, status, waited);
    }

    /// An exchange ended (each one exactly once).
    fn finished(&self, exchange: &Finished) {
        let _ = exchange;
    }

    /// An endpoint's health changed.
    fn health_changed(
        &self,
        pool: &str,
        endpoint: &EndpointAddr,
        from: EndpointState,
        to: EndpointState,
    ) {
        let _ = (pool, endpoint, from, to);
    }

    /// The proxy refused a request before forwarding it (a route, a check, or
    /// a loop).
    fn refused(&self, class: ErrorClass) {
        let _ = class;
    }

    /// `endpoint` left `leftover` bytes after a complete response: it is out
    /// of step with the proxy, and its connection was closed.
    fn desync(&self, endpoint: &EndpointAddr, leftover: usize) {
        let _ = (endpoint, leftover);
    }
}

/// The observer that observes nothing.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoObserver;

impl Observer for NoObserver {}

/// What [`Observer::finished`] reports about one exchange.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finished {
    /// The route that matched.
    pub route: usize,
    /// The pool's name.
    pub pool: Box<str>,
    /// The endpoint of the last attempt, if one was picked.
    pub endpoint: Option<EndpointAddr>,
    /// The status the client got, if any.
    pub status: Option<u16>,
    /// What ended it, when it failed.
    pub failure: Option<ErrorClass>,
    /// Attempts made.
    pub attempts: u8,
    /// Request body bytes sent upstream.
    pub bytes_up: u64,
    /// Response body bytes relayed to the client.
    pub bytes_down: u64,
    /// From the start of the exchange to its end.
    pub took: Duration,
}

/// Proxy-wide counters, read without locks.
#[derive(Debug, Default)]
pub(crate) struct Counters {
    pub exchanges: AtomicU64,
    pub succeeded: AtomicU64,
    pub failed: AtomicU64,
    pub retries: AtomicU64,
    pub refused: AtomicU64,
    pub desyncs: AtomicU64,
}

impl Counters {
    pub(crate) fn add(counter: &AtomicU64) {
        let _ = counter.fetch_add(1, Ordering::Relaxed);
    }
}

/// A snapshot of the proxy's counters.
///
/// ```no_run
/// let proxy = proxy_net::ProxyBuilder::new()
///     .pool("app", proxy_net::Pool::new(["127.0.0.1:3000"]))
///     .build()?;
/// let stats = proxy.handle().stats();
/// println!("{} exchanges, {} retried", stats.exchanges, stats.retries);
/// # Ok::<(), proxy_net::ProxyError>(())
/// ```
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProxyStats {
    /// Exchanges started.
    pub exchanges: u64,
    /// Exchanges that relayed a whole response.
    pub succeeded: u64,
    /// Exchanges that failed (an error answer, or a response cut short).
    pub failed: u64,
    /// Retries granted.
    pub retries: u64,
    /// Requests refused before forwarding.
    pub refused: u64,
    /// Upstream responses followed by leftover bytes.
    pub desyncs: u64,
}

impl Counters {
    pub(crate) fn snapshot(&self) -> ProxyStats {
        let r = |c: &AtomicU64| c.load(Ordering::Relaxed);
        ProxyStats {
            exchanges: r(&self.exchanges),
            succeeded: r(&self.succeeded),
            failed: r(&self.failed),
            retries: r(&self.retries),
            refused: r(&self.refused),
            desyncs: r(&self.desyncs),
        }
    }
}

/// The parts of a builder that are not configuration: the observer and the
/// connector. Compared by identity.
#[derive(Clone, Default)]
pub(crate) struct Extras {
    pub observer: Option<std::sync::Arc<dyn Observer>>,
    pub connector: Option<std::sync::Arc<dyn crate::client::Connector>>,
}

impl std::fmt::Debug for Extras {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Extras")
            .field("observer", &self.observer.is_some())
            .field("connector", &self.connector.is_some())
            .finish()
    }
}

impl PartialEq for Extras {
    fn eq(&self, other: &Extras) -> bool {
        let same_observer = match (&self.observer, &other.observer) {
            (Some(a), Some(b)) => std::sync::Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        let same_connector = match (&self.connector, &other.connector) {
            (Some(a), Some(b)) => std::sync::Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        same_observer && same_connector
    }
}

impl Eq for Extras {}

impl crate::config::ProxyBuilder {
    /// Receives the proxy's events (route matched, endpoint picked, retries,
    /// health changes, each exchange's end). Default: none.
    pub fn observer(mut self, observer: impl Observer) -> crate::config::ProxyBuilder {
        self.extras_mut().observer = Some(std::sync::Arc::new(observer));
        self
    }

    /// How the proxy reaches endpoints. Default
    /// [`TcpConnector`](crate::client::TcpConnector).
    pub fn connector(
        mut self,
        connector: impl crate::client::Connector,
    ) -> crate::config::ProxyBuilder {
        self.extras_mut().connector = Some(std::sync::Arc::new(connector));
        self
    }
}
