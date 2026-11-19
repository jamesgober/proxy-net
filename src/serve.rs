//! The serve-net service (feature `serve`) and Tier 1.

use std::cell::RefCell;
use std::fmt;
use std::net::ToSocketAddrs;
use std::sync::Arc;
use std::time::Duration;
use std::vec::Vec;

use serve_net::{
    BodyPolicy, Envelope, HeadAction, HttpBody, HttpHead, HttpReply, HttpRequest, HttpService,
    ListenerSpec, Outcome, Scheme, Server, Workers,
};

use crate::client::forward::{Failure, Forward, RequestParts};
use crate::client::runtime::{Runtime, Shared};
use crate::config::ProxyBuilder;
use crate::core::{BodyFacts, ErrorClass, RefusalKind, RouteMatch, check_request, detect_loop};
use crate::observe::Counters;
use crate::pool::{HashKey, Pool};
use crate::proxy::ProxyError;
use crate::proxy::{Proxy, ProxyHandle};

/// The most reply fields serve-net 0.8 writes; a response with more is
/// answered 502 rather than sent incomplete.
const MAX_REPLY_FIELDS: usize = 64;

/// The blocking worker threads `Proxy::serve` and `proxy` start.
const WORKERS: u16 = 256;

/// Listens on `listen` and forwards every request to `upstreams`: one pool,
/// round robin, passive health, the default limits and timeouts, and both
/// styles of forwarding headers.
///
/// ```no_run
/// let running = proxy_net::proxy("127.0.0.1:8080", ["127.0.0.1:3000", "127.0.0.1:3001"])?;
/// running.wait();
/// # Ok::<(), proxy_net::ProxyError>(())
/// ```
///
/// # Errors
///
/// `Config` when an endpoint does not parse or there is none; `Serve` when
/// the listener cannot be bound.
pub fn proxy<A, I, S>(listen: A, upstreams: I) -> Result<Running, ProxyError>
where
    A: ToSocketAddrs,
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    ProxyBuilder::new()
        .pool("default", Pool::new(upstreams))
        .build()?
        .serve(listen)
}

/// A proxy serving a listener.
pub struct Running {
    server: serve_net::Running,
    proxy: Proxy,
}

impl fmt::Debug for Running {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Running")
            .field("local_addr", &self.server.local_addr())
            .finish_non_exhaustive()
    }
}

impl Running {
    /// The address the listener is bound to.
    pub fn local_addr(&self) -> Option<std::net::SocketAddr> {
        self.server.local_addr()
    }

    /// A handle to the proxy's health and counters.
    pub fn handle(&self) -> ProxyHandle {
        self.proxy.handle()
    }

    /// Stops accepting, lets the exchanges in flight finish within `within`,
    /// then stops.
    pub fn drain(self, within: Duration) -> serve_net::DrainReport {
        let Running { server, proxy } = self;
        let report = server.drain(within);
        drop(proxy);
        report
    }

    /// Runs until the server stops (a signal, or a drain from elsewhere).
    pub fn wait(self) -> serve_net::DrainReport {
        let Running { server, proxy } = self;
        let report = server.wait();
        drop(proxy);
        report
    }
}

impl Proxy {
    /// The serve-net service of this proxy, to mount in a serve-net server
    /// of your own. It needs blocking workers (`Workers::blocking`) and a
    /// body limit at least `Limits::max_request_body`; see [`Proxy::serve`].
    pub fn service(&self) -> ProxyService {
        ProxyService {
            shared: Arc::clone(&self.shared),
            #[cfg(feature = "ws")]
            bridges: Arc::default(),
        }
    }

    /// Serves `listen` with this proxy: a serve-net server with blocking
    /// workers, the body limit raised to `Limits::max_request_body`, and the
    /// proxy's service. The same as `serve_with(listen, Inbound::new())`.
    ///
    /// # Errors
    ///
    /// `Serve` when the listener cannot be bound.
    pub fn serve<A: ToSocketAddrs>(self, listen: A) -> Result<Running, ProxyError> {
        self.serve_with(listen, Inbound::new())
    }

    /// As [`serve`](Self::serve), with the client side set by `inbound`.
    ///
    /// ```no_run
    /// # fn main() -> Result<(), proxy_net::ProxyError> {
    /// // Behind a load balancer: every client arrives from its few addresses.
    /// let proxy = proxy_net::ProxyBuilder::new()
    ///     .pool("app", proxy_net::Pool::new(["10.0.0.1:80"]))
    ///     .build()?;
    /// let running = proxy.serve_with("0.0.0.0:8080", proxy_net::Inbound::new().max_per_source(224))?;
    /// # Ok(()) }
    /// ```
    ///
    /// # Errors
    ///
    /// `Inbound` for a zero setting; `Serve` when the listener cannot be
    /// bound or serve-net refuses a setting (a per-source cap above the
    /// connection cap).
    pub fn serve_with<A: ToSocketAddrs>(
        self,
        listen: A,
        inbound: Inbound,
    ) -> Result<Running, ProxyError> {
        let mut limits = serve_net::Limits::default().max_body(
            self.shared
                .runtime()
                .config
                .limits()
                .max_request_body_value(),
        );
        if let Some(n) = inbound.max_connections {
            limits = limits.max_connections(n);
        }
        let transport = serve_net::net_io::Limits::builder()
            .max_per_source(
                inbound.max_per_source,
                serve_net::net_io::PrefixPolicy::DEFAULT,
            )
            .build()
            .map_err(|_| ProxyError::Inbound("max_per_source must be 1 or more"))?;
        if inbound.workers == 0 {
            return Err(ProxyError::Inbound("workers must be 1 or more"));
        }
        let server = Server::builder()
            .listener(ListenerSpec::tcp("proxy", listen))
            .workers(Workers::blocking(inbound.workers))
            .limits(limits)
            .transport_limits(transport)
            .serve(self.service())
            .map_err(ProxyError::Serve)?;
        // The address PROXY headers and `by=` name (serve-net does not hand
        // the local address to services).
        if let Some(addr) = server.local_addr() {
            self.shared.set_listener(addr);
        }
        Ok(Running {
            server,
            proxy: self,
        })
    }
}

/// How [`Proxy::serve_with`] accepts clients.
///
/// Each open client connection holds one blocking worker, so `workers` is
/// the most clients served at once. `max_per_source` caps how many of those
/// one address (an IPv6 /64) may hold, so one client cannot take every
/// worker. Behind a load balancer or another proxy, clients arrive from a
/// few addresses: raise it to what each of those addresses carries, or the
/// connections above it are closed as they arrive.
///
/// ```
/// let inbound = proxy_net::Inbound::new().workers(512).max_per_source(64);
/// assert_eq!(inbound.workers_value(), 512);
/// assert_eq!(proxy_net::Inbound::new().max_per_source_value(), 16);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Inbound {
    workers: u16,
    max_per_source: u32,
    max_connections: Option<u32>,
}

impl Default for Inbound {
    fn default() -> Inbound {
        Inbound::new()
    }
}

impl Inbound {
    /// 256 workers, 16 connections per source, and serve-net's own cap on
    /// connections (the workers less their headroom).
    #[must_use]
    pub const fn new() -> Inbound {
        Inbound {
            workers: WORKERS,
            max_per_source: 16,
            max_connections: None,
        }
    }

    /// Blocking workers: the most client connections open at once.
    #[must_use]
    pub const fn workers(mut self, n: u16) -> Inbound {
        self.workers = n;
        self
    }

    /// The most connections open at once from one address (IPv4) or one
    /// /64 (IPv6). At most the connection cap: `max_connections`, or the
    /// workers less serve-net's headroom (224 of 256); above it the server
    /// refuses to start (`Serve`, naming `max_per_source`).
    #[must_use]
    pub const fn max_per_source(mut self, n: u32) -> Inbound {
        self.max_per_source = n;
        self
    }

    /// The most client connections open at once, below what the workers
    /// allow.
    #[must_use]
    pub const fn max_connections(mut self, n: u32) -> Inbound {
        self.max_connections = Some(n);
        self
    }

    /// The workers set.
    #[must_use]
    pub const fn workers_value(&self) -> u16 {
        self.workers
    }

    /// The per-source cap set.
    #[must_use]
    pub const fn max_per_source_value(&self) -> u32 {
        self.max_per_source
    }

    /// The connection cap set, if any.
    #[must_use]
    pub const fn max_connections_value(&self) -> Option<u32> {
        self.max_connections
    }
}

/// The proxy as a serve-net [`HttpService`]: routes and checks each request
/// in `head`, then forwards it, streaming the body up and the response down.
#[derive(Clone)]
pub struct ProxyService {
    shared: Arc<Shared>,
    #[cfg(feature = "ws")]
    bridges: Arc<crate::ws::Bridges>,
}

impl fmt::Debug for ProxyService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProxyService").finish_non_exhaustive()
    }
}

/// The exchange of the request a blocking worker is serving.
struct Current {
    connection: serve_net::ConnectionId,
    seq: serve_net::RequestSeq,
    forward: Forward,
    relaying: bool,
    buf: Vec<u8>,
}

impl Drop for Current {
    fn drop(&mut self) {
        crate::client::spare::give_bytes(core::mem::take(&mut self.buf));
    }
}

thread_local! {
    static CURRENT: RefCell<Option<Current>> = const { RefCell::new(None) };
}

fn host_of<'a>(head: &HttpHead<'a>) -> &'a [u8] {
    head.header("host")
        .or_else(|| head.authority().map(str::as_bytes))
        .unwrap_or(b"")
}

impl ProxyService {
    fn refuse(&self, rt: &Runtime, class: ErrorClass) -> HeadAction {
        Counters::add(&rt.counters.refused);
        rt.observer.refused(class);
        HeadAction::refuse(class.status().unwrap_or(400))
    }

    fn parts(&self, rt: &Runtime, head: &HttpHead<'_>, pool: usize) -> RequestParts {
        let target = match head.query() {
            Some(q) => format!("{}?{}", head.path(), q),
            None => head.path().to_string(),
        };
        let body = match head.content_length() {
            Some(0) => BodyFacts::None,
            Some(n) => BodyFacts::Length(n),
            None if head.header("transfer-encoding").is_some() => BodyFacts::Chunked,
            None => BodyFacts::None,
        };
        let client = head.client().socket_addr();
        let key = rt.config.pool_at(pool).and_then(|p| match p.balance() {
            crate::Balance::ConsistentHash { key, .. } => {
                let bytes: Option<Vec<u8>> = match key {
                    HashKey::ClientAddress => client.map(|c| c.ip().to_string().into_bytes()),
                    HashKey::Header(name) => head.header(name).map(<[u8]>::to_vec),
                    HashKey::Cookie(name) => cookie(head.header("cookie"), name),
                    HashKey::Path => Some(head.path().as_bytes().to_vec()),
                    HashKey::PathAndQuery => Some(target.as_bytes().to_vec()),
                };
                bytes.and_then(|b| rt.pools.get(pool).map(|r| r.balancer.hash(&b)))
            }
            _ => None,
        });
        let mut fields = crate::client::spare::list();
        for (n, v) in head.headers() {
            fields.push(n, v);
        }
        RequestParts {
            method: head.method_str().into(),
            target: target.into_boxed_str(),
            host: host_of(head).into(),
            fields,
            client,
            https: head.client().scheme() == Scheme::Https,
            body,
            key,
            peer: head.client().peer(),
            hops: head.client().hops(),
            request_id: rt.config.request_id().map(|_| {
                self.shared
                    .next_request_id()
                    .into_bytes()
                    .into_boxed_slice()
            }),
        }
    }

    /// Opens the upstream WebSocket and asks serve-net for the client's.
    #[cfg(feature = "ws")]
    fn bridge(
        &self,
        rt: &Runtime,
        route: usize,
        pool: usize,
        head: &HttpHead<'_>,
        env: &mut Envelope,
    ) -> HeadAction {
        let wants_websocket = head.header("upgrade").is_some_and(|u| {
            u.split(|b| *b == b',')
                .any(|p| p.trim_ascii().eq_ignore_ascii_case(b"websocket"))
        });
        if !wants_websocket || head.method_str() != "GET" {
            return self.refuse(rt, ErrorClass::Refused(RefusalKind::Method));
        }
        let target = match head.query() {
            Some(q) => format!("{}?{}", head.path(), q),
            None => head.path().to_string(),
        };
        let up = crate::ws::Upgrade {
            fields: head.headers().collect(),
            method: b"GET",
            target: target.as_bytes(),
            host: host_of(head),
            client: head.client().socket_addr(),
            https: head.client().scheme() == Scheme::Https,
            peer: head.client().peer(),
            hops: head.client().hops(),
        };
        match crate::ws::open(&self.bridges, rt, route, pool, &up, env.connection()) {
            Ok(protocol) => {
                let service = Arc::new(crate::ws::BridgeService {
                    bridges: Arc::clone(&self.bridges),
                });
                let mut t = serve_net::WebSocketTarget::shared(service);
                if let Some(p) = protocol {
                    t = t.subprotocol(p);
                }
                HeadAction::Upgrade(serve_net::UpgradeTarget::WebSocket(t))
            }
            Err(class) => self.refuse(rt, class),
        }
    }

    /// The route and pool of a new request in `call`: the ones `head` chose,
    /// or, when the configuration changed since, the ones the current
    /// configuration gives, after the same checks `head` runs.
    fn resolve(
        &self,
        rt: &Runtime,
        head: &HttpHead<'_>,
        word: u64,
    ) -> Result<(usize, usize), Failure> {
        let refuse = |class: ErrorClass, status: Option<u16>| {
            Counters::add(&rt.counters.refused);
            rt.observer.refused(class);
            Failure {
                class,
                status: status.or(class.status()).or(Some(400)),
            }
        };
        let no_route = ErrorClass::Refused(RefusalKind::NoRoute);
        let (generation, index) = ((word >> 32) as u32, (word & 0xffff_ffff) as usize);
        let route = if generation == rt.generation {
            index.checked_sub(1).ok_or_else(|| refuse(no_route, None))?
        } else {
            let config = &rt.config;
            let method = head.method_str().as_bytes();
            let (index, route) =
                match config
                    .routes()
                    .find(host_of(head), head.path().as_bytes(), method)
                {
                    RouteMatch::Route { index, route } => (index, route),
                    RouteMatch::UnknownHost => {
                        return Err(refuse(
                            ErrorClass::Refused(RefusalKind::UnknownHost),
                            Some(config.unknown_host().status()),
                        ));
                    }
                    _ => return Err(refuse(no_route, None)),
                };
            if let Some(p) = config.via() {
                if detect_loop(head.header_all("via"), p) {
                    return Err(refuse(ErrorClass::Loop, None));
                }
            }
            check_request(method, head.path().as_bytes(), has_body(head), route)
                .map_err(|class| refuse(class, None))?;
            index
        };
        let pool = rt
            .config
            .routes()
            .get(route)
            .map(|r| r.pool_index())
            .ok_or_else(|| refuse(no_route, None))?;
        Ok((route, pool))
    }

    fn answer(failure: Failure, reply: &mut HttpReply) -> Outcome {
        match failure.status {
            Some(status) => {
                if status == 503 {
                    let _ = reply.retry_after(Duration::from_secs(1));
                }
                let _ = reply.text(status, &failure.class.to_string());
                Outcome::Done
            }
            None => Outcome::Close,
        }
    }
}

/// True when the request says it has a body.
fn has_body(head: &HttpHead<'_>) -> bool {
    head.content_length().is_some_and(|n| n > 0) || head.header("transfer-encoding").is_some()
}

/// The value of cookie `name` in a `Cookie` field.
fn cookie(field: Option<&[u8]>, name: &str) -> Option<Vec<u8>> {
    let field = field?;
    field.split(|b| *b == b';').find_map(|pair| {
        let pair = pair.strip_prefix(b" ").unwrap_or(pair);
        let (n, v) = pair.split_at(pair.iter().position(|b| *b == b'=')?);
        (n == name.as_bytes()).then(|| v[1..].to_vec())
    })
}

impl HttpService for ProxyService {
    fn head(&self, head: &HttpHead<'_>, env: &mut Envelope) -> HeadAction {
        let rt = self.shared.runtime();
        let config = &rt.config;
        let method = head.method_str().as_bytes();
        let route = match config
            .routes()
            .find(host_of(head), head.path().as_bytes(), method)
        {
            RouteMatch::Route { index, route } => (index, route),
            RouteMatch::UnknownHost => {
                return self
                    .refuse(&rt, ErrorClass::Refused(RefusalKind::UnknownHost))
                    .with_status(config.unknown_host().status());
            }
            _ => return self.refuse(&rt, ErrorClass::Refused(RefusalKind::NoRoute)),
        };
        if let Some(p) = config.via() {
            if detect_loop(head.header_all("via"), p) {
                return self.refuse(&rt, ErrorClass::Loop);
            }
        }
        if let Err(class) = check_request(method, head.path().as_bytes(), has_body(head), route.1) {
            return self.refuse(&rt, class);
        }
        #[cfg(feature = "ws")]
        if head.is_upgrade() && route.1.websocket() {
            return self.bridge(&rt, route.0, route.1.pool_index(), head, env);
        }
        if let Some(p) = config.pool_at(route.1.pool_index()) {
            rt.observer.route_matched(route.0, p.name());
        }
        env.set_user_word((u64::from(rt.generation) << 32) | (route.0 as u64 + 1));
        HeadAction::ProceedWith(BodyPolicy::Stream)
    }

    fn call(&self, request: &HttpRequest<'_>, env: &Envelope, reply: &mut HttpReply) -> Outcome {
        let rt = self.shared.runtime();
        let fresh = CURRENT.with(|cell| {
            !cell
                .borrow()
                .as_ref()
                .is_some_and(|c| c.connection == env.connection() && c.seq == env.seq())
        });
        if fresh {
            let (route, pool) = match self.resolve(&rt, request.head(), env.user_word()) {
                Ok(found) => found,
                Err(f) => return ProxyService::answer(f, reply),
            };
            let forward = Forward::new(
                Arc::clone(&rt),
                route,
                pool,
                self.parts(&rt, request.head(), pool),
            );
            CURRENT.with(|cell| {
                *cell.borrow_mut() = Some(Current {
                    connection: env.connection(),
                    seq: env.seq(),
                    forward,
                    relaying: false,
                    buf: crate::client::spare::bytes(),
                });
            });
        }
        CURRENT.with(|cell| {
            let mut slot = cell.borrow_mut();
            let Some(current) = slot.as_mut() else {
                return Outcome::Close;
            };
            if !current.relaying {
                let (bytes, last): (&[u8], bool) = match request.body() {
                    HttpBody::Chunk { bytes, last } => (bytes, last),
                    HttpBody::Full(b) => (b, true),
                    _ => (&[], true),
                };
                if let Err(f) = current.forward.push_body(bytes, last) {
                    *slot = None;
                    return ProxyService::answer(f, reply);
                }
                if !last {
                    return Outcome::More;
                }
                let start = match current.forward.response() {
                    Ok(start) => start,
                    Err(f) => {
                        *slot = None;
                        return ProxyService::answer(f, reply);
                    }
                };
                if start.fields.len() > MAX_REPLY_FIELDS {
                    *slot = None;
                    let _ = reply.text(502, "the upstream's response head is too large");
                    return Outcome::Done;
                }
                let _ = reply.status(start.status);
                for (name, value) in start.fields.iter() {
                    let _ = reply.header(name, value);
                }
                crate::client::spare::give_list(start.fields);
                if !start.has_body {
                    let mut ignored = Vec::new();
                    let _ = current.forward.read_body(&mut ignored, 0);
                    *slot = None;
                    return Outcome::Done;
                }
                current.relaying = true;
            }
            let max = rt.config.limits().relay_buffer_value() as usize;
            current.buf.clear();
            match current.forward.read_body(&mut current.buf, max) {
                Ok(ended) => {
                    let _ = reply.stream().send(&current.buf);
                    if ended {
                        *slot = None;
                        Outcome::Done
                    } else {
                        Outcome::More
                    }
                }
                Err(_) => {
                    *slot = None;
                    Outcome::Close
                }
            }
        })
    }

    fn done(&self, env: &Envelope, _finished: &serve_net::Finished) {
        CURRENT.with(|cell| {
            let mut slot = cell.borrow_mut();
            if slot
                .as_ref()
                .is_some_and(|c| c.connection == env.connection() && c.seq == env.seq())
            {
                *slot = None;
            }
        });
    }
}

trait WithStatus {
    fn with_status(self, status: u16) -> HeadAction;
}

impl WithStatus for HeadAction {
    fn with_status(self, status: u16) -> HeadAction {
        match self {
            HeadAction::Refuse { retry_after, .. } => HeadAction::Refuse {
                status,
                retry_after,
            },
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cookie_lookup() {
        assert_eq!(
            cookie(Some(b"a=1; session=xyz; b=2"), "session"),
            Some(b"xyz".to_vec())
        );
        assert_eq!(cookie(Some(b"a=1"), "session"), None);
        assert_eq!(cookie(Some(b"session"), "session"), None);
        assert_eq!(cookie(None, "session"), None);
    }
}
