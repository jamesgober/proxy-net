//! Head rewriting: what an upstream request and a client response carry.
//!
//! The request side, in order: hop-by-hop fields removed (the fixed set and
//! every field `Connection` names), the forwarding set removed, `Host`,
//! `Content-Length`, and `Expect: 100-continue` removed (the proxy writes the
//! framing itself), the route's rules applied, then `Host`, the forwarding
//! fields, and `Via` added. Because the proxy's own fields are added last, a
//! `Connection` option cannot remove them.

use alloc::vec::Vec;
use core::net::{IpAddr, SocketAddr};

use parse_net::forwarded::{Element, Node, NodePort, Proto};

use super::class::{ErrorClass, RefusalKind};
use super::fields::{FieldList, FieldSource};
use super::route::{CompiledRoute, PathRewrite};
use crate::config::{ByNode, Config, ForwardedStyle};
use crate::endpoint::{Endpoint, EndpointAddr};
use crate::rewrite::{HeaderRule, HostHeader};

/// Hop-by-hop fields (RFC 9110 7.6.1) and the framing the proxy writes
/// itself.
const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-connection",
    "te",
    "transfer-encoding",
    "upgrade",
    "trailer",
    "proxy-authenticate",
    "proxy-authorization",
    "content-length",
];

/// The field of defence idea I-1; never accepted from a client.
pub const FRAME_FIELD: &str = "proxy-frame";

/// What the proxy knows about a request beyond its fields.
///
/// ```
/// use proxy_net::core::ForwardFacts;
/// let endpoint = proxy_net::Endpoint::parse("10.0.0.1:8080")?;
/// let facts = ForwardFacts::new(b"GET", b"/a?b", b"example.com", &endpoint)
///     .client("192.0.2.7:51000".parse().ok())
///     .https(true);
/// # let _ = facts;
/// # Ok::<(), proxy_net::ConfigError>(())
/// ```
#[derive(Clone, Copy, Debug)]
pub struct ForwardFacts<'a> {
    method: &'a [u8],
    target: &'a [u8],
    host: &'a [u8],
    endpoint: &'a Endpoint,
    client: Option<SocketAddr>,
    https: bool,
    listener: Option<SocketAddr>,
    upstream_tls: bool,
    body: BodyFacts,
    peer: Option<SocketAddr>,
    hops: u8,
    request_id: Option<&'a [u8]>,
}

/// The request body as the proxy will frame it upstream.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum BodyFacts {
    /// No body.
    #[default]
    None,
    /// A body of this length.
    Length(u64),
    /// A body of unknown length, sent chunked.
    Chunked,
}

impl<'a> ForwardFacts<'a> {
    /// The facts of a request with `method`, `target` (path and query as
    /// received), and `host` (the `Host` value), going to `endpoint`. The
    /// client is unknown, the scheme plain, the body empty, until set.
    pub fn new(
        method: &'a [u8],
        target: &'a [u8],
        host: &'a [u8],
        endpoint: &'a Endpoint,
    ) -> ForwardFacts<'a> {
        ForwardFacts {
            method,
            target,
            host,
            endpoint,
            client: None,
            https: false,
            listener: None,
            upstream_tls: false,
            body: BodyFacts::None,
            peer: None,
            hops: 0,
            request_id: None,
        }
    }

    /// The client address serve-net resolved (after any trusted proxy in
    /// front), or `None` when it is unknown.
    #[must_use]
    pub fn client(mut self, client: Option<SocketAddr>) -> ForwardFacts<'a> {
        self.client = client;
        self
    }

    /// The client reached the proxy over TLS (directly or through a trusted
    /// front end that said so).
    #[must_use]
    pub fn https(mut self, https: bool) -> ForwardFacts<'a> {
        self.https = https;
        self
    }

    /// The listener address, for `by=` with [`ByNode::Listener`].
    #[must_use]
    pub fn listener(mut self, listener: Option<SocketAddr>) -> ForwardFacts<'a> {
        self.listener = listener;
        self
    }

    /// The upstream connection uses TLS (decides the default port of `Host`
    /// for [`HostHeader::Endpoint`]).
    #[must_use]
    pub fn upstream_tls(mut self, tls: bool) -> ForwardFacts<'a> {
        self.upstream_tls = tls;
        self
    }

    /// How the body will be framed upstream.
    #[must_use]
    pub fn body(mut self, body: BodyFacts) -> ForwardFacts<'a> {
        self.body = body;
        self
    }

    /// The trusted chain in front of this proxy: the socket peer and how many
    /// proxies the inbound engine walked to find the client (0 for a direct
    /// client). Used by `Forwarded::append_trusted`.
    #[must_use]
    pub fn chain(mut self, peer: Option<SocketAddr>, hops: u8) -> ForwardFacts<'a> {
        self.peer = peer;
        self.hops = hops;
        self
    }

    /// The request id to write when the request has none (see
    /// `ProxyBuilder::request_id`).
    #[must_use]
    pub fn request_id(mut self, id: Option<&'a [u8]>) -> ForwardFacts<'a> {
        self.request_id = id;
        self
    }

    /// The request method.
    pub fn method(&self) -> &'a [u8] {
        self.method
    }

    /// The request target as received.
    pub fn target(&self) -> &'a [u8] {
        self.target
    }
}

/// True when the `Via` values name `pseudonym` as a received-by (RFC 9110
/// 7.6.3: `protocol received-by [comment]`, comma-separated, any case).
///
/// ```
/// use proxy_net::core::detect_loop;
/// assert!(detect_loop([&b"1.1 cdn, 1.1 Edge-1 (proxy-net)"[..]], "edge-1"));
/// assert!(!detect_loop([&b"1.1 edge-10"[..]], "edge-1"));
/// ```
pub fn detect_loop<'v, I>(via_values: I, pseudonym: &str) -> bool
where
    I: IntoIterator<Item = &'v [u8]>,
{
    via_values.into_iter().any(|value| {
        elements(value).any(|element| {
            let mut parts = element
                .split(|b| *b == b' ' || *b == b'\t')
                .filter(|p| !p.is_empty());
            let _protocol = parts.next();
            parts
                .next()
                .is_some_and(|by| by.eq_ignore_ascii_case(pseudonym.as_bytes()))
        })
    })
}

/// `core::fmt::Write` over a byte buffer: addresses and numbers are written
/// in place instead of through a `String`.
struct Bytes<'a>(&'a mut Vec<u8>);

impl core::fmt::Write for Bytes<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        self.0.extend_from_slice(s.as_bytes());
        Ok(())
    }
}

/// The elements of a comma-separated list value, through parse-net; a value
/// that is not a valid list is split at every comma (only used for matching,
/// never written).
fn elements(value: &[u8]) -> impl Iterator<Item = &[u8]> {
    let parsed: Vec<&[u8]> = match parse_net::list(value) {
        Ok(list) => list.iter().collect(),
        Err(_) => value
            .split(|b| *b == b',')
            .map(trim)
            .filter(|e| !e.is_empty())
            .collect(),
    };
    parsed.into_iter()
}

fn trim(s: &[u8]) -> &[u8] {
    let s = match s.iter().position(|b| *b != b' ' && *b != b'\t') {
        Some(i) => &s[i..],
        None => return &[],
    };
    match s.iter().rposition(|b| *b != b' ' && *b != b'\t') {
        Some(i) => &s[..=i],
        None => &[],
    }
}

/// The tokens every `Connection` field names, lowercase-compared later.
fn connection_options<F: FieldSource + ?Sized>(fields: &F) -> Result<Vec<&[u8]>, ErrorClass> {
    let mut out = Vec::new();
    for (name, value) in fields.fields() {
        if name.eq_ignore_ascii_case("connection") {
            let list = parse_net::list(value)
                .map_err(|_| ErrorClass::Refused(RefusalKind::InvalidField))?;
            out.extend(list.iter());
        }
    }
    Ok(out)
}

fn dropped(name: &str, options: &[&[u8]]) -> bool {
    HOP_BY_HOP.iter().any(|h| h.eq_ignore_ascii_case(name))
        || options
            .iter()
            .any(|o| o.eq_ignore_ascii_case(name.as_bytes()))
}

fn apply_rules(rules: &[HeaderRule], out: &mut FieldList) {
    for rule in rules {
        match rule {
            HeaderRule::Set(n, v) => out.set(n, v.as_bytes()),
            HeaderRule::Add(n, v) => out.push(n, v.as_bytes()),
            HeaderRule::Remove(n) => out.remove(n),
        }
    }
}

/// Writes the upstream request's target (the path rewritten by the route,
/// then the query unchanged) into `out`.
///
/// ```
/// use proxy_net::core::{rewrite_target, PathRewrite};
/// let mut out = Vec::new();
/// rewrite_target(b"/v1/users?id=7", &PathRewrite::Strip("/v1".into()), &mut out);
/// assert_eq!(out, b"/users?id=7");
/// rewrite_target(b"/v1", &PathRewrite::Strip("/v1".into()), &mut out);
/// assert_eq!(out, b"/");
/// rewrite_target(b"/old/a", &PathRewrite::Replace("/old".into(), "/new".into()), &mut out);
/// assert_eq!(out, b"/new/a");
/// ```
pub fn rewrite_target(target: &[u8], rewrite: &PathRewrite, out: &mut Vec<u8>) {
    out.clear();
    let (path, query) = match target.iter().position(|&b| b == b'?') {
        Some(q) => (&target[..q], &target[q..]),
        None => (target, &b""[..]),
    };
    let (prefix, replacement): (&[u8], &[u8]) = match rewrite {
        PathRewrite::Strip(p) => (p.as_bytes(), b""),
        PathRewrite::Replace(from, to) => (from.as_bytes(), to.as_bytes()),
        _ => (b"", b""),
    };
    match path.strip_prefix(prefix) {
        Some(rest) if !prefix.is_empty() => {
            let replacement = replacement.strip_suffix(b"/").unwrap_or(replacement);
            out.extend_from_slice(replacement);
            if !rest.starts_with(b"/") {
                out.push(b'/');
            }
            out.extend_from_slice(rest);
        }
        _ => out.extend_from_slice(path),
    }
    out.extend_from_slice(query);
}

/// The fields of the request sent upstream (see the module documentation
/// for the order), and its target in `target`.
///
/// ```
/// use proxy_net::core::{request_fields, FieldList, ForwardFacts};
/// use proxy_net::{Pool, ProxyBuilder};
///
/// let config = ProxyBuilder::new().pool("app", Pool::new(["10.0.0.1:80"])).config()?;
/// let route = config.routes().get(0).expect("route");
/// let endpoint = &config.pool("app").expect("pool").endpoints()[0];
/// let incoming = [
///     ("Host", &b"example.com"[..]),
///     ("X-Forwarded-For", b"127.0.0.1"),
///     ("Connection", b"keep-alive, x-secret"),
///     ("X-Secret", b"s"),
///     ("Accept", b"*/*"),
/// ];
/// let facts = ForwardFacts::new(b"GET", b"/", b"example.com", endpoint)
///     .client("192.0.2.7:51000".parse().ok());
/// let (mut out, mut target) = (FieldList::new(), Vec::new());
/// request_fields(&incoming, &facts, route, &config, &mut out, &mut target).expect("forward");
/// let names: Vec<&str> = out.iter().map(|(n, _)| n).collect();
/// assert_eq!(names, ["host", "accept", "forwarded", "x-forwarded-for", "x-forwarded-proto", "x-forwarded-host", "via"]);
/// assert_eq!(out.get("x-forwarded-for"), Some(&b"192.0.2.7"[..]));
/// assert_eq!(out.get("forwarded"), Some(&b"for=192.0.2.7;proto=http;host=example.com"[..]));
/// # Ok::<(), proxy_net::ConfigError>(())
/// ```
///
/// # Errors
///
/// `Loop` when `Via` names this proxy; `Refused(InvalidField)` when a
/// `Connection` value is not a list.
pub fn request_fields(
    fields: &[(&str, &[u8])],
    facts: &ForwardFacts<'_>,
    route: &CompiledRoute,
    config: &Config,
    out: &mut FieldList,
    target: &mut Vec<u8>,
) -> Result<(), ErrorClass> {
    request_fields_in(fields, facts, route, config, out, target)
}

/// [`request_fields`] over any field store, so a caller's list is read in
/// place.
pub(crate) fn request_fields_in<F: FieldSource + ?Sized>(
    fields: &F,
    facts: &ForwardFacts<'_>,
    route: &CompiledRoute,
    config: &Config,
    out: &mut FieldList,
    target: &mut Vec<u8>,
) -> Result<(), ErrorClass> {
    out.clear();
    if let Some(p) = config.via() {
        let via = fields
            .fields()
            .filter(|(n, _)| n.eq_ignore_ascii_case("via"))
            .map(|(_, v)| v);
        if detect_loop(via, p) {
            return Err(ErrorClass::Loop);
        }
    }
    let options = connection_options(fields)?;
    push_host(facts, route, out);
    for (name, value) in fields.fields() {
        let stripped = config
            .stripped_headers()
            .any(|s| s.eq_ignore_ascii_case(name));
        let expect_continue = name.eq_ignore_ascii_case("expect")
            && trim(value).eq_ignore_ascii_case(b"100-continue");
        if dropped(name, &options)
            || stripped
            || expect_continue
            || name.eq_ignore_ascii_case("host")
            || name.eq_ignore_ascii_case(FRAME_FIELD)
        {
            continue;
        }
        out.push(name, value);
    }
    apply_rules(route.request_rules(), out);
    if let (Some(name), Some(id)) = (config.request_id(), facts.request_id) {
        let kept = out.get(name).is_some_and(|v| {
            (1..=64).contains(&v.len()) && v.iter().all(|&b| crate::syntax::is_tchar(b))
        });
        if !kept {
            out.set(name, id);
        }
    }
    push_forwarding(fields, facts, config, out);
    if let Some(p) = config.via() {
        push_via(p, out);
    }
    if config.frame_facts() {
        push_frame_facts(facts, out);
    }
    rewrite_target(facts.target, route.rewrite(), target);
    Ok(())
}

fn push_host(facts: &ForwardFacts<'_>, route: &CompiledRoute, out: &mut FieldList) {
    use core::fmt::Write;
    match route.host_header() {
        HostHeader::Fixed(h) => out.push("host", h.as_bytes()),
        HostHeader::Endpoint => {
            let default = if facts.upstream_tls { 443 } else { 80 };
            let e = facts.endpoint;
            let port = e.addr().port();
            out.push_with("host", |v| {
                match (e.server_name_value(), e.addr()) {
                    (Some(name), _) => v.extend_from_slice(name.as_bytes()),
                    (None, EndpointAddr::Ip(a)) => match a.ip() {
                        IpAddr::V4(ip) => {
                            let _ = write!(Bytes(v), "{ip}");
                        }
                        IpAddr::V6(ip) => {
                            let _ = write!(Bytes(v), "[{ip}]");
                        }
                    },
                    (None, EndpointAddr::Name { host, .. }) => v.extend_from_slice(host.as_bytes()),
                }
                if port != default {
                    let _ = write!(Bytes(v), ":{port}");
                }
            });
        }
        _ => out.push("host", facts.host),
    }
}

/// `Via: 1.1 <pseudonym>`.
fn push_via(pseudonym: &str, out: &mut FieldList) {
    out.push_with("via", |v| {
        v.extend_from_slice(b"1.1 ");
        v.extend_from_slice(pseudonym.as_bytes());
    });
}

/// Writes one `Forwarded` element: `for`, `proto`, and `by` through
/// parse-net, then `host` here, quoted unless it is a token.
///
/// parse-net 0.7 writes a host name bare. A `reg-name` may be empty or hold
/// `,`, `;`, `=`, `(`, and `)`; written bare, the value does not parse, or a
/// client's `Host` such as `x,proto=https` ends the pair and forges an element
/// of its own. Nothing is left in `out` on `Err`, which is also returned for
/// a host holding a byte no quoted string may carry unescaped (no valid host
/// does).
fn write_element(e: &Element<'_>, out: &mut Vec<u8>) -> Result<(), ()> {
    let start = out.len();
    let rest = Element { host: None, ..*e };
    if parse_net::forwarded::write_element(&rest, out).is_err() {
        out.truncate(start);
        return Err(());
    }
    let Some(h) = &e.host else {
        return Ok(());
    };
    if out.len() > start {
        out.push(b';');
    }
    out.extend_from_slice(b"host=");
    let at = out.len();
    if h.write_to(out).is_err()
        || out[at..]
            .iter()
            .any(|&b| b == b'"' || b == b'\\' || !(0x21..0x7f).contains(&b))
    {
        out.truncate(start);
        return Err(());
    }
    if !crate::syntax::is_token(&out[at..]) {
        out.insert(at, b'"');
        out.push(b'"');
    }
    Ok(())
}

/// The elements of the incoming chain added by trusted proxies: the last
/// `hops` of `Forwarded` (re-serialized) and of `X-Forwarded-For`. `None`
/// when either does not parse (the chain is then replaced, as when it is not
/// trusted).
fn trusted_chain<F: FieldSource + ?Sized>(
    fields: &F,
    hops: u8,
) -> Option<(Vec<Vec<u8>>, Vec<IpAddr>)> {
    let lines = |name: &str| -> Vec<&[u8]> {
        fields
            .fields()
            .filter(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v)
            .collect()
    };
    let limits = parse_net::Limits::DEFAULT;
    let fwd = lines("forwarded");
    let mut elements = Vec::new();
    if !fwd.is_empty() {
        let parsed = parse_net::forwarded::parse(&fwd, &limits).ok()?;
        let all: Vec<_> = parsed.iter().collect();
        let kept = &all[all.len().saturating_sub(usize::from(hops))..];
        for e in leave_room(kept) {
            let mut v = Vec::new();
            write_element(&e, &mut v).ok()?;
            elements.push(v);
        }
    }
    let xff = lines("x-forwarded-for");
    let mut addrs = Vec::new();
    if !xff.is_empty() {
        let parsed = parse_net::forwarded::parse_x_forwarded_for(&xff, &limits).ok()?;
        let all: Vec<IpAddr> = parsed.iter().map(|(ip, _)| ip).collect();
        addrs.extend(leave_room(
            &all[all.len().saturating_sub(usize::from(hops))..],
        ));
    }
    Some((elements, addrs))
}

/// The most entries a forwarded chain may have and still parse with
/// parse-net's default limits (`Limits::DEFAULT`, 16 hops), which upstreams
/// built on it apply too.
const MAX_CHAIN: usize = 16;

/// The kept part of a trusted chain, with room for this hop: at most
/// `MAX_CHAIN - 1` entries. A longer chain keeps its first entry (the client
/// the outermost trusted proxy saw) and its most recent ones; the proxies in
/// between are dropped, so the header still parses upstream.
fn leave_room<T: Copy>(kept: &[T]) -> impl Iterator<Item = T> + '_ {
    let room = MAX_CHAIN - 1;
    let skip = kept.len().saturating_sub(room);
    let (first, rest) = match kept.split_first() {
        Some((f, r)) if skip > 0 => (Some(*f), &r[skip..]),
        _ => (None, kept),
    };
    first.into_iter().chain(rest.iter().copied())
}

fn push_forwarding<F: FieldSource + ?Sized>(
    fields: &F,
    facts: &ForwardFacts<'_>,
    config: &Config,
    out: &mut FieldList,
) {
    let forwarded = config.forwarded();
    let style = forwarded.style();
    let proto = if facts.https { "https" } else { "http" };
    // Append mode: keep what trusted proxies added and add this hop, whose
    // client is the socket peer.
    let chain = if forwarded.appends_trusted() && facts.hops > 0 {
        trusted_chain(fields, facts.hops)
    } else {
        None
    };
    let this_client = match (&chain, facts.peer) {
        (Some(_), Some(peer)) => Some(peer),
        _ => facts.client,
    };
    if matches!(style, ForwardedStyle::Both | ForwardedStyle::Standard) {
        let host = parse_net::parse_host_port(facts.host).ok();
        let by = match forwarded.by_node() {
            ByNode::Listener => facts
                .listener
                .map(|l| Node::Addr(l.ip(), Some(NodePort::Number(l.port())))),
            ByNode::Obfuscated(name) => Some(Node::Obfuscated(name.as_bytes(), None)),
            _ => None,
        };
        let element = Element {
            for_: Some(match this_client {
                Some(c) => Node::Addr(c.ip(), None),
                None => Node::Unknown(None),
            }),
            by,
            host,
            proto: Some(if facts.https {
                Proto::Https
            } else {
                Proto::Http
            }),
        };
        out.push_with("forwarded", |value| {
            if let Some((elements, _)) = &chain {
                let elements: &Vec<Vec<u8>> = elements;
                let mut first = true;
                // A chain the inbound engine trusted but no Forwarded line
                // came with (PROXY protocol, X-Forwarded-For only): start
                // from the client.
                if elements.is_empty() {
                    if let Some(c) = facts.client {
                        let e = Element {
                            for_: Some(Node::Addr(c.ip(), None)),
                            by: None,
                            host: None,
                            proto: None,
                        };
                        let _ = parse_net::forwarded::write_element(&e, value);
                        first = false;
                    }
                }
                for e in elements {
                    if !first {
                        value.extend_from_slice(b", ");
                    }
                    value.extend_from_slice(e);
                    first = false;
                }
                if !first {
                    value.extend_from_slice(b", ");
                }
            }
            let start = value.len();
            if write_element(&element, value).is_err() {
                value.truncate(start);
                // The host could not be written: write the element without
                // it.
                let element = Element {
                    host: None,
                    ..element
                };
                let _ = parse_net::forwarded::write_element(&element, value);
            }
        });
    }
    if matches!(style, ForwardedStyle::Both | ForwardedStyle::XForwarded) {
        use core::fmt::Write;
        match (&chain, facts.peer) {
            (Some((_, addrs)), Some(peer)) => {
                out.push_with("x-forwarded-for", |v| {
                    let mut w = Bytes(v);
                    for a in addrs {
                        let _ = write!(w, "{a}, ");
                    }
                    if addrs.is_empty() {
                        if let Some(c) = facts.client {
                            let _ = write!(w, "{}, ", c.ip());
                        }
                    }
                    let _ = write!(w, "{}", peer.ip());
                });
            }
            _ => {
                if let Some(c) = facts.client {
                    out.push_with("x-forwarded-for", |v| {
                        let _ = write!(Bytes(v), "{}", c.ip());
                    });
                }
            }
        }
        out.push("x-forwarded-proto", proto.as_bytes());
        if !facts.host.is_empty() {
            out.push("x-forwarded-host", facts.host);
        }
    }
}

fn push_frame_facts(facts: &ForwardFacts<'_>, out: &mut FieldList) {
    use core::fmt::Write;
    let fields = out.len();
    out.push_with(FRAME_FIELD, |v| {
        let mut w = Bytes(v);
        let _ = match facts.body {
            BodyFacts::None => write!(w, "none; fields={fields}"),
            BodyFacts::Length(n) => write!(w, "length={n}; fields={fields}"),
            BodyFacts::Chunked => write!(w, "chunked; fields={fields}"),
        };
    });
}

/// The fields of the response sent to the client: hop-by-hop fields and
/// framing removed (the inbound engine frames the body), the route's rules
/// applied, and `Via` added.
///
/// ```
/// use proxy_net::core::{response_fields, FieldList};
/// use proxy_net::{HeaderRule, Pool, ProxyBuilder, Route};
///
/// let config = ProxyBuilder::new()
///     .pool("app", Pool::new(["10.0.0.1:80"]))
///     .route(Route::any().to("app").response_header(HeaderRule::remove("server")))
///     .config()?;
/// let route = config.routes().get(0).expect("route");
/// let upstream = [
///     ("Content-Type", &b"text/plain"[..]),
///     ("Content-Length", b"5"),
///     ("Connection", b"close"),
///     ("Server", b"app/1.0"),
/// ];
/// let mut out = FieldList::new();
/// response_fields(&upstream, route, &config, &mut out).expect("forward");
/// let names: Vec<&str> = out.iter().map(|(n, _)| n).collect();
/// assert_eq!(names, ["content-type", "via"]);
/// # Ok::<(), proxy_net::ConfigError>(())
/// ```
///
/// # Errors
///
/// `UpstreamProtocol` when the upstream's `Connection` value is not a list.
pub fn response_fields(
    fields: &[(&str, &[u8])],
    route: &CompiledRoute,
    config: &Config,
    out: &mut FieldList,
) -> Result<(), ErrorClass> {
    response_fields_in(fields, route, config, out)
}

/// [`response_fields`] over any field store.
pub(crate) fn response_fields_in<F: FieldSource + ?Sized>(
    fields: &F,
    route: &CompiledRoute,
    config: &Config,
    out: &mut FieldList,
) -> Result<(), ErrorClass> {
    out.clear();
    let options = connection_options(fields).map_err(|_| ErrorClass::UpstreamProtocol)?;
    for (name, value) in fields.fields() {
        if !dropped(name, &options) {
            out.push(name, value);
        }
    }
    apply_rules(route.response_rules(), out);
    if let Some(p) = config.via() {
        push_via(p, out);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ByNode, Forwarded, HostHeader, Pool, ProxyBuilder, Route};

    fn config(b: ProxyBuilder) -> Config {
        b.config().expect("config")
    }

    fn one(route: Route) -> ProxyBuilder {
        ProxyBuilder::new()
            .pool("p", Pool::new(["10.0.0.1:8080"]))
            .route(route.to("p"))
    }

    fn run(c: &Config, fields: &[(&str, &[u8])], facts: ForwardFacts<'_>) -> (FieldList, Vec<u8>) {
        let route = c.routes().get(0).expect("route");
        let (mut out, mut target) = (FieldList::new(), Vec::new());
        request_fields(fields, &facts, route, c, &mut out, &mut target).expect("forward");
        (out, target)
    }

    #[test]
    fn test_forwarding_set_is_stripped_and_rebuilt() {
        // E-80, T-3.
        let c = config(one(Route::any()));
        let e = &c.pool("p").expect("p").endpoints()[0];
        let fields: Vec<(&str, &[u8])> = crate::FORWARDING_SET
            .iter()
            .map(|n| (*n, &b"spoofed"[..]))
            .chain([("X-FORWARDED-FOR", &b"1.2.3.4"[..]), ("Proxy-Frame", b"x")])
            .collect();
        let facts = ForwardFacts::new(b"GET", b"/", b"h", e).client("[2001:db8::1]:9".parse().ok());
        let (out, _) = run(&c, &fields, facts);
        assert!(out.iter().all(|(_, v)| v != b"spoofed" && v != b"1.2.3.4"));
        // E-82: IPv6 quoted and bracketed in Forwarded, bare in X-Forwarded-For.
        assert_eq!(
            out.get("forwarded"),
            Some(&b"for=\"[2001:db8::1]\";proto=http;host=h"[..])
        );
        assert_eq!(out.get("x-forwarded-for"), Some(&b"2001:db8::1"[..]));
        assert_eq!(out.get(FRAME_FIELD), None);
    }

    #[test]
    fn test_connection_cannot_remove_proxy_fields() {
        // E-81, T-6.
        let c = config(one(Route::any()));
        let e = &c.pool("p").expect("p").endpoints()[0];
        let fields = [
            (
                "Connection",
                &b"x-forwarded-for, via, forwarded, authorization"[..],
            ),
            ("Authorization", b"t"),
        ];
        let facts = ForwardFacts::new(b"GET", b"/", b"h", e).client("192.0.2.1:1".parse().ok());
        let (out, _) = run(&c, &fields, facts);
        assert_eq!(out.get("x-forwarded-for"), Some(&b"192.0.2.1"[..]));
        assert!(out.get("via").is_some());
        assert!(out.get("forwarded").is_some());
        // A field a client names in Connection is hop-by-hop, as RFC 9110 says.
        assert_eq!(out.get("authorization"), None);
        assert_eq!(out.get("connection"), None);
    }

    #[test]
    fn test_hop_by_hop_framing_and_expect_are_dropped() {
        let c = config(one(Route::any()));
        let e = &c.pool("p").expect("p").endpoints()[0];
        let fields = [
            ("Keep-Alive", &b"timeout=5"[..]),
            ("TE", b"trailers"),
            ("Transfer-Encoding", b"chunked"),
            ("Content-Length", b"5"),
            ("Upgrade", b"h2c"),
            ("HTTP2-Settings", b"AAMAAABkAAQAAP__"),
            ("Connection", b"Upgrade, HTTP2-Settings"),
            ("Proxy-Authorization", b"Basic x"),
            ("Expect", b"100-continue"),
            ("Trailer", b"x"),
            ("X-Keep", b"1"),
        ];
        let (out, _) = run(&c, &fields, ForwardFacts::new(b"POST", b"/", b"h", e));
        let names: Vec<&str> = out.iter().map(|(n, _)| n).collect();
        // T-10: the h2c upgrade and its settings are gone.
        assert_eq!(
            names,
            [
                "host",
                "x-keep",
                "forwarded",
                "x-forwarded-proto",
                "x-forwarded-host",
                "via"
            ]
        );
        // An unknown client: for=unknown, no X-Forwarded-For (E-84).
        assert_eq!(
            out.get("forwarded"),
            Some(&b"for=unknown;proto=http;host=h"[..])
        );
    }

    #[test]
    fn test_loop_detection() {
        // E-89, E-90, T-17.
        let c = config(one(Route::any()).via("edge-1"));
        let route = c.routes().get(0).expect("route");
        let e = &c.pool("p").expect("p").endpoints()[0];
        let (mut out, mut target) = (FieldList::new(), Vec::new());
        let facts = ForwardFacts::new(b"GET", b"/", b"h", e);
        for via in [
            &b"1.1 EDGE-1"[..],
            b"1.0 cdn, 1.1 edge-1 (x)",
            b"HTTP/1.1 edge-1",
        ] {
            assert_eq!(
                request_fields(&[("Via", via)], &facts, route, &c, &mut out, &mut target),
                Err(ErrorClass::Loop),
                "{via:?}"
            );
        }
        assert!(
            request_fields(
                &[("Via", &b"1.1 cdn"[..])],
                &facts,
                route,
                &c,
                &mut out,
                &mut target
            )
            .is_ok()
        );
        assert_eq!(
            out.get_all("via").collect::<Vec<_>>(),
            [&b"1.1 cdn"[..], b"1.1 edge-1"]
        );
        // No Via: no loop detection.
        let c = config(one(Route::any()).no_via());
        let route = c.routes().get(0).expect("route");
        assert!(
            request_fields(
                &[("Via", &b"1.1 proxy-net"[..])],
                &facts,
                route,
                &c,
                &mut out,
                &mut target
            )
            .is_ok()
        );
        assert_eq!(out.get_all("via").count(), 1);
        assert!(!detect_loop([&b"garbage,,(("[..]], "proxy-net"));
        assert!(detect_loop(
            [&b"1.1 a, (unclosed, 1.1 proxy-net"[..]],
            "proxy-net"
        ));
    }

    #[test]
    fn test_host_modes() {
        let c = config(
            ProxyBuilder::new()
                .pool(
                    "p",
                    Pool::new(["10.0.0.1:80", "app.internal:8443", "[::1]:80"]),
                )
                .route(
                    Route::any()
                        .path("/e")
                        .to("p")
                        .host_header(HostHeader::Endpoint),
                )
                .route(
                    Route::any()
                        .path("/f")
                        .to("p")
                        .host_header(HostHeader::fixed("fixed.example:81")),
                )
                .route(Route::any().to("p")),
        );
        let eps = c.pool("p").expect("p").endpoints();
        let host = |route: usize, ep: usize, tls: bool| {
            let route = c.routes().get(route).expect("route");
            let facts =
                ForwardFacts::new(b"GET", b"/", b"client.example", &eps[ep]).upstream_tls(tls);
            let (mut out, mut target) = (FieldList::new(), Vec::new());
            request_fields(&[], &facts, route, &c, &mut out, &mut target).expect("forward");
            out.get("host").map(<[u8]>::to_vec).unwrap_or_default()
        };
        assert_eq!(host(0, 0, false), b"10.0.0.1");
        assert_eq!(host(0, 1, false), b"app.internal:8443");
        assert_eq!(host(0, 1, true), b"app.internal:8443");
        assert_eq!(host(0, 2, false), b"[::1]");
        assert_eq!(host(0, 0, true), b"10.0.0.1:80");
        assert_eq!(host(1, 0, false), b"fixed.example:81");
        assert_eq!(host(2, 0, false), b"client.example");
    }

    #[test]
    fn test_rules_and_rewrites() {
        let c = config(one(Route::any()
            .path("/v1")
            .strip_prefix("/v1")
            .request_header(HeaderRule::set("x-env", "prod"))
            .request_header(HeaderRule::remove("cookie"))
            .request_header(HeaderRule::add("x-env", "two"))));
        let e = &c.pool("p").expect("p").endpoints()[0];
        let fields = [("X-Env", &b"client"[..]), ("Cookie", b"a=b")];
        let (out, target) = run(
            &c,
            &fields,
            ForwardFacts::new(b"GET", b"/v1/x?q=1", b"h", e),
        );
        assert_eq!(target, b"/x?q=1");
        assert_eq!(
            out.get_all("x-env").collect::<Vec<_>>(),
            [&b"prod"[..], b"two"]
        );
        assert_eq!(out.get("cookie"), None);
        let mut t = Vec::new();
        rewrite_target(b"/v1/", &PathRewrite::Strip("/v1/".into()), &mut t);
        assert_eq!(t, b"/");
        rewrite_target(
            b"/a",
            &PathRewrite::Replace("/a".into(), "/b/".into()),
            &mut t,
        );
        assert_eq!(t, b"/b/");
        rewrite_target(b"/x", &PathRewrite::None, &mut t);
        assert_eq!(t, b"/x");
        rewrite_target(b"/other", &PathRewrite::Strip("/v1".into()), &mut t);
        assert_eq!(t, b"/other");
    }

    #[test]
    fn test_forwarded_styles_and_by() {
        let e = crate::Endpoint::parse("10.0.0.1:80").expect("e");
        let facts = ForwardFacts::new(b"GET", b"/", b"example.com:8443", &e)
            .client("192.0.2.9:4000".parse().ok())
            .https(true)
            .listener("198.51.100.1:443".parse().ok());
        let c = config(one(Route::any()).forwarded(Forwarded::standard().by(ByNode::Listener)));
        let (out, _) = run(&c, &[], facts);
        assert_eq!(
            out.get("forwarded"),
            Some(
                &b"for=192.0.2.9;proto=https;by=\"198.51.100.1:443\";host=\"example.com:8443\""[..]
            )
        );
        assert_eq!(out.get("x-forwarded-for"), None);
        let c = config(one(Route::any()).forwarded(Forwarded::x_forwarded()));
        let (out, _) = run(&c, &[], facts);
        assert_eq!(out.get("forwarded"), None);
        assert_eq!(out.get("x-forwarded-proto"), Some(&b"https"[..]));
        assert_eq!(out.get("x-forwarded-host"), Some(&b"example.com:8443"[..]));
        let c = config(one(Route::any()).forwarded(Forwarded::off()));
        let (out, _) = run(&c, &[("X-Forwarded-For", &b"1.1.1.1"[..])], facts);
        assert_eq!(
            out.iter().map(|(n, _)| n).collect::<Vec<_>>(),
            ["host", "via"]
        );
        let c = config(
            one(Route::any()).forwarded(Forwarded::standard().by(ByNode::obfuscated("_edge"))),
        );
        let (out, _) = run(&c, &[], facts);
        assert_eq!(
            out.get("forwarded"),
            Some(&b"for=192.0.2.9;proto=https;by=_edge;host=\"example.com:8443\""[..])
        );
        // A host that is not a valid host: the element is written without it.
        let bad = ForwardFacts::new(b"GET", b"/", b"a b", &e);
        let c = config(one(Route::any()).forwarded(Forwarded::standard()));
        let (out, _) = run(&c, &[], bad);
        assert_eq!(out.get("forwarded"), Some(&b"for=unknown;proto=http"[..]));
    }

    #[test]
    fn test_forwarded_host_cannot_forge_an_element() {
        // A reg-name may hold `,`, `;`, and `=`: written bare, the client's
        // Host would end the pair and add an element of its own, such as one
        // saying the request came over HTTPS.
        let c = config(one(Route::any()).forwarded(Forwarded::standard()));
        let e = &c.pool("p").expect("p").endpoints()[0];
        let limits = parse_net::Limits::DEFAULT;
        let cases: [(&[u8], &[u8]); 6] = [
            (
                b"x,proto=https",
                b"for=192.0.2.9;proto=http;host=\"x,proto=https\"",
            ),
            (
                b"x,for=_spoofed",
                b"for=192.0.2.9;proto=http;host=\"x,for=_spoofed\"",
            ),
            (
                b"example.com:",
                b"for=192.0.2.9;proto=http;host=example.com",
            ),
            (b"", b"for=192.0.2.9;proto=http;host=\"\""),
            (b"example.com", b"for=192.0.2.9;proto=http;host=example.com"),
            (b"192.0.2.1", b"for=192.0.2.9;proto=http;host=192.0.2.1"),
        ];
        for (host, want) in cases {
            let facts = ForwardFacts::new(b"GET", b"/", host, e).client("192.0.2.9:1".parse().ok());
            let (out, _) = run(&c, &[], facts);
            let fwd = out.get("forwarded").expect("forwarded");
            assert_eq!(fwd, want, "{:?}", core::str::from_utf8(fwd));
            let lines = [fwd];
            let parsed = parse_net::forwarded::parse(&lines, &limits).expect("parses");
            assert_eq!(parsed.iter().count(), 1, "{:?}", core::str::from_utf8(fwd));
        }
        // The same for a trusted proxy's element, re-serialized in append mode.
        let c = config(one(Route::any()).forwarded(Forwarded::standard().append_trusted()));
        let e = &c.pool("p").expect("p").endpoints()[0];
        for (incoming, kept) in [
            (
                &b"for=1.1.1.1;host=\"a,proto=https\""[..],
                &b"for=1.1.1.1;host=\"a,proto=https\""[..],
            ),
            (b"Host=\"\"", b"host=\"\""),
        ] {
            let facts = ForwardFacts::new(b"GET", b"/", b"h", e)
                .client("192.0.2.9:1".parse().ok())
                .chain("10.1.1.1:2".parse().ok(), 1);
            let (out, _) = run(&c, &[("Forwarded", incoming)], facts);
            let fwd = out.get("forwarded").expect("forwarded");
            let mut want = kept.to_vec();
            want.extend_from_slice(b", for=10.1.1.1;proto=http;host=h");
            assert_eq!(fwd, &want[..], "{:?}", core::str::from_utf8(fwd));
            let lines = [fwd];
            let parsed = parse_net::forwarded::parse(&lines, &limits).expect("parses");
            assert_eq!(parsed.iter().count(), 2);
        }
    }

    #[test]
    fn test_a_full_trusted_chain_still_parses_upstream() {
        use alloc::format;
        use alloc::string::{String, ToString};
        // A chain already at parse-net's limit of 16: keeping all of it and
        // adding this hop would make 17, which the same parser refuses.
        let c = config(one(Route::any()).forwarded(Forwarded::both().append_trusted()));
        let e = &c.pool("p").expect("p").endpoints()[0];
        let fwd: Vec<String> = (1..=16).map(|i| format!("for=10.0.0.{i}")).collect();
        let fwd = fwd.join(", ");
        let xff: Vec<String> = (1..=16).map(|i| format!("10.0.0.{i}")).collect();
        let xff = xff.join(", ");
        let facts = ForwardFacts::new(b"GET", b"/", b"h", e)
            .client("192.0.2.9:1".parse().ok())
            .chain("10.9.9.9:2".parse().ok(), 255);
        let (out, _) = run(
            &c,
            &[
                ("Forwarded", fwd.as_bytes()),
                ("X-Forwarded-For", xff.as_bytes()),
            ],
            facts,
        );
        let limits = parse_net::Limits::DEFAULT;
        let lines = [out.get("forwarded").expect("forwarded")];
        let parsed = parse_net::forwarded::parse(&lines, &limits).expect("parses");
        let all: Vec<_> = parsed.iter().collect();
        assert_eq!(all.len(), 16);
        let ip = |e: &Element<'_>| match e.for_ {
            Some(Node::Addr(ip, _)) => ip.to_string(),
            other => format!("{other:?}"),
        };
        // The first (the client) and the most recent hops are kept.
        assert_eq!(ip(&all[0]), "10.0.0.1");
        assert_eq!(ip(&all[1]), "10.0.0.3");
        assert_eq!(ip(&all[14]), "10.0.0.16");
        assert_eq!(ip(&all[15]), "10.9.9.9");
        let xff_lines = [out.get("x-forwarded-for").expect("xff")];
        let parsed =
            parse_net::forwarded::parse_x_forwarded_for(&xff_lines, &limits).expect("parses");
        let addrs: Vec<String> = parsed.iter().map(|(ip, _)| ip.to_string()).collect();
        assert_eq!(addrs.len(), 16);
        assert_eq!(
            (addrs[0].as_str(), addrs[15].as_str()),
            ("10.0.0.1", "10.9.9.9")
        );
        // A chain with room is kept whole.
        let short = "for=10.0.0.1, for=10.0.0.2";
        let facts = ForwardFacts::new(b"GET", b"/", b"h", e)
            .client("192.0.2.9:1".parse().ok())
            .chain("10.9.9.9:2".parse().ok(), 255);
        let (out, _) = run(&c, &[("Forwarded", short.as_bytes())], facts);
        assert_eq!(
            out.get("forwarded"),
            Some(&b"for=10.0.0.1, for=10.0.0.2, for=10.9.9.9;proto=http;host=h"[..])
        );
    }

    #[test]
    fn test_frame_facts() {
        let c = config(one(Route::any()).frame_facts(true));
        let e = &c.pool("p").expect("p").endpoints()[0];
        let (out, _) = run(
            &c,
            &[],
            ForwardFacts::new(b"POST", b"/", b"h", e).body(BodyFacts::Length(12)),
        );
        assert_eq!(out.get(FRAME_FIELD), Some(&b"length=12; fields=5"[..]));
        let (out, _) = run(
            &c,
            &[],
            ForwardFacts::new(b"POST", b"/", b"h", e).body(BodyFacts::Chunked),
        );
        assert_eq!(out.get(FRAME_FIELD), Some(&b"chunked; fields=5"[..]));
        let (out, _) = run(&c, &[], ForwardFacts::new(b"GET", b"/", b"h", e));
        assert_eq!(out.get(FRAME_FIELD), Some(&b"none; fields=5"[..]));
    }

    #[test]
    fn test_invalid_connection_is_refused() {
        let c = config(one(Route::any()));
        let route = c.routes().get(0).expect("route");
        let e = &c.pool("p").expect("p").endpoints()[0];
        let (mut out, mut target) = (FieldList::new(), Vec::new());
        let facts = ForwardFacts::new(b"GET", b"/", b"h", e);
        assert_eq!(
            request_fields(
                &[("Connection", &b"a\x01b"[..])],
                &facts,
                route,
                &c,
                &mut out,
                &mut target
            ),
            Err(ErrorClass::Refused(RefusalKind::InvalidField))
        );
        assert_eq!(
            response_fields(&[("Connection", &b"\"open"[..])], route, &c, &mut out),
            Err(ErrorClass::UpstreamProtocol)
        );
    }

    #[test]
    fn test_response_fields() {
        let c = config(one(
            Route::any().response_header(HeaderRule::add("x-proxy", "1"))
        ));
        let route = c.routes().get(0).expect("route");
        let upstream = [
            ("Transfer-Encoding", &b"chunked"[..]),
            ("Connection", b"x-internal"),
            ("X-Internal", b"secret"),
            ("Keep-Alive", b"timeout=5"),
            ("Set-Cookie", b"a=1"),
            ("Set-Cookie", b"b=2"),
        ];
        let mut out = FieldList::new();
        response_fields(&upstream, route, &c, &mut out).expect("forward");
        let names: Vec<&str> = out.iter().map(|(n, _)| n).collect();
        assert_eq!(names, ["set-cookie", "set-cookie", "x-proxy", "via"]);
        assert_eq!(trim(b"  a b \t"), b"a b");
        assert_eq!(trim(b"   "), b"");
    }
}
