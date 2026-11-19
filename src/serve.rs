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
use crate::client::runtime::Runtime;
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
            rt: Arc::clone(&self.rt),
        }
    }

    /// Serves `listen` with this proxy: a serve-net server with blocking
    /// workers, the body limit raised to `Limits::max_request_body`, and the
    /// proxy's service.
    ///
    /// # Errors
    ///
    /// `Serve` when the listener cannot be bound.
    pub fn serve<A: ToSocketAddrs>(self, listen: A) -> Result<Running, ProxyError> {
        let limits =
            serve_net::Limits::default().max_body(self.rt.config.limits().max_request_body_value());
        let server = Server::builder()
            .listener(ListenerSpec::tcp("proxy", listen))
            .workers(Workers::blocking(WORKERS))
            .limits(limits)
            .serve(self.service())
            .map_err(ProxyError::Serve)?;
        Ok(Running {
            server,
            proxy: self,
        })
    }
}

/// The proxy as a serve-net [`HttpService`]: routes and checks each request
/// in `head`, then forwards it, streaming the body up and the response down.
#[derive(Clone)]
pub struct ProxyService {
    rt: Arc<Runtime>,
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

thread_local! {
    static CURRENT: RefCell<Option<Current>> = const { RefCell::new(None) };
}

fn host_of<'a>(head: &HttpHead<'a>) -> &'a [u8] {
    head.header("host")
        .or_else(|| head.authority().map(str::as_bytes))
        .unwrap_or(b"")
}

impl ProxyService {
    fn refuse(&self, class: ErrorClass) -> HeadAction {
        Counters::add(&self.rt.counters.refused);
        self.rt.observer.refused(class);
        HeadAction::refuse(class.status().unwrap_or(400))
    }

    fn parts(&self, head: &HttpHead<'_>, pool: usize) -> RequestParts {
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
        let key = self
            .rt
            .config
            .pool_at(pool)
            .and_then(|p| match p.balance() {
                crate::Balance::ConsistentHash { key, .. } => {
                    let bytes: Option<Vec<u8>> = match key {
                        HashKey::ClientAddress => client.map(|c| c.ip().to_string().into_bytes()),
                        HashKey::Header(name) => head.header(name).map(<[u8]>::to_vec),
                        HashKey::Cookie(name) => cookie(head.header("cookie"), name),
                        HashKey::Path => Some(head.path().as_bytes().to_vec()),
                        HashKey::PathAndQuery => Some(target.as_bytes().to_vec()),
                    };
                    bytes.and_then(|b| self.rt.pools.get(pool).map(|r| r.balancer.hash(&b)))
                }
                _ => None,
            });
        RequestParts {
            method: head.method_str().into(),
            target: target.into_boxed_str(),
            host: host_of(head).into(),
            fields: head.headers().map(|(n, v)| (n.into(), v.into())).collect(),
            client,
            https: head.client().scheme() == Scheme::Https,
            body,
            key,
        }
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
        let config = &self.rt.config;
        let method = head.method_str().as_bytes();
        let route = match config
            .routes()
            .find(host_of(head), head.path().as_bytes(), method)
        {
            RouteMatch::Route { index, route } => (index, route),
            RouteMatch::UnknownHost => {
                return self
                    .refuse(ErrorClass::Refused(RefusalKind::UnknownHost))
                    .with_status(config.unknown_host().status());
            }
            _ => return self.refuse(ErrorClass::Refused(RefusalKind::NoRoute)),
        };
        if let Some(p) = config.via() {
            if detect_loop(head.header_all("via"), p) {
                return self.refuse(ErrorClass::Loop);
            }
        }
        let has_body = head.content_length().is_some_and(|n| n > 0)
            || head.header("transfer-encoding").is_some();
        if let Err(class) = check_request(method, head.path().as_bytes(), has_body, route.1) {
            return self.refuse(class);
        }
        if let Some(p) = config.pool_at(route.1.pool_index()) {
            self.rt.observer.route_matched(route.0, p.name());
        }
        env.set_user_word(route.0 as u64 + 1);
        HeadAction::ProceedWith(BodyPolicy::Stream)
    }

    fn call(&self, request: &HttpRequest<'_>, env: &Envelope, reply: &mut HttpReply) -> Outcome {
        let Some(route) = env.user_word().checked_sub(1) else {
            let _ = reply.text(500, "no route");
            return Outcome::Done;
        };
        let route = route as usize;
        let Some(pool) = self.rt.config.routes().get(route).map(|r| r.pool_index()) else {
            let _ = reply.text(500, "no route");
            return Outcome::Done;
        };
        CURRENT.with(|cell| {
            let mut slot = cell.borrow_mut();
            let fresh = !slot
                .as_ref()
                .is_some_and(|c| c.connection == env.connection() && c.seq == env.seq());
            if fresh {
                *slot = Some(Current {
                    connection: env.connection(),
                    seq: env.seq(),
                    forward: Forward::new(
                        Arc::clone(&self.rt),
                        route,
                        pool,
                        self.parts(request.head(), pool),
                    ),
                    relaying: false,
                    buf: Vec::new(),
                });
            }
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
                if !start.has_body {
                    let mut ignored = Vec::new();
                    let _ = current.forward.read_body(&mut ignored, 0);
                    *slot = None;
                    return Outcome::Done;
                }
                current.relaying = true;
            }
            let max = self.rt.config.limits().relay_buffer_value() as usize;
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
