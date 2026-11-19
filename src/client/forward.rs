//! The blocking forwarder: drives one `core::Exchange` against real
//! connections, on the thread of the client connection.

use std::boxed::Box;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use std::vec::Vec;

use http_net::{ClientConn, ClientEvent, Request};

use super::pool::{Taken, UpstreamConn};
use super::runtime::Runtime;
use crate::core::{
    Action, BodyFacts, EndpointId, ErrorClass, Event, Exchange, FieldList, ForwardFacts, LimitKind,
    Outcome, RefusalKind, Tick, is_idempotent, request_fields, response_fields,
};
use crate::observe::{Counters, Finished};

/// A request as the forwarder needs it, owned (the inbound engine's view
/// lives only for one call).
#[derive(Clone, Debug, Default)]
pub struct RequestParts {
    /// The method.
    pub method: Box<str>,
    /// The target as received (path and query).
    pub target: Box<str>,
    /// The `Host` value.
    pub host: Box<[u8]>,
    /// The fields as received, in order.
    pub fields: Vec<(Box<str>, Box<[u8]>)>,
    /// The client address the inbound engine resolved.
    pub client: Option<SocketAddr>,
    /// The client reached the proxy over TLS.
    pub https: bool,
    /// How the request body is framed.
    pub body: BodyFacts,
    /// The balancer key for consistent hashing, if the pool hashes.
    pub key: Option<u64>,
    /// The socket peer (the trusted proxy in front, when there is one).
    pub peer: Option<SocketAddr>,
    /// Proxies between the client and this one, as the inbound engine walked
    /// them (0 for a direct client).
    pub hops: u8,
    /// The request id to write when the request has none.
    pub request_id: Option<Box<[u8]>>,
}

/// The PROXY protocol header for one upstream connection. A client or
/// destination that is unknown (or a mix of families that cannot be shown
/// as one) gives `UNKNOWN` (v1) or `LOCAL` (v2), as a health check does.
pub(crate) fn prelude(
    mode: crate::ProxyProtocol,
    client: Option<SocketAddr>,
    destination: Option<SocketAddr>,
    authority: &[u8],
) -> Vec<u8> {
    use net_io::{ProxyCommand, Tlv};
    let pair = match (client, destination) {
        (Some(c), Some(d)) if c.is_ipv4() == d.is_ipv4() => Some((c, d)),
        (Some(c), Some(d)) => {
            let six = |a: SocketAddr| match a {
                SocketAddr::V4(v4) => {
                    SocketAddr::new(std::net::IpAddr::V6(v4.ip().to_ipv6_mapped()), v4.port())
                }
                v6 => v6,
            };
            Some((six(c), six(d)))
        }
        _ => None,
    };
    let mut out = vec![0u8; 512];
    let n = match mode {
        crate::ProxyProtocol::V1 => match pair {
            Some((c, d)) => net_io::encode_v1(c, d, &mut out).ok(),
            None => {
                return b"PROXY UNKNOWN\r\n".to_vec();
            }
        },
        crate::ProxyProtocol::V2 {
            authority: with_authority,
            crc,
        } => {
            let tlv = [Tlv {
                kind: 0x02,
                value: authority,
            }];
            let tlvs: &[Tlv<'_>] =
                if with_authority && !authority.is_empty() && authority.len() <= 255 {
                    &tlv
                } else {
                    &[]
                };
            match pair {
                Some((c, d)) => {
                    net_io::encode_v2(ProxyCommand::Proxy, Some(c), Some(d), tlvs, crc, &mut out)
                        .ok()
                }
                None => net_io::encode_v2(ProxyCommand::Local, None, None, &[], crc, &mut out).ok(),
            }
        }
        _ => Some(0),
    };
    out.truncate(n.unwrap_or(0));
    out
}

/// The start of the response the client will get.
#[derive(Debug)]
pub struct ResponseStart {
    /// The status.
    pub status: u16,
    /// The fields to send, already rewritten.
    pub fields: FieldList,
    /// False for responses that carry no body (`HEAD`, 204, 304).
    pub has_body: bool,
}

/// Why an exchange failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Failure {
    /// What ended it.
    pub class: ErrorClass,
    /// The status to answer with, or `None` when the client connection must
    /// close (a response was already under way, or the client left).
    pub status: Option<u16>,
}

impl Failure {
    fn new(class: ErrorClass) -> Failure {
        Failure {
            class,
            status: class.status(),
        }
    }
}

/// One request being forwarded.
pub struct Forward {
    rt: Arc<Runtime>,
    route: usize,
    pool: usize,
    parts: RequestParts,
    idempotent: bool,
    exchange: Option<Exchange>,
    buffered: Vec<u8>,
    body_complete: bool,
    head_sent: bool,
    conn: Option<UpstreamConn>,
    endpoint: Option<EndpointId>,
    balanced: Option<EndpointId>,
    deadline: Instant,
    out: Vec<u8>,
    started: Instant,
    sent_at: Instant,
    head: Option<ResponseStart>,
    retry_held: bool,
    bytes_up: u64,
    bytes_down: u64,
    status: Option<u16>,
    failure: Option<ErrorClass>,
    over: bool,
}

impl std::fmt::Debug for Forward {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Forward")
            .field("route", &self.route)
            .field("pool", &self.pool)
            .field("endpoint", &self.endpoint)
            .field("over", &self.over)
            .finish_non_exhaustive()
    }
}

impl Forward {
    pub(crate) fn new(rt: Arc<Runtime>, route: usize, pool: usize, parts: RequestParts) -> Forward {
        if let Some(p) = rt.pools.get(pool) {
            p.budget.exchange_started();
        }
        Counters::add(&rt.counters.exchanges);
        let now = Instant::now();
        Forward {
            idempotent: is_idempotent(parts.method.as_bytes()),
            rt,
            route,
            pool,
            parts,
            exchange: None,
            buffered: Vec::new(),
            body_complete: false,
            head_sent: false,
            conn: None,
            endpoint: None,
            balanced: None,
            deadline: now,
            out: Vec::new(),
            started: now,
            sent_at: now,
            head: None,
            retry_held: false,
            bytes_up: 0,
            bytes_down: 0,
            status: None,
            failure: None,
            over: false,
        }
    }

    fn fail(&mut self, f: Failure) -> Failure {
        if !self.over {
            self.over = true;
            self.failure = Some(f.class);
            self.status = f.status;
            self.drop_conn();
        }
        f
    }

    /// Takes the next piece of the request body (`last` on the final one).
    /// The first `Limits::request_buffer` bytes are held before any upstream
    /// connection is taken, so a slow client does not hold one for a small
    /// body; past that, the request goes upstream and the rest streams.
    ///
    /// # Errors
    ///
    /// `Limit(RequestBody)` past `Limits::max_request_body`, or a failure of
    /// the attempt that started the stream.
    pub fn push_body(&mut self, bytes: &[u8], last: bool) -> Result<(), Failure> {
        if self.over {
            return Err(Failure::new(self.failure.unwrap_or(ErrorClass::ClientGone)));
        }
        let limits = self.rt.config.limits();
        let total = self.bytes_up + self.buffered.len() as u64 + bytes.len() as u64;
        if total > limits.max_request_body_value() {
            let f = Failure {
                class: ErrorClass::Limit(LimitKind::RequestBody),
                status: if self.head_sent { None } else { Some(413) },
            };
            return Err(self.fail(f));
        }
        if !self.head_sent {
            self.buffered.extend_from_slice(bytes);
            if last {
                self.body_complete = true;
                return Ok(());
            }
            if self.buffered.len() as u64 > u64::from(limits.request_buffer_value()) {
                // Too large to hold: start the one attempt that can stream.
                self.run(false)?;
            }
            return Ok(());
        }
        // Streaming after the head went out.
        let Some(conn) = self.conn.as_mut() else {
            return Err(self.fail(Failure::new(ErrorClass::UpstreamReset)));
        };
        self.out.clear();
        let mut sent = conn.http.send_body(bytes, &mut self.out).is_ok();
        if sent && last {
            sent = conn.http.end_body(&mut self.out).is_ok();
        }
        if !sent {
            return Err(self.fail(Failure::new(ErrorClass::Refused(RefusalKind::InvalidField))));
        }
        let deadline = self.deadline;
        let result = conn.io.send(&self.out, deadline);
        self.bytes_up += bytes.len() as u64;
        let now = self.rt.tick();
        let event = match result {
            Ok(()) if last => Event::Sent,
            Ok(()) => Event::Progress,
            Err(ErrorClass::Timeout(_)) => Event::Tick,
            Err(c) => Event::UpstreamFailed(c),
        };
        let at = now.max(self.tick_of(deadline));
        let action = match self.exchange.as_mut() {
            Some(ex) => ex.on(event, at),
            None => Action::Nothing,
        };
        match action {
            Action::Send { deadline } | Action::Wait { deadline } => {
                self.deadline = self.rt.instant(deadline);
                if last {
                    self.body_complete = true;
                    self.sent_at = Instant::now();
                }
                Ok(())
            }
            Action::Fail { class, status } => Err(self.fail(Failure { class, status })),
            _ => Ok(()),
        }
    }

    fn tick_of(&self, at: Instant) -> Tick {
        self.rt.tick_at(at)
    }

    /// The response head, once the request body is complete: runs attempts
    /// (retries included) until one gets a response head, or the exchange
    /// fails.
    ///
    /// # Errors
    ///
    /// The exchange's failure, with the status to answer.
    pub fn response(&mut self) -> Result<ResponseStart, Failure> {
        if self.over {
            return Err(Failure::new(self.failure.unwrap_or(ErrorClass::ClientGone)));
        }
        if let Some(head) = self.head.take() {
            return Ok(head);
        }
        if self.head_sent {
            // Streaming: the request went out in full; wait for the head.
            let action = Action::Wait {
                deadline: self.tick_of(self.deadline),
            };
            self.drive(action)?;
        } else {
            self.run(true)?;
        }
        match self.head.take() {
            Some(head) => Ok(head),
            None => Err(self.fail(Failure::new(ErrorClass::UpstreamProtocol))),
        }
    }

    /// Starts the exchange: `replayable` when the whole body is held.
    fn run(&mut self, replayable: bool) -> Result<(), Failure> {
        let Some(pool) = self.rt.config.pool_at(self.pool) else {
            return Err(self.fail(Failure::new(ErrorClass::NoEndpoint)));
        };
        let now = self.rt.tick();
        let mut ex = Exchange::new(self.parts.method.as_bytes(), replayable, pool, now);
        let first = ex.on(Event::Start, now);
        self.exchange = Some(ex);
        self.drive(first)
    }

    fn event(&mut self, event: Event) -> Action {
        let now = self.rt.tick();
        match self.exchange.as_mut() {
            Some(ex) => ex.on(event, now),
            None => Action::Nothing,
        }
    }

    /// Follows the exchange's actions until the response head arrived, the
    /// request is streaming, or it failed.
    fn drive(&mut self, mut action: Action) -> Result<(), Failure> {
        let rt = Arc::clone(&self.rt);
        loop {
            action = match action {
                Action::Pick { attempt } => {
                    let tried: Vec<EndpointId> = self
                        .exchange
                        .as_ref()
                        .map(|e| e.tried().to_vec())
                        .unwrap_or_default();
                    match rt
                        .pools
                        .get(self.pool)
                        .and_then(|p| p.balancer.pick(self.parts.key, &tried))
                    {
                        Some(id) => {
                            if let (Some(cfg), Some(e)) = (
                                rt.config.pool_at(self.pool),
                                rt.config
                                    .pool_at(self.pool)
                                    .and_then(|p| p.endpoints().get(id.index())),
                            ) {
                                rt.observer.endpoint_picked(cfg.name(), e.addr(), attempt);
                            }
                            self.event(Event::Picked(id))
                        }
                        None => self.event(Event::NoEndpoint),
                    }
                }
                Action::Backoff { until, .. } => {
                    let wait = rt.instant(until).saturating_duration_since(Instant::now());
                    std::thread::sleep(wait);
                    self.event(Event::Tick)
                }
                Action::Connect { endpoint, deadline } => {
                    self.endpoint = Some(endpoint);
                    self.deadline = rt.instant(deadline);
                    match self.connect(endpoint) {
                        Ok(()) => self.event(Event::Connected),
                        Err(c) => self.event(Event::ConnectFailed(c)),
                    }
                }
                Action::Send { deadline } => {
                    self.deadline = rt.instant(deadline);
                    match self.send_head() {
                        Ok(true) => {
                            self.sent_at = Instant::now();
                            self.event(Event::Sent)
                        }
                        Ok(false) => return Ok(()),
                        Err(Failure {
                            class: ErrorClass::Timeout(_),
                            ..
                        }) => self.timed_out(),
                        Err(f)
                            if f.class == ErrorClass::Refused(RefusalKind::InvalidField)
                                || f.class == ErrorClass::Loop =>
                        {
                            return Err(self.fail(f));
                        }
                        Err(f) => self.event(Event::UpstreamFailed(f.class)),
                    }
                }
                Action::Wait { deadline } => {
                    self.deadline = rt.instant(deadline);
                    match self.read_head() {
                        Ok(status) => {
                            self.record_success();
                            if let (Some(id), Some(cfg)) =
                                (self.endpoint, rt.config.pool_at(self.pool))
                            {
                                if let Some(e) = cfg.endpoints().get(id.index()) {
                                    rt.observer.response_head(
                                        e.addr(),
                                        status,
                                        self.sent_at.elapsed(),
                                    );
                                }
                            }
                            self.event(Event::ResponseHead(status))
                        }
                        Err(ErrorClass::Timeout(_)) => self.timed_out(),
                        Err(c) => self.event(Event::UpstreamFailed(c)),
                    }
                }
                Action::Relay { deadline } => {
                    self.deadline = rt.instant(deadline);
                    return Ok(());
                }
                Action::Retry { class } => {
                    self.head = None;
                    self.record_failure(class);
                    self.drop_conn();
                    if self.retry_held {
                        if let Some(p) = rt.pools.get(self.pool) {
                            p.budget.retry_finished();
                        }
                        self.retry_held = false;
                    }
                    let granted = rt
                        .pools
                        .get(self.pool)
                        .is_some_and(|p| p.budget.try_retry());
                    if granted {
                        self.retry_held = true;
                        Counters::add(&rt.counters.retries);
                        if let (Some(id), Some(cfg)) = (self.endpoint, rt.config.pool_at(self.pool))
                        {
                            if let Some(e) = cfg.endpoints().get(id.index()) {
                                let attempt = self.exchange.as_ref().map_or(1, Exchange::attempt);
                                rt.observer.retry(e.addr(), class, attempt);
                            }
                        }
                        self.event(Event::RetryGranted)
                    } else {
                        self.event(Event::RetryDenied)
                    }
                }
                Action::Fail { class, status } => {
                    self.record_failure(class);
                    return Err(self.fail(Failure { class, status }));
                }
                _ => return Err(self.fail(Failure::new(ErrorClass::UpstreamProtocol))),
            };
        }
    }

    /// Feeds the passing of the current deadline.
    fn timed_out(&mut self) -> Action {
        let at = self.tick_of(self.deadline).max(self.rt.tick());
        match self.exchange.as_mut() {
            Some(ex) => ex.on(Event::Tick, at),
            None => Action::Nothing,
        }
    }

    fn connect(&mut self, id: EndpointId) -> Result<(), ErrorClass> {
        let rt = Arc::clone(&self.rt);
        let (Some(cfg), Some(conns)) = (rt.config.pool_at(self.pool), rt.conns(self.pool, id))
        else {
            return Err(ErrorClass::NoEndpoint);
        };
        let Some(endpoint) = cfg.endpoints().get(id.index()) else {
            return Err(ErrorClass::NoEndpoint);
        };
        if let Some(p) = rt.pools.get(self.pool) {
            p.balancer.started(id);
            self.balanced = Some(id);
        }
        let begun = Instant::now();
        match conns.take(cfg.limits(), self.idempotent, self.deadline)? {
            Taken::Pooled(conn) => {
                rt.observer
                    .connected(endpoint.addr(), begun.elapsed(), true);
                self.conn = Some(*conn);
                Ok(())
            }
            Taken::Dial => match rt.pools[self.pool].connector.connect(
                endpoint.addr(),
                &prelude(
                    cfg.proxy_protocol(),
                    self.parts.client,
                    rt.listener,
                    &self.parts.host,
                ),
                self.deadline,
            ) {
                Ok(io) => {
                    let l = rt.config.limits();
                    let limits = http_net::Limits::default()
                        .with_max_header_bytes(l.max_response_head_value() as usize)
                        .with_max_headers(usize::from(l.max_response_fields_value()))
                        .with_max_body(l.max_response_body_value().unwrap_or(u64::MAX));
                    rt.observer
                        .connected(endpoint.addr(), begun.elapsed(), false);
                    self.conn = Some(UpstreamConn {
                        io,
                        http: ClientConn::with_limits(limits),
                        exchanges: 0,
                        reused: false,
                    });
                    Ok(())
                }
                Err(c) => {
                    conns.dial_failed();
                    Err(c)
                }
            },
        }
    }

    /// Writes the request head and the held body; `Ok(true)` when the whole
    /// request went out, `Ok(false)` when the rest of the body streams.
    fn send_head(&mut self) -> Result<bool, Failure> {
        let rt = Arc::clone(&self.rt);
        let config = &rt.config;
        let (Some(route), Some(id)) = (config.routes().get(self.route), self.endpoint) else {
            return Err(Failure::new(ErrorClass::NoEndpoint));
        };
        let Some(endpoint) = config
            .pool_at(self.pool)
            .and_then(|p| p.endpoints().get(id.index()))
        else {
            return Err(Failure::new(ErrorClass::NoEndpoint));
        };
        let fields: Vec<(&str, &[u8])> = self
            .parts
            .fields
            .iter()
            .map(|(n, v)| (&**n, &**v))
            .collect();
        let facts = ForwardFacts::new(
            self.parts.method.as_bytes(),
            self.parts.target.as_bytes(),
            &self.parts.host,
            endpoint,
        )
        .client(self.parts.client)
        .https(self.parts.https)
        .listener(rt.listener)
        .chain(self.parts.peer, self.parts.hops)
        .request_id(self.parts.request_id.as_deref())
        .upstream_tls(config.pool_at(self.pool).is_some_and(|p| p.tls().is_some()))
        .body(self.parts.body);
        let mut list = FieldList::new();
        let mut target = Vec::new();
        request_fields(&fields, &facts, route, config, &mut list, &mut target)
            .map_err(Failure::new)?;
        let target = String::from_utf8(target)
            .map_err(|_| Failure::new(ErrorClass::Refused(RefusalKind::InvalidField)))?;
        let mut request = Request::new(&self.parts.method, &target);
        for (name, value) in list.iter() {
            request = request.header(name, value);
        }
        request = match self.parts.body {
            BodyFacts::Length(n) => request.content_length(n),
            BodyFacts::Chunked => request.chunked(),
            BodyFacts::None => request,
        };
        let Some(conn) = self.conn.as_mut() else {
            return Err(Failure::new(ErrorClass::UpstreamReset));
        };
        self.out.clear();
        let invalid = |_| Failure::new(ErrorClass::Refused(RefusalKind::InvalidField));
        conn.http
            .send_request(&request, &mut self.out)
            .map_err(invalid)?;
        let has_body = !matches!(self.parts.body, BodyFacts::None);
        if has_body && !self.buffered.is_empty() {
            conn.http
                .send_body(&self.buffered, &mut self.out)
                .map_err(invalid)?;
        }
        if has_body && self.body_complete {
            conn.http.end_body(&mut self.out).map_err(invalid)?;
        }
        conn.io
            .send(&self.out, self.deadline)
            .map_err(Failure::new)?;
        self.head_sent = true;
        self.bytes_up = self.buffered.len() as u64;
        if !self.body_complete {
            // The body goes on streaming; nothing more is held for a replay.
            self.buffered = Vec::new();
            return Ok(false);
        }
        Ok(true)
    }

    /// Reads until the response head, which it keeps in `self.head`.
    fn read_head(&mut self) -> Result<u16, ErrorClass> {
        let rt = Arc::clone(&self.rt);
        let Some(route) = rt.config.routes().get(self.route) else {
            return Err(ErrorClass::NoEndpoint);
        };
        let head_request = self.parts.method.as_bytes() == b"HEAD";
        let deadline = self.deadline;
        let Some(conn) = self.conn.as_mut() else {
            return Err(ErrorClass::UpstreamReset);
        };
        loop {
            match conn.http.poll() {
                Ok(ClientEvent::Response(head)) => {
                    let status = head.status();
                    self.status = Some(status);
                    let fields: Vec<(&str, &[u8])> = head.headers().collect();
                    let mut list = FieldList::new();
                    response_fields(&fields, route, &rt.config, &mut list)?;
                    list.remove("date");
                    self.head = Some(ResponseStart {
                        status,
                        fields: list,
                        has_body: !(head_request || status == 204 || status == 304),
                    });
                    return Ok(status);
                }
                Ok(ClientEvent::NeedInput) => {
                    let buf = conn.http.read_buf();
                    let n = conn
                        .io
                        .recv(buf, deadline, crate::core::TimeoutKind::ResponseHead)?;
                    if n == 0 {
                        conn.http.feed_eof();
                    } else {
                        conn.http.commit(n);
                    }
                }
                Ok(ClientEvent::Closed) => return Err(ErrorClass::UpstreamReset),
                Ok(_) => {}
                Err(_) => return Err(ErrorClass::UpstreamProtocol),
            }
        }
    }

    /// Appends up to `max` more response body bytes to `out`; `Ok(true)` when
    /// the response ended (its connection is back in the pool when it can
    /// be reused).
    ///
    /// # Errors
    ///
    /// The failure, with no status: the response was under way, so the
    /// client connection must close.
    pub fn read_body(&mut self, out: &mut Vec<u8>, max: usize) -> Result<bool, Failure> {
        if self.over {
            return Ok(true);
        }
        let limit = self.rt.config.limits().max_response_body_value();
        loop {
            let deadline = self.deadline;
            let Some(conn) = self.conn.as_mut() else {
                return Err(self.fail(Failure {
                    class: ErrorClass::UpstreamReset,
                    status: None,
                }));
            };
            match conn.http.poll() {
                Ok(ClientEvent::Body(bytes)) => {
                    out.extend_from_slice(bytes);
                    self.bytes_down += bytes.len() as u64;
                    if limit.is_some_and(|l| self.bytes_down > l) {
                        let a = self.event(Event::UpstreamFailed(ErrorClass::Limit(
                            LimitKind::ResponseBody,
                        )));
                        return Err(self.finish_failed(a));
                    }
                    let _ = self.event(Event::Progress);
                    if out.len() >= max {
                        return Ok(false);
                    }
                }
                Ok(ClientEvent::ResponseEnd) => {
                    let leftover = conn.http.remaining_input().len();
                    let mut reusable = conn.http.is_reusable() && leftover == 0;
                    if leftover > 0 {
                        reusable = false;
                        self.desync(leftover);
                    }
                    let action = self.event(Event::ResponseEnd { reusable });
                    self.finish(action);
                    return Ok(true);
                }
                Ok(ClientEvent::NeedInput) => {
                    if !out.is_empty() {
                        return Ok(false);
                    }
                    let buf = conn.http.read_buf();
                    match conn
                        .io
                        .recv(buf, deadline, crate::core::TimeoutKind::ReadIdle)
                    {
                        Ok(0) => conn.http.feed_eof(),
                        Ok(n) => conn.http.commit(n),
                        Err(c) => {
                            let a = match c {
                                ErrorClass::Timeout(_) => self.timed_out(),
                                c => self.event(Event::UpstreamFailed(c)),
                            };
                            return Err(self.finish_failed(a));
                        }
                    }
                }
                Ok(ClientEvent::Closed) => {
                    let a = self.event(Event::UpstreamFailed(ErrorClass::UpstreamReset));
                    return Err(self.finish_failed(a));
                }
                Ok(_) => {}
                Err(_) => {
                    let a = self.event(Event::UpstreamFailed(ErrorClass::UpstreamProtocol));
                    return Err(self.finish_failed(a));
                }
            }
        }
    }

    fn finish_failed(&mut self, action: Action) -> Failure {
        let (class, status) = match action {
            Action::Fail { class, status } => (class, status),
            _ => (ErrorClass::UpstreamProtocol, None),
        };
        self.record_failure(class);
        self.fail(Failure { class, status })
    }

    fn finish(&mut self, action: Action) {
        let proxied = self
            .rt
            .config
            .pool_at(self.pool)
            .is_some_and(|p| p.proxy_protocol().is_on());
        // A connection that carried a PROXY header speaks for one client only.
        let reuse = matches!(action, Action::Finish { reuse: true, .. }) && !proxied;
        self.over = true;
        self.rt.response_size(self.route, self.bytes_down);
        self.status = self.status.or(Some(200));
        Counters::add(&self.rt.counters.succeeded);
        if let (Some(mut conn), Some(id)) = (self.conn.take(), self.endpoint) {
            conn.exchanges = conn.exchanges.saturating_add(1);
            let max_idle = self
                .rt
                .config
                .pool_at(self.pool)
                .map_or(0, |p| p.limits().max_idle_value());
            if let Some(conns) = self.rt.conns(self.pool, id) {
                conns.release(conn, reuse, max_idle);
            }
        }
        self.unbalance();
    }

    fn desync(&mut self, leftover: usize) {
        Counters::add(&self.rt.counters.desyncs);
        if let (Some(id), Some(cfg)) = (self.endpoint, self.rt.config.pool_at(self.pool)) {
            if let Some(e) = cfg.endpoints().get(id.index()) {
                self.rt.observer.desync(e.addr(), leftover);
            }
        }
        self.record_failure(ErrorClass::Desync);
    }

    fn record_success(&mut self) {
        if let (Some(id), Some(p)) = (self.endpoint, self.rt.pools.get(self.pool)) {
            if let Some(t) = p.health.record(id, Outcome::Success, self.rt.tick()) {
                self.rt.changed(self.pool, t);
            }
        }
    }

    fn record_failure(&mut self, class: ErrorClass) {
        if let (Some(id), Some(p)) = (self.endpoint, self.rt.pools.get(self.pool)) {
            if let Some(t) = p.health.record(id, Outcome::from(class), self.rt.tick()) {
                self.rt.changed(self.pool, t);
            }
        }
        self.unbalance();
    }

    /// Ends the balancer's count for the current attempt.
    fn unbalance(&mut self) {
        if let (Some(id), Some(p)) = (self.balanced.take(), self.rt.pools.get(self.pool)) {
            let latency = self.sent_at.saturating_duration_since(self.started);
            p.balancer
                .finished(id, u64::try_from(latency.as_nanos()).unwrap_or(u64::MAX));
        }
    }

    /// Closes the current upstream connection without reusing it.
    fn drop_conn(&mut self) {
        if let (Some(conn), Some(id)) = (self.conn.take(), self.endpoint) {
            if let Some(conns) = self.rt.conns(self.pool, id) {
                conns.release(conn, false, 0);
            }
        }
        self.head_sent = false;
    }

    /// The client went away: the exchange ends, its connection is closed.
    pub fn abandon(&mut self) {
        if !self.over {
            self.over = true;
            self.failure = Some(ErrorClass::ClientGone);
            self.drop_conn();
            self.unbalance();
        }
    }
}

impl Drop for Forward {
    fn drop(&mut self) {
        self.abandon();
        let rt = Arc::clone(&self.rt);
        if let Some(p) = rt.pools.get(self.pool) {
            if self.retry_held {
                p.budget.retry_finished();
            }
            p.budget.exchange_finished();
        }
        if self.failure.is_some() {
            Counters::add(&rt.counters.failed);
        }
        let pool = rt.config.pool_at(self.pool);
        let endpoint = self
            .endpoint
            .and_then(|id| pool.and_then(|p| p.endpoints().get(id.index())))
            .map(|e| e.addr().clone());
        rt.observer.finished(&Finished {
            route: self.route,
            pool: pool.map(|p| Box::from(p.name())).unwrap_or_default(),
            endpoint,
            status: self.status,
            failure: self.failure,
            attempts: self.exchange.as_ref().map_or(0, Exchange::attempt),
            bytes_up: self.bytes_up,
            bytes_down: self.bytes_down,
            took: self.started.elapsed(),
        });
    }
}
